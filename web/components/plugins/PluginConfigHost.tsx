"use client";

import type { PluginConfigController } from "@crawler-media/plugin-ui-sdk";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useToast } from "@/components/feedback";
import {
  PluginSchemaForm,
  schemaHasSecrets,
  type SchemaOptionItem,
} from "@/components/plugins/PluginSchemaForm";
import { PluginSurfaceHost, type PluginSurfaceHostDeps } from "@/components/plugins/PluginSurfaceHost";
import { listDownloaders } from "@/lib/api/downloaders";
import {
  fetchPluginUiDescriptor,
  getPluginConfig,
  getPluginConfigSchema,
  getPluginDetail,
  isPluginConfigEditable,
  type PluginConfigSchemaView,
  type PluginConfigView,
  type PluginDetailView,
  type SecretWriteAction,
  type UiDescriptorView,
  updatePluginConfig,
} from "@/lib/api/plugins";
import { listConfiguredSites } from "@/lib/api/sites";
import { evictPluginFederationCache } from "@/lib/plugins/federation-loader";
import { useOptionalSession } from "@/lib/session";
import { useTheme } from "@/lib/ui-prefs";

function isConfigMountableLifecycle(lifecycle: string): boolean {
  return (
    lifecycle === "enabled" ||
    lifecycle === "disabled" ||
    lifecycle === "installed-disabled"
  );
}

function isFederatedConfigAvailable(uiDesc: UiDescriptorView | null): boolean {
  const configSurface = uiDesc?.surfaces.find((s) => s.kind === "config");
  return Boolean(
    uiDesc &&
      isConfigMountableLifecycle(uiDesc.lifecycle) &&
      uiDesc.ui_mode === "federation-react" &&
      uiDesc.ui_trusted &&
      uiDesc.ui_assets_present !== false &&
      configSurface &&
      configSurface.mountable,
  );
}

function shouldEvictFederationCache(uiDesc: UiDescriptorView | null): boolean {
  return !(
    uiDesc &&
    uiDesc.ui_trusted &&
    uiDesc.ui_assets_present !== false &&
    uiDesc.entry_url &&
    uiDesc.surfaces.some((s) => s.mountable)
  );
}

function usePluginConfigData(
  pluginId: string,
  reloadKey: string | number | undefined,
  fetchDesc: PluginSurfaceHostDeps["fetchDescriptor"] | undefined,
  skipDetailFetch: boolean,
) {
  const sessionCtx = useOptionalSession();
  const sessionUserId = sessionCtx?.session.id ?? null;
  const [schemaView, setSchemaView] = useState<PluginConfigSchemaView | null>(null);
  const [configView, setConfigView] = useState<PluginConfigView | null>(null);
  const [detailView, setDetailView] = useState<PluginDetailView | null>(null);
  const [uiDesc, setUiDesc] = useState<UiDescriptorView | null>(null);
  const [sites, setSites] = useState<SchemaOptionItem[]>([]);
  const [downloaders, setDownloaders] = useState<SchemaOptionItem[]>([]);
  const [draftValues, setDraftValues] = useState<Record<string, unknown>>({});
  const [useSchemaFallback, setUseSchemaFallback] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchDescRef = useRef(fetchDesc);
  fetchDescRef.current = fetchDesc;
  const loadedPluginRef = useRef<string | null>(null);
  const reqSeqRef = useRef(0);

  const load = useCallback(async () => {
    const seq = ++reqSeqRef.current;
    if (loadedPluginRef.current !== pluginId) {
      setLoading(true);
    }
    setError(null);
    setUseSchemaFallback(false);
    try {
      const [schemaRes, configRes, detailRes, uiRes, siteRes, dlRes] = await Promise.all([
        getPluginConfigSchema(pluginId),
        getPluginConfig(pluginId),
        skipDetailFetch ? Promise.resolve(null) : getPluginDetail(pluginId).catch(() => null),
        (fetchDescRef.current ?? fetchPluginUiDescriptor)(pluginId).catch(() => null),
        listConfiguredSites().catch(() => []),
        listDownloaders().catch(() => []),
      ]);
      if (seq !== reqSeqRef.current) return;
      setSchemaView(schemaRes);
      setConfigView(configRes);
      setDraftValues(configRes.values ?? {});
      setDetailView(
        detailRes &&
          typeof detailRes === "object" &&
          !Array.isArray(detailRes) &&
          typeof detailRes.lifecycle === "string"
          ? detailRes
          : null,
      );
      const subjectMismatch = Boolean(
        uiRes && sessionUserId !== null && uiRes.subject_user_id !== sessionUserId,
      );
      const effectiveUi = subjectMismatch ? null : uiRes;
      if (shouldEvictFederationCache(effectiveUi)) {
        evictPluginFederationCache(pluginId);
      }
      setUiDesc(effectiveUi);
      setSites(siteRes.map((s) => ({ id: s.id, name: s.name })));
      setDownloaders(dlRes.map((d) => ({ id: d.id, name: d.name })));
      loadedPluginRef.current = pluginId;
    } catch (err) {
      if (seq !== reqSeqRef.current) return;
      setError((err as Error).message || "读取插件配置失败");
    } finally {
      if (seq === reqSeqRef.current) {
        setLoading(false);
      }
    }
  }, [pluginId, skipDetailFetch, sessionUserId]);

  useEffect(() => {
    void load();
  }, [load, reloadKey]);

  return {
    schemaView,
    configView,
    setConfigView,
    detailView,
    uiDesc,
    sites,
    downloaders,
    draftValues,
    setDraftValues,
    useSchemaFallback,
    setUseSchemaFallback,
    loading,
    error,
  };
}

function computeConfigEditable(
  pluginEditable: boolean | undefined,
  detailView: PluginDetailView | null,
  uiDesc: UiDescriptorView | null,
  configSurface: UiDescriptorView["surfaces"][number] | undefined,
): boolean {
  if (uiDesc && !isConfigMountableLifecycle(uiDesc.lifecycle)) {
    return false;
  }
  if (pluginEditable !== undefined) {
    return configSurface ? pluginEditable && configSurface.editable : pluginEditable;
  }
  if (detailView) {
    const detailEditable = isPluginConfigEditable(detailView);
    return configSurface ? detailEditable && configSurface.editable : detailEditable;
  }
  if (uiDesc) {
    const lcEditable =
      uiDesc.lifecycle === "installed-disabled" || uiDesc.lifecycle === "disabled";
    return configSurface ? lcEditable && configSurface.editable : lcEditable;
  }
  if (configSurface) {
    return configSurface.editable;
  }
  return false;
}

export function PluginConfigHost({
  pluginId,
  reloadKey,
  pluginEditable,
  credentialBound,
  onSaved,
  surfaceDeps,
}: {
  pluginId: string;
  reloadKey?: string | number;
  pluginEditable?: boolean;
  credentialBound?: boolean;
  onSaved?: () => void;
  surfaceDeps?: PluginSurfaceHostDeps;
}) {
  const toast = useToast();
  const theme = useTheme();
  const data = usePluginConfigData(
    pluginId,
    reloadKey,
    surfaceDeps?.fetchDescriptor,
    pluginEditable !== undefined && credentialBound !== undefined,
  );
  const { schemaView, configView, detailView, uiDesc, draftValues, setDraftValues, setConfigView } =
    data;

  const isCredentialBound = credentialBound ?? detailView?.credential_bound ?? false;
  const configSurface = uiDesc?.surfaces.find((s) => s.kind === "config");
  const editable = computeConfigEditable(pluginEditable, detailView, uiDesc, configSurface);
  const canUseFederatedConfig = isFederatedConfigAvailable(uiDesc);
  const usingFederatedConfig = canUseFederatedConfig && !data.useSchemaFallback;

  const prevFederatedRef = useRef<{ pluginId: string; active: boolean }>({
    pluginId,
    active: false,
  });
  useEffect(() => {
    if (
      prevFederatedRef.current.pluginId === pluginId &&
      prevFederatedRef.current.active &&
      !canUseFederatedConfig &&
      shouldEvictFederationCache(uiDesc)
    ) {
      evictPluginFederationCache(pluginId);
    }
    prevFederatedRef.current = { pluginId, active: canUseFederatedConfig };
  }, [pluginId, canUseFederatedConfig, uiDesc]);

  const saveFullConfig = useCallback(
    async (
      values: Record<string, unknown>,
      secrets: Record<string, SecretWriteAction> = {},
      opts?: { siteId?: string; attachAttribution?: boolean },
    ) => {
      if (!configView || !schemaView) return;
      const hasSecretWrite = Object.values(secrets).some(
        (s) => s.action === "replace" || s.action === "clear",
      );
      const singleSitesEntry = Array.isArray(values.sites)
        ? values.sites
            .filter((s): s is string => typeof s === "string" && s.trim().length > 0)
            .map((s) => s.trim())
        : [];
      const fallbackValueSite =
        typeof values.site_id === "string" && values.site_id.trim()
          ? values.site_id.trim()
          : singleSitesEntry.length === 1
            ? singleSitesEntry[0]
            : "";
      const rawSiteId = opts?.siteId?.trim() || fallbackValueSite;
      const resolvedSiteId = rawSiteId.replace(/^site:/i, "").trim();
      if (isCredentialBound && hasSecretWrite && !resolvedSiteId) {
        throw new Error("凭证绑定插件更新密钥时必须选择绑定站点 (site_id)");
      }
      const updated = await updatePluginConfig(
        pluginId,
        {
          schema_revision: configView.schema_revision || schemaView.schema_revision,
          values,
          secrets,
          ...(isCredentialBound && resolvedSiteId ? { site_id: resolvedSiteId } : {}),
        },
        configView.revision,
        opts?.attachAttribution && usingFederatedConfig && uiDesc
          ? { packageHash: uiDesc.package_hash, generation: uiDesc.generation }
          : undefined,
      );
      setConfigView(updated);
      setDraftValues(updated.values ?? {});
      toast.success("插件配置已保存");
      onSaved?.();
    },
    [
      configView,
      schemaView,
      isCredentialBound,
      pluginId,
      usingFederatedConfig,
      uiDesc,
      setConfigView,
      setDraftValues,
      toast,
      onSaved,
    ],
  );

  const configController = useMemo<PluginConfigController | undefined>(() => {
    if (!configView || !schemaView) return undefined;
    return {
      values: draftValues,
      schemaRevision: configView.schema_revision || schemaView.schema_revision,
      revision: String(configView.revision),
      editable,
      onDraftChange: (next) => setDraftValues(next),
      save: async (next) => {
        await saveFullConfig(next, {}, { attachAttribution: true });
      },
    };
  }, [configView, schemaView, draftValues, editable, setDraftValues, saveFullConfig]);

  if (data.loading) {
    return (
      <div className="rounded-xl border border-white/10 bg-white/[0.03] p-5 text-sub text-[var(--text-muted)]">
        正在加载配置表单…
      </div>
    );
  }
  if (data.error || !schemaView || !configView) {
    return (
      <div className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 p-4 text-sub text-[#ff6b6b]">
        {data.error ?? "无法读取插件配置"}
      </div>
    );
  }

  const hasSecrets = schemaHasSecrets(schemaView.schema, configView.secrets);

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-2 text-caption text-[var(--text-faint)]">
        <span>
          Schema rev: {configView.schema_revision} · 配置版本 #{configView.revision}
        </span>
        {!editable && <span className="text-amber-300">当前状态只读</span>}
      </div>

      {usingFederatedConfig && configSurface && configController ? (
        <>
          <PluginSurfaceHost
            pluginId={pluginId}
            surfaceId={configSurface.id}
            expectedKind="config"
            reloadKey={reloadKey}
            config={configController}
            fallbackAction={{
              label: "切换为基础表单配置",
              onSelect: () => data.setUseSchemaFallback(true),
            }}
            deps={{
              theme: theme.id,
              notifyToast: (msg) =>
                msg.level === "error" ? toast.error(msg.message) : toast.info(msg.message),
              ...surfaceDeps,
            }}
          />
          {hasSecrets && (
            <PluginSchemaForm
              schema={schemaView.schema}
              values={draftValues}
              secrets={configView.secrets}
              editable={editable}
              sites={data.sites}
              downloaders={data.downloaders}
              hideNonSecrets
              credentialBound={isCredentialBound}
              onSave={(mergedValues, secrets, siteId) => saveFullConfig(mergedValues, secrets, { siteId })}
            />
          )}
        </>
      ) : (
        <PluginSchemaForm
          schema={schemaView.schema}
          values={draftValues}
          secrets={configView.secrets}
          editable={editable}
          sites={data.sites}
          downloaders={data.downloaders}
          credentialBound={isCredentialBound}
          onSave={(values, secrets, siteId) => saveFullConfig(values, secrets, { siteId })}
        />
      )}
    </div>
  );
}
