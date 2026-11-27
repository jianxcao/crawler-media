"use client";

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import { PluginMarketTab } from "@/components/plugins/PluginMarketTab";
import { PluginUploadModal } from "@/components/plugins/PluginUploadModal";
import {
  createTrustKey,
  disableDeveloperMode,
  disablePlugin,
  enableDeveloperMode,
  enablePlugin,
  ensureOperationSucceeded,
  getPluginSettings,
  listPluginRepositories,
  listPlugins,
  listTrustKeys,
  type PluginRepositoryInput,
  type PluginRepositoryView,
  type PluginSettingsView,
  type PluginSummaryView,
  prepareTrustKey,
  reauthPluginAdmin,
  revokeTrustKey,
  savePluginRepositories,
  type TrustKeyPreparationView,
  type TrustKeyView,
  uninstallPlugin,
  waitOperationTerminal,
} from "@/lib/api/plugins";
import { evictPluginFederationCache } from "@/lib/plugins/federation-loader";
import { useTabParam } from "@/lib/use-tab-param";

const TABS = [
  { id: "installed", label: "已安装" },
  { id: "market", label: "插件市场" },
  { id: "sources", label: "仓库与信任密钥" },
] as const;

type ManagerTab = (typeof TABS)[number]["id"];

const LIFECYCLE_BADGE: Record<string, string> = {
  enabled: "border-emerald-400/30 bg-emerald-400/15 text-emerald-300",
  disabled: "border-white/15 bg-white/[0.06] text-[var(--text-muted)]",
  "installed-disabled": "border-white/15 bg-white/[0.06] text-[var(--text-muted)]",
  failed: "border-[#ff6b6b]/30 bg-[#ff6b6b]/15 text-[#ff6b6b]",
  draining: "border-amber-400/30 bg-amber-400/15 text-amber-300",
};

export function PluginManagerView() {
  const toast = useToast();
  const confirm = useConfirm();
  const [tab, setTab] = useTabParam<ManagerTab>(TABS.map((t) => t.id), "installed");
  const [plugins, setPlugins] = useState<PluginSummaryView[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [uploadOpen, setUploadOpen] = useState(false);
  const [uploadTarget, setUploadTarget] = useState<PluginSummaryView | null>(null);

  const [repos, setRepos] = useState<PluginRepositoryView[]>([]);
  const [settings, setSettings] = useState<PluginSettingsView | null>(null);
  const [trustKeys, setTrustKeys] = useState<TrustKeyView[]>([]);
  const [sourcesLoading, setSourcesLoading] = useState(false);
  const [sourcesLoaded, setSourcesLoaded] = useState(false);
  const [sourcesError, setSourcesError] = useState<string | null>(null);
  const [sourcesBusy, setSourcesBusy] = useState(false);
  const [newRepo, setNewRepo] = useState({ id: "", name: "", index_url: "" });

  const [devEnableModal, setDevEnableModal] = useState(false);
  const [devPassword, setDevPassword] = useState("");
  const [addKeyModal, setAddKeyModal] = useState(false);
  const [keyDraft, setKeyDraft] = useState({ label: "", publicKey: "", password: "" });
  const [keyPrep, setKeyPrep] = useState<TrustKeyPreparationView | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<TrustKeyView | null>(null);
  const [revokeForm, setRevokeForm] = useState({ reason: "", password: "" });

  const loadInstalled = useCallback(async () => {
    setError(null);
    try {
      setPlugins((await listPlugins()).items);
    } catch (err) {
      setError((err as Error).message || "无法加载插件列表");
    } finally {
      setLoading(false);
    }
  }, []);

  const loadSources = useCallback(async () => {
    setSourcesLoading(true);
    setSourcesError(null);
    try {
      const [repoRes, cfgRes, keysRes] = await Promise.all([
        listPluginRepositories(),
        getPluginSettings(),
        listTrustKeys(),
      ]);
      setRepos(repoRes.repositories);
      setSettings(cfgRes);
      setTrustKeys(keysRes.items);
      setSourcesLoaded(true);
    } catch (err) {
      setSourcesError((err as Error).message || "加载仓库与信任配置失败");
    } finally {
      setSourcesLoading(false);
    }
  }, []);

  useEffect(() => { void loadInstalled(); }, [loadInstalled]);
  useEffect(() => { if (tab === "sources") void loadSources(); }, [tab, loadSources]);

  async function toggleLifecycle(plugin: PluginSummaryView) {
    const enabling = !plugin.enabled;
    if (!enabling) {
      const ok = await confirm({
        title: `停用插件「${plugin.name}」？`,
        description: "停用后将暂停调度任务与运行态界面，但保留已保存的配置与数据。",
        confirmLabel: "停用",
      });
      if (!ok) return;
    }
    setBusyId(plugin.id);
    try {
      const op = enabling
        ? await enablePlugin(plugin.id, plugin.revision)
        : await disablePlugin(plugin.id, plugin.revision, "admin-ui-toggle");
      ensureOperationSucceeded(await waitOperationTerminal(op));
      toast.success(enabling ? `已启用 ${plugin.name}` : `已停用 ${plugin.name}`);
      await loadInstalled();
    } catch (err) {
      toast.error((err as Error).message || "操作失败");
    } finally {
      setBusyId(null);
    }
  }

  async function handleUninstall(plugin: PluginSummaryView) {
    const res = await confirm({
      title: `卸载插件「${plugin.name}」？`,
      description: "卸载后将移除插件版本与权限授予。",
      confirmLabel: "卸载",
      tone: "danger",
      checkbox: { label: "同时删除插件私有数据", description: "一并清除该插件保存的配置、密钥和状态数据" },
    });
    if (!res.ok) return;
    setBusyId(plugin.id);
    try {
      const op = await uninstallPlugin(plugin.id, res.checked, plugin.revision);
      ensureOperationSucceeded(await waitOperationTerminal(op, 30_000));
      evictPluginFederationCache(plugin.id);
      toast.success(`已卸载 ${plugin.name}`);
      await loadInstalled();
    } catch (err) {
      toast.error((err as Error).message || "卸载失败");
    } finally {
      setBusyId(null);
    }
  }

  async function persistRepositories(next: PluginRepositoryInput[], message: string) {
    if (sourcesLoading || !sourcesLoaded) return;
    setSourcesBusy(true);
    try {
      setRepos((await savePluginRepositories(next)).repositories);
      toast.success(message);
    } catch (err) {
      toast.error((err as Error).message || "保存仓库列表失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  async function handleAddRepo(e: React.FormEvent) {
    e.preventDefault();
    if (sourcesLoading || !sourcesLoaded) return;
    const id = newRepo.id.trim();
    const index_url = newRepo.index_url.trim();
    if (!id || !index_url) return;
    await persistRepositories(
      [...repos, { id, name: newRepo.name.trim() || id, index_url, enabled: true, builtin: false }],
      "已添加插件仓库",
    );
    setNewRepo({ id: "", name: "", index_url: "" });
  }

  async function handleToggleDeveloperMode() {
    if (!settings) return;
    if (!settings.developer_mode) { setDevPassword(""); setDevEnableModal(true); return; }
    const ok = await confirm({
      title: "关闭插件开发者模式？",
      description: "关闭后将自动停用所有未签名的开发模式插件。",
      confirmLabel: "关闭开发者模式",
      tone: "danger",
    });
    if (!ok) return;
    setSourcesBusy(true);
    try {
      const op = await disableDeveloperMode(settings.revision);
      ensureOperationSucceeded(await waitOperationTerminal(op, 30_000));
      toast.success("已关闭开发者模式");
      await Promise.all([loadSources(), loadInstalled()]);
    } catch (err) {
      toast.error((err as Error).message || "关闭开发者模式失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  async function handleConfirmEnableDevMode(e: React.FormEvent) {
    e.preventDefault();
    if (!settings) return;
    setSourcesBusy(true);
    try {
      const reauth = await reauthPluginAdmin(devPassword, "developer-mode:enable");
      setSettings(await enableDeveloperMode(reauth.reauth_token, settings.revision));
      setDevEnableModal(false);
      setDevPassword("");
      toast.success("已开启开发者模式");
    } catch (err) {
      toast.error((err as Error).message || "开启开发者模式失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  async function handlePrepareKey(e: React.FormEvent) {
    e.preventDefault();
    setSourcesBusy(true);
    try {
      setKeyPrep(await prepareTrustKey(keyDraft.publicKey.trim(), keyDraft.label.trim()));
      setKeyDraft((d) => ({ ...d, password: "" }));
    } catch (err) {
      toast.error((err as Error).message || "解析公钥失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  async function handleCreateKey(e: React.FormEvent) {
    e.preventDefault();
    if (!keyPrep) return;
    setSourcesBusy(true);
    try {
      const reauth = await reauthPluginAdmin(keyDraft.password, "trust-key:create");
      await createTrustKey({
        label: keyPrep.label, public_key: keyPrep.public_key, fingerprint: keyPrep.fingerprint,
        consent_token: keyPrep.consent_token, reauth_token: reauth.reauth_token,
      });
      setAddKeyModal(false); setKeyPrep(null); setKeyDraft({ label: "", publicKey: "", password: "" });
      toast.success("已添加信任密钥");
      await loadSources();
    } catch (err) {
      toast.error((err as Error).message || "添加信任密钥失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  async function handleConfirmRevokeKey(e: React.FormEvent) {
    e.preventDefault();
    if (!revokeTarget) return;
    setSourcesBusy(true);
    try {
      const reauth = await reauthPluginAdmin(revokeForm.password, "trust-key:revoke");
      const op = await revokeTrustKey(
        revokeTarget.key_id,
        { reason: revokeForm.reason.trim() || "admin-ui-revoke", reauth_token: reauth.reauth_token },
        revokeTarget.revision,
      );
      ensureOperationSucceeded(await waitOperationTerminal(op, 30_000));
      setRevokeTarget(null); setRevokeForm({ reason: "", password: "" });
      toast.success("已吊销签名信任密钥");
      await Promise.all([loadSources(), loadInstalled()]);
    } catch (err) {
      toast.error((err as Error).message || "吊销密钥失败");
    } finally {
      setSourcesBusy(false);
    }
  }

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap gap-1.5" role="tablist" aria-label="插件管理分区">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              onClick={() => setTab(t.id)}
              className={`rounded-full px-3.5 py-1.5 text-sub font-medium transition-colors ${
                tab === t.id ? "bg-white/[0.14] text-white" : "text-[var(--text-muted)] hover:bg-white/[0.07] hover:text-[var(--text)]"
              }`}
            >
              {t.id === "installed" ? `${t.label} (${plugins.length})` : t.label}
            </button>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            type="button"
            onClick={() => { setUploadTarget(null); setUploadOpen(true); }}
            className="btn-accent rounded-full px-3.5 py-1.5 text-sub font-semibold"
          >
            本地上传安装 (.cmplugin)
          </button>
          <Link to="/dashboard" className="btn-glass px-3.5 py-1.5 text-sub font-medium">插件工作台看板</Link>
          <button type="button" onClick={() => void (tab === "sources" ? loadSources() : loadInstalled())} className="btn-glass px-3.5 py-1.5 text-sub font-medium">刷新</button>
        </div>
      </div>

      {tab === "installed" ? (
        <>
          {error && <div role="alert" className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-sub text-[#ff6b6b]">{error}</div>}
          {loading ? (
            <div className="rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">正在加载已安装插件…</div>
          ) : plugins.length === 0 ? (
            <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">
              <span>暂无已安装插件，可前往「插件市场」安装或本地上传 `.cmplugin`。</span>
              <button type="button" onClick={() => setTab("market")} className="btn-accent rounded-full px-4 py-1.5 text-sub font-semibold">浏览插件市场</button>
            </div>
          ) : (
            <div className="divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08] bg-white/[0.03]">
              {plugins.map((plugin) => {
                const badgeClass = LIFECYCLE_BADGE[plugin.lifecycle] ?? LIFECYCLE_BADGE.disabled;
                const busy = busyId === plugin.id;
                const nonBuiltin = plugin.source !== "builtin";
                return (
                  <div key={plugin.id} className="flex flex-wrap items-center justify-between gap-4 px-5 py-4 transition-colors hover:bg-white/[0.03]">
                    <div className="min-w-0 flex-1 space-y-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Link to={`/settings/plugins/${encodeURIComponent(plugin.id)}`} className="text-body font-semibold text-[var(--text)] hover:underline">
                          {plugin.name}
                        </Link>
                        <span className={`rounded-full border px-2 py-0.5 text-micro font-medium ${badgeClass}`}>{plugin.lifecycle}</span>
                        <span className="rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 text-micro text-[var(--text-muted)]">
                          {plugin.source} · v{plugin.version}
                        </span>
                      </div>
                      <p className="truncate text-caption text-[var(--text-faint)]">
                        {plugin.id} · generation #{plugin.generation} · rev #{plugin.revision} · health: {plugin.health}
                      </p>
                      {plugin.failure_reason && <p className="text-caption text-[#ff6b6b]">{plugin.failure_reason}</p>}
                    </div>
                    <div className="flex shrink-0 flex-wrap items-center gap-2">
                      <button type="button" disabled={busy} onClick={() => void toggleLifecycle(plugin)} className="btn-glass px-3 py-1.5 text-caption font-medium disabled:opacity-50">
                        {busy ? "处理中…" : plugin.enabled ? "停用" : "启用"}
                      </button>
                      <Link to={`/settings/plugins/${encodeURIComponent(plugin.id)}?tab=config`} className="btn-glass px-3 py-1.5 text-caption font-medium">配置</Link>
                      {nonBuiltin && (
                        <>
                          <button type="button" disabled={busy} onClick={() => { setUploadTarget(plugin); setUploadOpen(true); }} className="btn-glass px-3 py-1.5 text-caption font-medium disabled:opacity-50">
                            升级
                          </button>
                          <button type="button" disabled={busy} onClick={() => void handleUninstall(plugin)} className="btn-glass btn-glass--danger px-3 py-1.5 text-caption font-medium disabled:opacity-50">
                            卸载
                          </button>
                        </>
                      )}
                      <Link to={`/settings/plugins/${encodeURIComponent(plugin.id)}`} className="btn-accent rounded-full px-3.5 py-1.5 text-caption font-semibold">管理</Link>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </>
      ) : tab === "market" ? (
        <PluginMarketTab installedPlugins={plugins} onChanged={loadInstalled} onOpenSources={() => setTab("sources")} />
      ) : (
        <div className="space-y-6">
          {sourcesError && <div role="alert" className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-sub text-[#ff6b6b]">{sourcesError}</div>}
          <section className="css-glass space-y-4 p-5">
            <div>
              <h2 className="text-body font-semibold text-[var(--text)]">插件市场仓库源</h2>
              <p className="mt-0.5 text-caption text-[var(--text-muted)]">配置官方与第三方插件市场索引地址 (`index.json`)。</p>
            </div>
            {sourcesLoading ? (
              <p className="text-caption text-[var(--text-muted)]">加载中…</p>
            ) : (
              <div className="space-y-2">
                {repos.map((repo) => (
                  <div key={repo.id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/[0.08] bg-white/[0.02] px-4 py-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="text-sub font-medium text-[var(--text)]">{repo.name}</span>
                        <span className="rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 text-micro text-[var(--text-faint)]">
                          {repo.builtin ? "官方内置" : repo.id}
                        </span>
                      </div>
                      <p className="truncate font-mono text-[11px] text-[var(--text-faint)]">{repo.index_url}</p>
                    </div>
                    <div className="flex items-center gap-2">
                      <button
                        type="button"
                        disabled={sourcesBusy || sourcesLoading || !sourcesLoaded}
                        onClick={() => void persistRepositories(repos.map((r) => (r.id === repo.id ? { ...r, enabled: !r.enabled } : r)), repo.enabled ? `已停用仓库 ${repo.name}` : `已启用仓库 ${repo.name}`)}
                        className="btn-glass px-3 py-1 text-caption font-medium"
                      >
                        {repo.enabled ? "已启用" : "已停用"}
                      </button>
                      {!repo.builtin && (
                        <button type="button" disabled={sourcesBusy || sourcesLoading || !sourcesLoaded} onClick={() => void persistRepositories(repos.filter((r) => r.id !== repo.id), `已移除仓库 ${repo.name}`)} className="btn-glass btn-glass--danger px-3 py-1 text-caption font-medium">
                          移除
                        </button>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
            <form onSubmit={(e) => void handleAddRepo(e)} className="grid gap-2.5 border-t border-white/[0.08] pt-4 sm:grid-cols-4">
              <input type="text" required value={newRepo.id} onChange={(e) => setNewRepo((s) => ({ ...s, id: e.target.value }))} placeholder="仓库 ID (如 community)" className="input-glass rounded-lg px-3 py-2 text-caption" />
              <input type="text" value={newRepo.name} onChange={(e) => setNewRepo((s) => ({ ...s, name: e.target.value }))} placeholder="显示名称" className="input-glass rounded-lg px-3 py-2 text-caption" />
              <input type="url" required value={newRepo.index_url} onChange={(e) => setNewRepo((s) => ({ ...s, index_url: e.target.value }))} placeholder="https://.../registry/index.json" className="input-glass rounded-lg px-3 py-2 text-caption sm:col-span-2" />
              <div className="sm:col-span-4 flex justify-end">
                <button type="submit" disabled={sourcesBusy || sourcesLoading || !sourcesLoaded || !newRepo.id.trim() || !newRepo.index_url.trim()} className="btn-accent rounded-full px-4 py-1.5 text-caption font-semibold disabled:opacity-50">
                  添加自定义仓库
                </button>
              </div>
            </form>
          </section>

          <section className="css-glass flex flex-wrap items-center justify-between gap-4 p-5">
            <div className="space-y-1">
              <div className="flex items-center gap-2">
                <h2 className="text-body font-semibold text-[var(--text)]">开发者模式</h2>
                <span className={`rounded-full border px-2 py-0.5 text-micro font-medium ${settings?.developer_mode ? "border-amber-400/30 bg-amber-400/15 text-amber-200" : "border-white/15 bg-white/[0.06] text-[var(--text-muted)]"}`}>
                  {settings?.developer_mode ? "已开启" : "已关闭"}
                </span>
              </div>
              <p className="text-caption text-[var(--text-muted)]">开启后允许本地上传及旁路加载未签名的 `.cmplugin` 开发包；生产环境建议保持关闭。</p>
            </div>
            <button type="button" disabled={!settings || sourcesBusy} onClick={() => void handleToggleDeveloperMode()} className="btn-glass px-4 py-1.5 text-sub font-medium">
              {settings?.developer_mode ? "关闭开发者模式" : "开启开发者模式…"}
            </button>
          </section>

          <section className="css-glass space-y-4 p-5">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <h2 className="text-body font-semibold text-[var(--text)]">Ed25519 信任密钥</h2>
                <p className="mt-0.5 text-caption text-[var(--text-muted)]">仅通过受信任公钥 (Minisign) 验签的插件包才可在非开发者模式下安装运行。</p>
              </div>
              <button type="button" onClick={() => { setKeyDraft({ label: "", publicKey: "", password: "" }); setKeyPrep(null); setAddKeyModal(true); }} className="btn-accent rounded-full px-3.5 py-1.5 text-caption font-semibold">
                添加信任密钥
              </button>
            </div>
            {trustKeys.length === 0 ? (
              <p className="text-caption text-[var(--text-faint)]">暂未配置签名信任密钥。</p>
            ) : (
              <div className="space-y-2">
                {trustKeys.map((k) => {
                  const active = k.status !== "revoked";
                  return (
                    <div key={k.key_id} className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/[0.08] bg-white/[0.02] px-4 py-3 text-caption">
                      <div className="min-w-0 flex-1 space-y-0.5">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="font-semibold text-[var(--text)]">{k.label}</span>
                          <span className={`rounded-full border px-2 py-0.5 text-micro ${active ? "border-emerald-400/30 bg-emerald-400/15 text-emerald-300" : "border-[#ff6b6b]/30 bg-[#ff6b6b]/15 text-[#ff6b6b]"}`}>
                            {active ? "受信任" : "已吊销"}
                          </span>
                          <span className="font-mono text-micro text-[var(--text-faint)]">{k.key_id}</span>
                        </div>
                        <p className="break-all font-mono text-[11px] text-[var(--text-faint)]">fingerprint: {k.fingerprint} · 关联版本: {k.associated_package_count}</p>
                        {k.revoked_reason && <p className="text-[#ff6b6b]">吊销原因：{k.revoked_reason}</p>}
                      </div>
                      {active && (
                        <button type="button" disabled={sourcesBusy} onClick={() => { setRevokeTarget(k); setRevokeForm({ reason: "", password: "" }); }} className="btn-glass btn-glass--danger px-3 py-1 text-caption font-medium">
                          吊销
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </div>
      )}

      <PluginUploadModal open={uploadOpen} onClose={() => setUploadOpen(false)} installedPlugins={plugins} initialUpgradePlugin={uploadTarget} onCompleted={loadInstalled} />

      {devEnableModal && (
        <Modal open onClose={() => setDevEnableModal(false)} label="开启开发者模式" width="md">
          <form onSubmit={(e) => void handleConfirmEnableDevMode(e)} className="space-y-4 p-6">
            <h2 className="text-title font-bold text-[var(--text)]">确认开启开发者模式</h2>
            <p className="text-sub text-[var(--text-muted)]">开启后允许安装未签名插件包。请输入管理员密码确认：</p>
            <input type="password" required value={devPassword} onChange={(e) => setDevPassword(e.target.value)} placeholder="管理员密码" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
            <div className="flex justify-end gap-2">
              <button type="button" onClick={() => setDevEnableModal(false)} className="btn-glass px-4 py-2 text-sub">取消</button>
              <button type="submit" disabled={sourcesBusy || !devPassword} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold">确认开启</button>
            </div>
          </form>
        </Modal>
      )}

      {addKeyModal && (
        <Modal open onClose={() => setAddKeyModal(false)} label="添加 Ed25519 信任密钥" width="lg">
          {!keyPrep ? (
            <form onSubmit={(e) => void handlePrepareKey(e)} className="space-y-4 p-6">
              <h2 className="text-title font-bold text-[var(--text)]">添加 Ed25519 信任密钥</h2>
              <input type="text" required value={keyDraft.label} onChange={(e) => setKeyDraft((d) => ({ ...d, label: e.target.value }))} placeholder="密钥备注名称 (如 official-signing)" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
              <textarea rows={4} required value={keyDraft.publicKey} onChange={(e) => setKeyDraft((d) => ({ ...d, publicKey: e.target.value }))} placeholder="粘贴 Minisign 公钥内容 (如 untrusted comment: ... / RWS...)" className="input-glass w-full rounded-lg px-3 py-2 font-mono text-caption" />
              <div className="flex justify-end gap-2">
                <button type="button" onClick={() => setAddKeyModal(false)} className="btn-glass px-4 py-2 text-sub">取消</button>
                <button type="submit" disabled={sourcesBusy || !keyDraft.label.trim() || !keyDraft.publicKey.trim()} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold">解析并校验公钥</button>
              </div>
            </form>
          ) : (
            <form onSubmit={(e) => void handleCreateKey(e)} className="space-y-4 p-6">
              <h2 className="text-title font-bold text-[var(--text)]">核对指纹并确认添加</h2>
              <div className="space-y-1 rounded-xl border border-white/10 bg-white/[0.03] p-3.5 text-caption">
                <p>备注：{keyPrep.label}</p>
                <p className="font-mono">Key ID: {keyPrep.key_id}</p>
                <p className="break-all font-mono text-[11px]">Fingerprint: {keyPrep.fingerprint}</p>
              </div>
              <input type="password" required value={keyDraft.password} onChange={(e) => setKeyDraft((d) => ({ ...d, password: e.target.value }))} placeholder="管理员密码二次验证" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
              <div className="flex justify-end gap-2">
                <button type="button" onClick={() => setKeyPrep(null)} className="btn-glass px-4 py-2 text-sub">返回</button>
                <button type="submit" disabled={sourcesBusy || !keyDraft.password} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold">确认添加信任密钥</button>
              </div>
            </form>
          )}
        </Modal>
      )}

      {revokeTarget && (
        <Modal open onClose={() => setRevokeTarget(null)} label="吊销信任密钥" width="md">
          <form onSubmit={(e) => void handleConfirmRevokeKey(e)} className="space-y-4 p-6">
            <h2 className="text-title font-bold text-[var(--text)]">吊销密钥「{revokeTarget.label}」</h2>
            <p className="text-caption text-[var(--text-muted)]">吊销后将立即停用所有使用该密钥签名的活动插件。</p>
            <input type="text" required value={revokeForm.reason} onChange={(e) => setRevokeForm((f) => ({ ...f, reason: e.target.value }))} placeholder="吊销原因 (如 key-rotated)" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
            <input type="password" required value={revokeForm.password} onChange={(e) => setRevokeForm((f) => ({ ...f, password: e.target.value }))} placeholder="管理员密码二次验证" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
            <div className="flex justify-end gap-2">
              <button type="button" onClick={() => setRevokeTarget(null)} className="btn-glass px-4 py-2 text-sub">取消</button>
              <button type="submit" disabled={sourcesBusy || !revokeForm.password || !revokeForm.reason.trim()} className="btn-danger rounded-full px-4 py-2 text-sub font-semibold">确认吊销</button>
            </div>
          </form>
        </Modal>
      )}
    </div>
  );
}
