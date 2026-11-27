"use client";

import type { PluginConfigController, PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import React, {
  Component,
  type ComponentType,
  type ErrorInfo,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { useNavigate } from "react-router-dom";

import { Modal } from "@/components/modal";
import {
  fetchPluginUiContract,
  fetchPluginUiDescriptor,
  type PluginUiContract,
  type UiDescriptorView,
  type UiSurfaceKind,
} from "@/lib/api/plugins";
import {
  evictPluginFederationCache,
  loadFederatedSurface,
} from "@/lib/plugins/federation-loader";
import { createPluginHostSdk, type ManagedPluginHostSdk } from "@/lib/plugins/sdk";
import { useSession } from "@/lib/session";

export interface PluginSurfaceHostDeps {
  fetchDescriptor?: (pluginId: string, signal?: AbortSignal) => Promise<UiDescriptorView>;
  fetchContract?: (signal?: AbortSignal) => Promise<PluginUiContract>;
  loadSurface?: (
    descriptor: UiDescriptorView,
    expose: string,
  ) => Promise<ComponentType<PluginSurfaceProps>>;
  theme?: string;
  notifyToast?: (input: { message: string; level?: "info" | "warning" | "error" }) => void;
  openConfirm?: (input: { title: string; message: string; confirmLabel: string }) => Promise<boolean>;
}

export interface PluginSurfaceFallbackAction {
  label: string;
  onSelect: () => void;
}

export interface PluginSurfaceHostProps {
  pluginId: string;
  surfaceId: string;
  expectedKind?: UiSurfaceKind;
  config?: PluginConfigController;
  reloadKey?: string | number;
  className?: string;
  onTitle?: (title: string) => void;
  onDescriptorLoaded?: (descriptor: UiDescriptorView) => void;
  onRegisterActions?: (actions: Array<{ id: string; label: string; onSelect: () => void }>) => void;
  onRegisterTabs?: (tabs: readonly { id: string; label: string; onSelect: () => void }[]) => void;
  fallbackAction?: PluginSurfaceFallbackAction;
  deps?: PluginSurfaceHostDeps;
}

class PluginErrorBoundary extends Component<
  { children: ReactNode; resetKey: string; fallbackAction?: PluginSurfaceFallbackAction },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidUpdate(prevProps: { resetKey: string }) {
    if (prevProps.resetKey !== this.props.resetKey && this.state.error !== null) {
      this.setState({ error: null });
    }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Plugin surface render error:", error, info);
  }

  render() {
    if (this.state.error) {
      return (
        <div
          role="alert"
          className="space-y-3 rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 p-4 text-sub text-[#ff6b6b]"
        >
          <div>
            <p className="font-semibold">插件界面渲染异常</p>
            <p className="mt-1 opacity-90">{this.state.error.message}</p>
          </div>
          {this.props.fallbackAction && (
            <button
              type="button"
              onClick={this.props.fallbackAction.onSelect}
              className="btn-glass px-3.5 py-1.5 text-caption font-medium text-[var(--text)]"
            >
              {this.props.fallbackAction.label}
            </button>
          )}
        </div>
      );
    }
    return this.props.children;
  }
}

type GateFailureReason =
  | "trust-required"
  | "subject-mismatch"
  | "contract-mismatch"
  | "missing-assets"
  | "missing-surface"
  | "disabled";

type GateResult =
  | { ok: true; surface: UiDescriptorView["surfaces"][number] }
  | { ok: false; reason: GateFailureReason; title: string; detail: string };

function evaluateSurfaceGate(
  descriptor: UiDescriptorView,
  contract: PluginUiContract,
  sessionUserId: string,
  surfaceId: string,
  expectedKind?: UiSurfaceKind,
): GateResult {
  if (descriptor.subject_user_id !== sessionUserId) {
    return {
      ok: false,
      reason: "subject-mismatch",
      title: "会话身份不匹配",
      detail: "Cookie 会话主体与当前登录用户不一致，已阻止挂载插件视图。请刷新页面重新登录。",
    };
  }
  if (descriptor.ui_trusted !== true) {
    return {
      ok: false,
      reason: "trust-required",
      title: "插件 UI 尚未信任",
      detail: "当前版本 UI 资产未获管理员明确信任，已按安全策略拒绝加载远程脚本。",
    };
  }
  if (
    descriptor.contract !== contract.contract ||
    descriptor.react !== contract.react ||
    descriptor.react_dom !== contract.react_dom
  ) {
    return {
      ok: false,
      reason: "contract-mismatch",
      title: "UI 契约版本不兼容",
      detail: `插件要求 contract=${descriptor.contract} / React ${descriptor.react}，宿主提供 contract=${contract.contract} / React ${contract.react}。`,
    };
  }
  if (descriptor.ui_assets_present === false || !descriptor.entry_url) {
    return {
      ok: false,
      reason: "missing-assets",
      title: "插件 UI 资产不可用",
      detail: "当前插件未包含可挂载的前端资产。",
    };
  }
  const surface = descriptor.surfaces.find((s) => s.id === surfaceId);
  if (!surface || (expectedKind !== undefined && surface.kind !== expectedKind)) {
    return {
      ok: false,
      reason: "missing-surface",
      title: "未找到目标界面入口",
      detail: `插件 ${descriptor.plugin_id} 未声明 surface "${surfaceId}"。`,
    };
  }
  if (!surface.mountable) {
    return {
      ok: false,
      reason: "disabled",
      title: "插件未启用，界面不可挂载",
      detail: "请先启用插件后再访问运行态界面；禁用状态下仅可编辑配置。",
    };
  }
  return { ok: true, surface };
}

type SurfaceLoadState =
  | { status: "loading" }
  | { status: "blocked"; title: string; detail: string }
  | {
      status: "ready";
      descriptor: UiDescriptorView;
      surface: UiDescriptorView["surfaces"][number];
      Component: ComponentType<PluginSurfaceProps>;
      managedSdk: ManagedPluginHostSdk;
    };

function usePluginSurfaceLoader(
  props: PluginSurfaceHostProps,
  activeTheme: string,
  setFallbackToast: (msg: string | null) => void,
  setDialogComp: React.Dispatch<React.SetStateAction<ComponentType<{ close: () => void }> | null>>,
): SurfaceLoadState {
  const { pluginId, surfaceId, expectedKind, reloadKey, deps } = props;
  const { session, loginEpoch = `${session.id}:active` } = useSession();
  const navigate = useNavigate();
  const [state, setState] = useState<SurfaceLoadState>({ status: "loading" });

  const runtimeRef = useRef({
    session,
    activeTheme,
    navigate,
    fetchDescriptor: deps?.fetchDescriptor ?? fetchPluginUiDescriptor,
    fetchContract: deps?.fetchContract ?? fetchPluginUiContract,
    loadSurface: deps?.loadSurface ?? loadFederatedSurface,
    notifyToast:
      deps?.notifyToast ??
      ((input: { message: string }) => {
        setFallbackToast(input.message);
      }),
    openConfirm:
      deps?.openConfirm ??
      (async (input: { title: string; message: string }) =>
        typeof window !== "undefined" ? window.confirm(`${input.title}\n\n${input.message}`) : false),
    onTitle: props.onTitle,
    onDescriptorLoaded: props.onDescriptorLoaded,
    onRegisterActions: props.onRegisterActions,
    onRegisterTabs: props.onRegisterTabs,
  });
  runtimeRef.current = {
    session,
    activeTheme,
    navigate,
    fetchDescriptor: deps?.fetchDescriptor ?? fetchPluginUiDescriptor,
    fetchContract: deps?.fetchContract ?? fetchPluginUiContract,
    loadSurface: deps?.loadSurface ?? loadFederatedSurface,
    notifyToast:
      deps?.notifyToast ??
      ((input: { message: string }) => {
        setFallbackToast(input.message);
      }),
    openConfirm:
      deps?.openConfirm ??
      (async (input: { title: string; message: string }) =>
        typeof window !== "undefined" ? window.confirm(`${input.title}\n\n${input.message}`) : false),
    onTitle: props.onTitle,
    onDescriptorLoaded: props.onDescriptorLoaded,
    onRegisterActions: props.onRegisterActions,
    onRegisterTabs: props.onRegisterTabs,
  };

  useEffect(() => {
    let cancelled = false;
    let activeSdk: ManagedPluginHostSdk | null = null;
    const ac = new AbortController();

    async function load() {
      setState({ status: "loading" });
      let gatePassed = false;
      try {
        const [descriptor, contract] = await Promise.all([
          runtimeRef.current.fetchDescriptor(pluginId, ac.signal),
          runtimeRef.current.fetchContract(ac.signal),
        ]);
        if (cancelled) return;
        runtimeRef.current.onDescriptorLoaded?.(descriptor);

        const gate = evaluateSurfaceGate(
          descriptor,
          contract,
          runtimeRef.current.session.id,
          surfaceId,
          expectedKind,
        );
        if (!gate.ok) {
          const keepOtherSurfaces =
            (gate.reason === "disabled" || gate.reason === "missing-surface") &&
            descriptor.ui_assets_present !== false &&
            Boolean(descriptor.entry_url) &&
            descriptor.surfaces.some((s) => s.mountable);
          if (!keepOtherSurfaces) {
            evictPluginFederationCache(pluginId);
          }
          setState({ status: "blocked", title: gate.title, detail: gate.detail });
          return;
        }
        gatePassed = true;
        runtimeRef.current.onTitle?.(gate.surface.title);

        const managed = createPluginHostSdk({
          descriptor,
          surfaceId: gate.surface.id,
          session: runtimeRef.current.session,
          loginEpoch,
          theme: runtimeRef.current.activeTheme,
          navigate: (to) => runtimeRef.current.navigate(to),
          notifyToast: (msg) => runtimeRef.current.notifyToast(msg),
          openConfirm: (c) => runtimeRef.current.openConfirm(c),
          openDialog: (comp) => {
            setDialogComp(() => comp);
            return () =>
              setDialogComp((cur: ComponentType<{ close: () => void }> | null) =>
                cur === comp ? null : cur,
              );
          },
          onRegisterActions: (actions) => runtimeRef.current.onRegisterActions?.(actions),
          onRegisterTabs: (tabs) => runtimeRef.current.onRegisterTabs?.(tabs),
        });
        activeSdk = managed;

        const RemoteComponent = await runtimeRef.current.loadSurface(descriptor, gate.surface.expose);
        if (cancelled) {
          managed.dispose();
          activeSdk = null;
          return;
        }
        setState({
          status: "ready",
          descriptor,
          surface: gate.surface,
          Component: RemoteComponent,
          managedSdk: managed,
        });
      } catch (err) {
        activeSdk?.dispose();
        activeSdk = null;
        const isAbort =
          (err instanceof DOMException && err.name === "AbortError") ||
          (err instanceof Error && err.name === "AbortError");
        if (cancelled || isAbort) return;
        if (!gatePassed) {
          evictPluginFederationCache(pluginId);
        }
        setState({
          status: "blocked",
          title: "无法加载插件界面",
          detail: (err as Error).message || "预检或远程模块加载失败",
        });
      }
    }

    void load();
    return () => {
      cancelled = true;
      ac.abort();
      activeSdk?.dispose();
      setDialogComp(null);
      runtimeRef.current.onRegisterActions?.([]);
      runtimeRef.current.onRegisterTabs?.([]);
    };
  }, [
    pluginId,
    surfaceId,
    expectedKind,
    reloadKey,
    session.id,
    session.role,
    loginEpoch,
    setDialogComp,
  ]);

  useEffect(() => {
    if (state.status === "ready") {
      state.managedSdk.notifyEnvironmentChange({ theme: activeTheme });
    }
  }, [activeTheme, state]);

  return state;
}

export function PluginSurfaceHost(props: PluginSurfaceHostProps) {
  const { config, className, fallbackAction, deps } = props;
  const [dialogComp, setDialogComp] = useState<ComponentType<{ close: () => void }> | null>(null);
  const [fallbackToast, setFallbackToast] = useState<string | null>(null);
  const activeTheme =
    deps?.theme ??
    (typeof document !== "undefined" ? document.documentElement.dataset.theme || "silver" : "silver");

  const state = usePluginSurfaceLoader(props, activeTheme, setFallbackToast, setDialogComp);

  if (state.status === "loading") {
    return (
      <div
        className={`rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)] ${className ?? ""}`}
      >
        正在校验插件 UI 信任与契约…
      </div>
    );
  }

  if (state.status === "blocked") {
    return (
      <div
        role="alert"
        className={`space-y-3 rounded-xl border border-amber-400/25 bg-amber-400/10 p-5 text-sub ${className ?? ""}`}
      >
        <div>
          <p className="font-semibold text-[var(--text)]">{state.title}</p>
          <p className="mt-1 leading-6 text-[var(--text-muted)]">{state.detail}</p>
        </div>
        {fallbackAction && (
          <button
            type="button"
            onClick={fallbackAction.onSelect}
            className="btn-glass px-3.5 py-1.5 text-caption font-medium text-[var(--text)]"
          >
            {fallbackAction.label}
          </button>
        )}
      </div>
    );
  }

  const mountKey = `${state.descriptor.plugin_id}:${state.descriptor.generation}:${state.descriptor.bundle_sha256}:${state.descriptor.subject_user_id}:${state.surface.id}`;
  const Remote = state.Component;
  const DialogBody = dialogComp;

  return (
    <div className={className} data-plugin-surface={mountKey}>
      {fallbackToast && (
        <div className="mb-3 rounded-lg border border-white/15 bg-white/[0.06] px-3 py-2 text-caption text-[var(--text)]">
          {fallbackToast}
        </div>
      )}
      <PluginErrorBoundary resetKey={mountKey} fallbackAction={fallbackAction}>
        <Remote
          key={mountKey}
          host={state.managedSdk.sdk}
          surfaceId={state.surface.id}
          config={config}
        />
      </PluginErrorBoundary>
      {DialogBody && (
        <Modal open onClose={() => setDialogComp(null)} label={state.surface.title} width="lg">
          <div className="p-5">
            <DialogBody close={() => setDialogComp(null)} />
          </div>
        </Modal>
      )}
    </div>
  );
}
