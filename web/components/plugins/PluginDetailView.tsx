"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { PluginConfigHost } from "@/components/plugins/PluginConfigHost";
import {
  PluginAuditTab,
  PluginOverviewSection,
  PluginPermissionsTab,
  PluginTasksTab,
  TrustGrantModal,
} from "@/components/plugins/PluginDetailTabs";
import { PluginSurfaceHost } from "@/components/plugins/PluginSurfaceHost";
import { usePluginDetailActions } from "@/components/plugins/usePluginDetailActions";
import {
  type AuditEventView,
  fetchPluginUiDescriptor,
  getPluginDetail,
  getPluginPermissions,
  isPluginConfigEditable,
  listPluginAudit,
  listPluginEvents,
  listPluginTasks,
  type PluginDetailView as PluginDetailData,
  type PluginEventDeliveryView,
  type PluginPermissionsView,
  type PluginTaskView,
  type TaskReceiptView,
  type UiDescriptorView,
  type UiTrustPreparationView,
} from "@/lib/api/plugins";
import { HttpError } from "@/lib/http";
import { evictPluginFederationCache } from "@/lib/plugins/federation-loader";
import { useOptionalSession } from "@/lib/session";
import { useTheme } from "@/lib/ui-prefs";

type DetailTab = "overview" | "config" | "page" | "permissions" | "tasks" | "audit";

function isUnauthorizedError(err: unknown): boolean {
  if (err instanceof HttpError && err.status === 401) return true;
  return Boolean(err && typeof err === "object" && (err as { status?: unknown }).status === 401);
}

function PluginDetailHeader({
  detail,
  appSurfaces,
  busyAction,
  onRunLifecycle,
}: {
  detail: PluginDetailData;
  appSurfaces: UiDescriptorView["surfaces"];
  busyAction: string | null;
  onRunLifecycle: (action: "enable" | "disable" | "uninstall") => void;
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-4">
      <div className="space-y-1">
        <div className="flex items-center gap-2 text-caption text-[var(--text-faint)]">
          <Link to="/settings/plugins" className="hover:text-[var(--text)]">
            设置 / 插件
          </Link>
          <span>/</span>
          <span>{detail.id}</span>
        </div>
        <h1 className="text-[22px] font-semibold text-[var(--text)]">{detail.name}</h1>
        <p className="text-caption text-[var(--text-muted)]">
          v{detail.version} · {detail.source} · {detail.lifecycle} · generation #{detail.generation} · rev #
          {detail.revision}
        </p>
      </div>

      <div className="flex flex-wrap items-center gap-2">
        {appSurfaces.map((app) => (
          <Link
            key={app.id}
            to={`/plugin-apps/${encodeURIComponent(detail.id)}/${encodeURIComponent(app.id)}`}
            className="btn-glass px-3.5 py-1.5 text-sub font-medium"
          >
            打开 {app.title}
          </Link>
        ))}
        <button
          type="button"
          disabled={busyAction !== null}
          onClick={() => onRunLifecycle(detail.enabled ? "disable" : "enable")}
          className="btn-accent rounded-full px-4 py-1.5 text-sub font-semibold disabled:opacity-50"
        >
          {detail.enabled ? "停用插件" : "启用插件"}
        </button>
        {detail.source !== "builtin" && (
          <button
            type="button"
            disabled={busyAction !== null}
            onClick={() => onRunLifecycle("uninstall")}
            className="btn-glass px-3.5 py-1.5 text-sub font-medium text-[#ff6b6b]"
          >
            卸载
          </button>
        )}
      </div>
    </div>
  );
}

interface PluginDetailTabPanelProps {
  tab: DetailTab;
  pluginId: string;
  detail: PluginDetailData;
  uiDesc: UiDescriptorView | null;
  uiAuthExpired: boolean;
  perms: PluginPermissionsView | null;
  tasks: PluginTaskView[];
  receiptsByTask: Record<string, TaskReceiptView[]>;
  events: PluginEventDeliveryView[];
  audit: AuditEventView[];
  busyAction: string | null;
  requestedView: string;
  onSelectTab: (nextTab: DetailTab, viewId?: string) => void;
  onReload: () => void;
  onReceiptsLoaded: (taskId: string, items: TaskReceiptView[]) => void;
  actions: ReturnType<typeof usePluginDetailActions>;
}

function PluginDetailTabPanel(p: PluginDetailTabPanelProps) {
  const toast = useToast();
  const confirm = useConfirm();
  const theme = useTheme();
  const pageSurfaces = (p.uiDesc?.surfaces ?? []).filter((s) => s.kind === "page");
  const activePageSurface = pageSurfaces.find((s) => s.id === p.requestedView) ?? pageSurfaces[0] ?? null;
  const reloadKey = `${p.detail.revision}:${p.detail.lifecycle}:${p.detail.enabled ? 1 : 0}:${p.detail.active_operation_id ?? ""}:${p.uiDesc?.trust?.revision ?? 0}`;
  const pluginEditable = isPluginConfigEditable(p.detail);

  if (p.tab === "overview") {
    return (
      <PluginOverviewSection
        pluginId={p.pluginId}
        detail={p.detail}
        uiDesc={p.uiDesc}
        uiAuthExpired={p.uiAuthExpired}
        busyAction={p.busyAction}
        onPrepareTrust={() => void p.actions.handlePrepareTrust()}
        onRevokeTrust={() => void p.actions.handleRevokeTrust()}
      />
    );
  }
  if (p.tab === "config") {
    return (
      <PluginConfigHost
        pluginId={p.pluginId}
        reloadKey={reloadKey}
        pluginEditable={pluginEditable}
        credentialBound={p.detail.credential_bound}
        onSaved={p.onReload}
      />
    );
  }
  if (p.tab === "page" && activePageSurface) {
    return (
      <div className="space-y-4">
        {pageSurfaces.length > 1 && (
          <div className="flex flex-wrap gap-2">
            {pageSurfaces.map((s) => (
              <button
                key={s.id}
                type="button"
                onClick={() => p.onSelectTab("page", s.id)}
                className={`rounded-lg px-3 py-1.5 text-caption font-medium ${
                  activePageSurface.id === s.id
                    ? "bg-white/15 text-[var(--text)]"
                    : "bg-white/[0.04] text-[var(--text-muted)]"
                }`}
              >
                {s.title}
              </button>
            ))}
          </div>
        )}
        <PluginSurfaceHost
          pluginId={p.pluginId}
          surfaceId={activePageSurface.id}
          expectedKind="page"
          reloadKey={reloadKey}
          deps={{
            theme: theme.id,
            notifyToast: (msg) => (msg.level === "error" ? toast.error(msg.message) : toast.info(msg.message)),
            openConfirm: (c) => confirm({ title: c.title, description: c.message, confirmLabel: c.confirmLabel }),
          }}
        />
      </div>
    );
  }
  if (p.tab === "permissions" && p.perms) {
    return (
      <PluginPermissionsTab
        perms={p.perms}
        pluginEditable={pluginEditable}
        busyAction={p.busyAction}
        onGrantAll={() => void p.actions.handleGrantMissingPerms()}
        onRevokeGrant={(g) => void p.actions.handleRevokeSingleGrant(g)}
      />
    );
  }
  if (p.tab === "tasks") {
    return (
      <PluginTasksTab
        pluginId={p.pluginId}
        tasks={p.tasks}
        receiptsByTask={p.receiptsByTask}
        onReceiptsLoaded={p.onReceiptsLoaded}
        busyAction={p.busyAction}
        onToggleTask={(task) => void p.actions.handleToggleTask(task)}
        onRunTask={(task) => void p.actions.handleRunTask(task)}
        onTasksChanged={p.onReload}
      />
    );
  }
  if (p.tab === "audit") {
    return (
      <PluginAuditTab
        events={p.events}
        audit={p.audit}
        busyAction={p.busyAction}
        onRequeueEvent={(eventId) => void p.actions.handleRequeueEvent(eventId)}
      />
    );
  }
  return null;
}

function usePluginDetailLoader(pluginId: string) {
  const sessionCtx = useOptionalSession();
  const sessionUserId = sessionCtx?.session.id ?? null;
  const [detail, setDetail] = useState<PluginDetailData | null>(null);
  const [uiDesc, setUiDesc] = useState<UiDescriptorView | null>(null);
  const [uiAuthExpired, setUiAuthExpired] = useState(false);
  const [perms, setPerms] = useState<PluginPermissionsView | null>(null);
  const [tasks, setTasks] = useState<PluginTaskView[]>([]);
  const [receiptsByTask, setReceiptsByTask] = useState<Record<string, TaskReceiptView[]>>({});
  const [audit, setAudit] = useState<AuditEventView[]>([]);
  const [events, setEvents] = useState<PluginEventDeliveryView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const reqSeqRef = useRef(0);
  const loadedPluginRef = useRef<string | null>(null);

  const loadAll = useCallback(async () => {
    const seq = ++reqSeqRef.current;
    if (loadedPluginRef.current !== pluginId) {
      setLoading(true);
    }
    setError(null);
    let saw401 = false;
    try {
      const [det, ui, perm, taskPage, auditPage, evItems] = await Promise.all([
        getPluginDetail(pluginId),
        fetchPluginUiDescriptor(pluginId).catch((err: unknown) => {
          if (isUnauthorizedError(err)) saw401 = true;
          return null;
        }),
        getPluginPermissions(pluginId).catch(() => null),
        listPluginTasks(pluginId).catch(() => ({ items: [], next_cursor: null })),
        listPluginAudit(pluginId).catch(() => ({ items: [], next_cursor: null })),
        listPluginEvents(pluginId).catch(() => []),
      ]);
      if (seq !== reqSeqRef.current) return;
      loadedPluginRef.current = pluginId;
      const subjectMismatch = Boolean(
        ui && sessionUserId !== null && ui.subject_user_id !== sessionUserId,
      );
      const effectiveUi = subjectMismatch ? null : ui;
      const retainFederation = Boolean(
        effectiveUi &&
          effectiveUi.ui_trusted &&
          effectiveUi.ui_assets_present !== false &&
          effectiveUi.entry_url &&
          effectiveUi.surfaces.some((s) => s.mountable),
      );
      if (!retainFederation) {
        evictPluginFederationCache(pluginId);
      }
      setDetail(det);
      setUiDesc(effectiveUi);
      setUiAuthExpired(saw401);
      setPerms(perm);
      setTasks(taskPage.items);
      setAudit(auditPage.items);
      setEvents(evItems);
    } catch (err) {
      if (seq !== reqSeqRef.current) return;
      setError((err as Error).message || "读取插件详情失败");
    } finally {
      if (seq === reqSeqRef.current) {
        setLoading(false);
      }
    }
  }, [pluginId, sessionUserId]);

  useEffect(() => {
    void loadAll();
    return () => {
      reqSeqRef.current += 1;
    };
  }, [loadAll]);
  return {
    detail,
    uiDesc,
    uiAuthExpired,
    perms,
    tasks,
    receiptsByTask,
    setReceiptsByTask,
    audit,
    events,
    loading,
    error,
    loadAll,
  };
}

export function PluginDetailView({ pluginId }: { pluginId: string }) {
  const [searchParams, setSearchParams] = useSearchParams();
  const tab = (searchParams.get("tab") as DetailTab | null) ?? "overview";
  const requestedView = searchParams.get("view") ?? "";

  const state = usePluginDetailLoader(pluginId);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [trustPrep, setTrustPrep] = useState<UiTrustPreparationView | null>(null);
  const [reauthPassword, setReauthPassword] = useState("");
  const [trustBusy, setTrustBusy] = useState(false);

  const actions = usePluginDetailActions({
    pluginId,
    detail: state.detail,
    uiDesc: state.uiDesc,
    perms: state.perms,
    trustPrep,
    reauthPassword,
    setBusyAction,
    setTrustPrep,
    setReauthPassword,
    setTrustBusy,
    setReceiptsByTask: state.setReceiptsByTask,
    loadAll: state.loadAll,
  });

  function selectTab(nextTab: DetailTab, viewId?: string) {
    const next = new URLSearchParams(searchParams);
    next.set("tab", nextTab);
    if (viewId) next.set("view", viewId);
    else if (nextTab !== "page") next.delete("view");
    setSearchParams(next, { replace: true });
  }

  if (state.loading) {
    return <div className="mx-auto max-w-6xl p-8 text-sub text-[var(--text-muted)]">正在加载插件详情…</div>;
  }
  if (state.error || !state.detail) {
    return (
      <div className="mx-auto max-w-6xl space-y-4 p-8">
        <Link to="/settings/plugins" className="btn-glass inline-flex px-3.5 py-1.5 text-sub">
          返回插件列表
        </Link>
        <div role="alert" className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 p-4 text-sub text-[#ff6b6b]">
          {state.error ?? "插件不存在"}
        </div>
      </div>
    );
  }

  const pageSurfaces = (state.uiDesc?.surfaces ?? []).filter((s) => s.kind === "page");
  const appSurfaces = (state.uiDesc?.surfaces ?? []).filter((s) => s.kind === "app");
  const tabs: Array<{ id: DetailTab; label: string; hidden?: boolean }> = [
    { id: "overview", label: "概览与信任" },
    { id: "config", label: "配置" },
    { id: "page", label: "控制台", hidden: pageSurfaces.length === 0 },
    { id: "permissions", label: "权限与能力" },
    { id: "tasks", label: `任务 (${state.tasks.length})` },
    { id: "audit", label: "审计与事件" },
  ];

  return (
    <div className="scroll-thin scroll-safe h-full overflow-y-auto">
      <div className="mx-auto max-w-6xl space-y-6 px-6 pb-20 pt-10 max-md:px-4">
        <PluginDetailHeader
          detail={state.detail}
          appSurfaces={appSurfaces}
          busyAction={busyAction}
          onRunLifecycle={(act) => void actions.runLifecycle(act)}
        />
        <div className="flex flex-wrap gap-2 border-b border-white/10 pb-3">
          {tabs.filter((t) => !t.hidden).map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => selectTab(t.id)}
              className={`rounded-full px-3.5 py-1.5 text-sub font-medium transition ${
                tab === t.id
                  ? "bg-[var(--accent)] text-white"
                  : "bg-white/[0.05] text-[var(--text-muted)] hover:bg-white/[0.09] hover:text-[var(--text)]"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
        <PluginDetailTabPanel
          tab={tab}
          pluginId={pluginId}
          detail={state.detail}
          uiDesc={state.uiDesc}
          uiAuthExpired={state.uiAuthExpired}
          perms={state.perms}
          tasks={state.tasks}
          receiptsByTask={state.receiptsByTask}
          events={state.events}
          audit={state.audit}
          busyAction={busyAction}
          requestedView={requestedView}
          onSelectTab={selectTab}
          onReload={() => void state.loadAll()}
          onReceiptsLoaded={(taskId, items) => state.setReceiptsByTask((prev) => ({ ...prev, [taskId]: items }))}
          actions={actions}
        />
      </div>
      {trustPrep && (
        <TrustGrantModal
          trustPrep={trustPrep}
          reauthPassword={reauthPassword}
          trustBusy={trustBusy}
          onPasswordChange={setReauthPassword}
          onClose={() => setTrustPrep(null)}
          onConfirm={(e) => void actions.handleConfirmTrust(e)}
        />
      )}
    </div>
  );
}
