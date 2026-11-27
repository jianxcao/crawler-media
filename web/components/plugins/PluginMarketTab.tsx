"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";

import { useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import {
  PreparationDetailsBox,
  prepareUpgradeWithSavedConfig,
  runPreparedInstallOrUpgrade,
} from "@/components/plugins/PluginUploadModal";
import {
  ensureOperationSucceeded,
  getPluginRepositoryCatalog,
  type InstallationPreparationView,
  listPluginRepositories,
  type PluginCatalogEntryView,
  type PluginRepositoryCatalogView,
  type PluginRepositoryView,
  type PluginSummaryView,
  preparePluginInstall,
  type UpgradePreparationView,
  waitOperationTerminal,
} from "@/lib/api/plugins";

interface MarketModalState {
  entry: PluginCatalogEntryView;
  action: "install" | "upgrade";
  preparing: boolean;
  executing: boolean;
  error: string | null;
  installPrep: InstallationPreparationView | null;
  upgradePrep: UpgradePreparationView | null;
  upgradeBaseRev: number | null;
  statePolicy: "preserve" | "reset";
  resetConfigDefaults: boolean;
  selectedCaps: Set<string>;
  reauthPassword: string;
  autoEnable: boolean;
}

export function PluginMarketTab({
  installedPlugins,
  onChanged,
  onOpenSources,
}: {
  installedPlugins: PluginSummaryView[];
  onChanged: () => Promise<void> | void;
  onOpenSources: () => void;
}) {
  const toast = useToast();
  const [repos, setRepos] = useState<PluginRepositoryView[]>([]);
  const [selectedRepoId, setSelectedRepoId] = useState("");
  const [catalog, setCatalog] = useState<PluginRepositoryCatalogView | null>(null);
  const [loadingRepos, setLoadingRepos] = useState(true);
  const [loadingCatalog, setLoadingCatalog] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [modal, setModal] = useState<MarketModalState | null>(null);

  const installedById = useMemo(() => new Map(installedPlugins.map((p) => [p.id, p])), [installedPlugins]);
  const enabledRepos = useMemo(() => repos.filter((r) => r.enabled), [repos]);

  const loadCatalog = useCallback(async (repoId: string) => {
    if (!repoId) return setCatalog(null);
    setLoadingCatalog(true);
    setError(null);
    try {
      setCatalog(await getPluginRepositoryCatalog(repoId));
    } catch (err) {
      setCatalog(null);
      setError((err as Error).message || "加载插件市场目录失败");
    } finally {
      setLoadingCatalog(false);
    }
  }, []);

  const loadReposAndCatalog = useCallback(async () => {
    setLoadingRepos(true);
    setError(null);
    try {
      const res = await listPluginRepositories();
      setRepos(res.repositories);
      const enabled = res.repositories.filter((r) => r.enabled);
      const nextRepo =
        enabled.find((r) => r.id === selectedRepoId)?.id ??
        enabled.find((r) => r.id === "official")?.id ??
        enabled[0]?.id ??
        "";
      setSelectedRepoId(nextRepo);
      if (nextRepo) await loadCatalog(nextRepo);
      else setCatalog(null);
    } catch (err) {
      setError((err as Error).message || "加载插件仓库列表失败");
    } finally {
      setLoadingRepos(false);
    }
  }, [loadCatalog, selectedRepoId]);

  useEffect(() => {
    void loadReposAndCatalog();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const filteredPlugins = useMemo(() => {
    const list = catalog?.plugins ?? [];
    const q = query.trim().toLowerCase();
    return q
      ? list.filter((p) => p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q) || p.description.toLowerCase().includes(q))
      : list;
  }, [catalog, query]);

  async function runPrepareInModal(
    entry: PluginCatalogEntryView,
    action: "install" | "upgrade",
    policy: "preserve" | "reset",
    useDefaultCfg: boolean,
  ) {
    if (!selectedRepoId) return;
    setModal({
      entry, action, preparing: true, executing: false, error: null,
      installPrep: null, upgradePrep: null, upgradeBaseRev: null,
      statePolicy: policy, resetConfigDefaults: useDefaultCfg,
      selectedCaps: new Set(entry.capabilities), reauthPassword: "", autoEnable: true,
    });
    try {
      if (action === "install") {
        const op = await preparePluginInstall({ repo_id: selectedRepoId, plugin_id: entry.id, version: entry.version });
        const term = ensureOperationSucceeded(await waitOperationTerminal(op, 60_000));
        const prep = term.result as InstallationPreparationView;
        setModal((cur) => (cur ? { ...cur, preparing: false, installPrep: prep, selectedCaps: new Set(prep.grant_diff.required) } : null));
      } else {
        const { prep, baseRevision } = await prepareUpgradeWithSavedConfig(
          entry.id, { repo_id: selectedRepoId, version: entry.version }, policy, useDefaultCfg,
        );
        setModal((cur) => (cur ? { ...cur, preparing: false, upgradePrep: prep, upgradeBaseRev: baseRevision, selectedCaps: new Set(prep.grant_diff.required) } : null));
      }
    } catch (err) {
      setModal((cur) => (cur ? { ...cur, preparing: false, error: (err as Error).message || "拉取或校验插件包失败" } : null));
    }
  }

  async function handleConfirmExecute(e: React.FormEvent) {
    e.preventDefault();
    if (!modal) return;
    setModal((cur) => (cur ? { ...cur, executing: true, error: null } : null));
    try {
      const res = await runPreparedInstallOrUpgrade({
        installPrep: modal.installPrep,
        upgradePrep: modal.upgradePrep,
        upgradeBaseRev: modal.upgradeBaseRev,
        selectedCaps: modal.selectedCaps,
        reauthPassword: modal.reauthPassword,
        autoEnable: modal.autoEnable,
      });
      if (res.enableWarning) toast.info(`已${res.upgraded ? "升级" : "安装"} ${modal.entry.name}，但自动启用失败：${res.enableWarning}`);
      else toast.success(`已${res.upgraded ? "升级" : "安装"} ${modal.entry.name} v${modal.entry.version}`);
      setModal(null);
      await onChanged();
      await loadCatalog(selectedRepoId);
    } catch (err) {
      setModal((cur) => (cur ? { ...cur, executing: false, error: (err as Error).message || "安装或升级失败" } : null));
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <select
            aria-label="选择插件仓库"
            value={selectedRepoId}
            disabled={loadingRepos || enabledRepos.length === 0}
            onChange={(e) => { setSelectedRepoId(e.target.value); void loadCatalog(e.target.value); }}
            className="input-glass rounded-full px-3.5 py-1.5 text-sub font-medium"
          >
            {enabledRepos.length === 0 ? (
              <option value="">暂无已启用仓库</option>
            ) : (
              enabledRepos.map((repo) => (
                <option key={repo.id} value={repo.id}>{repo.name} ({repo.builtin ? "官方" : repo.id})</option>
              ))
            )}
          </select>
          <input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索插件名称、ID 或说明…"
            className="input-glass w-60 rounded-full px-3.5 py-1.5 text-sub max-sm:w-full"
          />
        </div>
        <div className="flex items-center gap-2">
          <button type="button" onClick={onOpenSources} className="btn-glass px-3.5 py-1.5 text-sub font-medium">管理仓库源</button>
          <button type="button" disabled={!selectedRepoId || loadingCatalog} onClick={() => void loadCatalog(selectedRepoId)} className="btn-glass px-3.5 py-1.5 text-sub font-medium">
            {loadingCatalog ? "同步中…" : "刷新目录"}
          </button>
        </div>
      </div>

      {error && <div role="alert" className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-sub text-[#ff6b6b]">{error}</div>}

      {loadingRepos || loadingCatalog ? (
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">正在获取插件市场目录…</div>
      ) : enabledRepos.length === 0 ? (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">
          <span>当前没有已启用的插件仓库源。</span>
          <button type="button" onClick={onOpenSources} className="btn-accent rounded-full px-4 py-1.5 text-sub font-semibold">前往启用仓库</button>
        </div>
      ) : filteredPlugins.length === 0 ? (
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">
          {catalog?.plugins.length ? "没有匹配的插件。" : "该仓库暂无可用插件。"}
        </div>
      ) : (
        <div className="grid gap-4 lg:grid-cols-2">
          {filteredPlugins.map((entry) => {
            const local = installedById.get(entry.id);
            const installedVer = entry.installed_version ?? local?.version ?? null;
            const lifecycle = entry.lifecycle ?? local?.lifecycle ?? null;
            const homepageOk = Boolean(entry.homepage && /^https?:\/\//i.test(entry.homepage));
            return (
              <article key={entry.id} className="css-glass flex flex-col justify-between gap-4 p-5">
                <div className="space-y-2.5">
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="min-w-0 space-y-0.5">
                      <div className="flex flex-wrap items-center gap-2">
                        <h3 className="text-body font-semibold text-[var(--text)]">{entry.name}</h3>
                        <span className="rounded-full border border-white/10 bg-white/[0.05] px-2 py-0.5 text-micro text-[var(--text-muted)]">v{entry.version}</span>
                      </div>
                      <p className="text-caption text-[var(--text-faint)]">
                        {entry.id} · {entry.authors.join(", ") || "unknown"} · {entry.license}
                      </p>
                    </div>
                    {installedVer ? (
                      entry.update_available ? (
                        <span className="rounded-full border border-amber-400/30 bg-amber-400/15 px-2.5 py-0.5 text-micro font-medium text-amber-200">
                          可升级 v{installedVer} → v{entry.version}
                        </span>
                      ) : (
                        <span className="rounded-full border border-emerald-400/30 bg-emerald-400/15 px-2.5 py-0.5 text-micro font-medium text-emerald-300">
                          已安装 v{installedVer}{lifecycle ? ` (${lifecycle})` : ""}
                        </span>
                      )
                    ) : (
                      <span className="rounded-full border border-white/10 bg-white/[0.04] px-2.5 py-0.5 text-micro text-[var(--text-faint)]">未安装</span>
                    )}
                  </div>
                  <p className="text-sub leading-relaxed text-[var(--text-muted)]">{entry.description || "暂无简介"}</p>
                  {entry.capabilities.length > 0 && (
                    <div className="flex flex-wrap gap-1.5">
                      {entry.capabilities.map((cap) => (
                        <span key={cap} className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2 py-0.5 font-mono text-micro text-[var(--text-muted)]">{cap}</span>
                      ))}
                    </div>
                  )}
                </div>
                <div className="flex flex-wrap items-center justify-between gap-2 border-t border-white/[0.06] pt-3">
                  <div className="flex items-center gap-3 text-micro text-[var(--text-faint)]">
                    <span className="font-mono">sha256:{entry.sha256.slice(0, 12)}…</span>
                    {homepageOk && entry.homepage && (
                      <a href={entry.homepage} target="_blank" rel="noreferrer" className="text-[var(--text-muted)] hover:text-[var(--text)] hover:underline">主页</a>
                    )}
                  </div>
                  <div className="flex items-center gap-2">
                    {installedVer && (
                      <Link to={`/settings/plugins/${encodeURIComponent(entry.id)}`} className="btn-glass px-3 py-1.5 text-caption font-medium">管理</Link>
                    )}
                    {!installedVer ? (
                      <button type="button" onClick={() => void runPrepareInModal(entry, "install", "preserve", false)} className="btn-accent rounded-full px-4 py-1.5 text-caption font-semibold">
                        安装
                      </button>
                    ) : entry.update_available ? (
                      <button type="button" onClick={() => void runPrepareInModal(entry, "upgrade", "preserve", false)} className="btn-accent rounded-full px-4 py-1.5 text-caption font-semibold">
                        升级到 v{entry.version}
                      </button>
                    ) : (
                      <button type="button" disabled className="btn-glass px-3.5 py-1.5 text-caption font-medium">已是最新</button>
                    )}
                  </div>
                </div>
              </article>
            );
          })}
        </div>
      )}

      {modal && (
        <Modal open onClose={modal.preparing || modal.executing ? () => {} : () => setModal(null)} label={modal.action === "upgrade" ? "确认升级插件" : "确认安装插件"} width="lg">
          <form onSubmit={(e) => void handleConfirmExecute(e)} className="space-y-4 p-6">
            <h2 className="text-title font-bold text-[var(--text)]">{modal.action === "upgrade" ? `升级 ${modal.entry.name}` : `安装 ${modal.entry.name}`}</h2>
            {modal.preparing ? (
              <div className="rounded-xl border border-white/10 bg-white/[0.03] p-5 text-sub text-[var(--text-muted)]">正在从仓库下载包并校验 SHA-256 摘要、签名与 WASM 组件…</div>
            ) : modal.error ? (
              <div role="alert" className="space-y-3 rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 p-4 text-caption text-[#ff6b6b]">
                <p>{modal.error}</p>
                {modal.action === "upgrade" && (
                  <div className="flex flex-wrap gap-2">
                    <button type="button" onClick={() => void runPrepareInModal(modal.entry, "upgrade", "reset", modal.resetConfigDefaults)} className="btn-glass px-3 py-1 text-micro font-medium text-[var(--text)]">
                      以重置状态模式重试
                    </button>
                    <button type="button" onClick={() => void runPrepareInModal(modal.entry, "upgrade", modal.statePolicy, true)} className="btn-glass px-3 py-1 text-micro font-medium text-[var(--text)]">
                      使用新版默认配置重试
                    </button>
                  </div>
                )}
              </div>
            ) : (
              <>
                <PreparationDetailsBox
                  installPrep={modal.installPrep}
                  upgradePrep={modal.upgradePrep}
                  fingerprint={modal.installPrep?.fingerprint}
                  selectedCaps={modal.selectedCaps}
                  onToggleCap={(cap) => setModal((cur) => {
                    if (!cur) return null;
                    const next = new Set(cur.selectedCaps);
                    if (next.has(cap)) next.delete(cap);
                    else next.add(cap);
                    return { ...cur, selectedCaps: next };
                  })}
                />
                {modal.upgradePrep?.impact.requires_reauth && (
                  <div>
                    <label htmlFor="market-reauth-pw" className="mb-1 block text-caption font-medium text-[var(--text-muted)]">管理员密码二次验证（重置插件状态需要） *</label>
                    <input id="market-reauth-pw" type="password" required value={modal.reauthPassword} onChange={(e) => setModal((cur) => (cur ? { ...cur, reauthPassword: e.target.value } : null))} className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
                  </div>
                )}
                <label className="flex cursor-pointer items-center gap-2 text-caption text-[var(--text-muted)]">
                  <input type="checkbox" checked={modal.autoEnable} onChange={(e) => setModal((cur) => (cur ? { ...cur, autoEnable: e.target.checked } : null))} className="size-3.5 accent-[var(--accent-2)]" />
                  <span>完成后自动启用插件</span>
                </label>
              </>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" disabled={modal.preparing || modal.executing} onClick={() => setModal(null)} className="btn-glass px-4 py-2 text-sub">取消</button>
              {!modal.preparing && !modal.error && (
                <button type="submit" disabled={modal.executing || (Boolean(modal.upgradePrep?.impact.requires_reauth) && !modal.reauthPassword)} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-50">
                  {modal.executing ? "执行中…" : modal.action === "upgrade" ? "确认升级" : "确认安装"}
                </button>
              )}
            </div>
          </form>
        </Modal>
      )}
    </div>
  );
}
