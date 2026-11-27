"use client";

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import {
  disablePlugin,
  enablePlugin,
  ensureOperationSucceeded,
  listPlugins,
  type PluginSummaryView,
  waitOperationTerminal,
} from "@/lib/api/plugins";

const LIFECYCLE_BADGE: Record<string, string> = {
  enabled: "border-emerald-400/30 bg-emerald-400/15 text-emerald-300",
  disabled: "border-white/15 bg-white/[0.06] text-[var(--text-muted)]",
  failed: "border-[#ff6b6b]/30 bg-[#ff6b6b]/15 text-[#ff6b6b]",
  draining: "border-amber-400/30 bg-amber-400/15 text-amber-300",
};

export function PluginManagerView() {
  const toast = useToast();
  const confirm = useConfirm();
  const [plugins, setPlugins] = useState<PluginSummaryView[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      const res = await listPlugins();
      setPlugins(res.items);
    } catch (err) {
      setError((err as Error).message || "无法加载插件列表");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
      await load();
    } catch (err) {
      toast.error((err as Error).message || "操作失败");
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sub text-[var(--text-muted)]">
          管理内置与第三方插件的生命周期、配置表单、权限授予与 Federated UI 信任边界。
        </p>
        <div className="flex items-center gap-2">
          <Link to="/dashboard" className="btn-glass px-3.5 py-1.5 text-sub font-medium">
            插件工作台看板
          </Link>
          <button
            type="button"
            onClick={() => void load()}
            className="btn-glass px-3.5 py-1.5 text-sub font-medium"
          >
            刷新
          </button>
        </div>
      </div>

      {error && (
        <div role="alert" className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-sub text-[#ff6b6b]">
          {error}
        </div>
      )}

      {loading ? (
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">
          正在加载已安装插件…
        </div>
      ) : plugins.length === 0 ? (
        <div className="rounded-xl border border-white/10 bg-white/[0.03] p-6 text-sub text-[var(--text-muted)]">
          暂无已安装插件。
        </div>
      ) : (
        <div className="divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08] bg-white/[0.03]">
          {plugins.map((plugin) => {
            const badgeClass = LIFECYCLE_BADGE[plugin.lifecycle] ?? LIFECYCLE_BADGE.disabled;
            const busy = busyId === plugin.id;
            return (
              <div
                key={plugin.id}
                className="flex flex-wrap items-center justify-between gap-4 px-5 py-4 transition-colors hover:bg-white/[0.03]"
              >
                <div className="min-w-0 flex-1 space-y-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <Link
                      to={`/settings/plugins/${encodeURIComponent(plugin.id)}`}
                      className="text-body font-semibold text-[var(--text)] hover:underline"
                    >
                      {plugin.name}
                    </Link>
                    <span className={`rounded-full border px-2 py-0.5 text-micro font-medium ${badgeClass}`}>
                      {plugin.lifecycle}
                    </span>
                    <span className="rounded-full border border-white/10 bg-white/[0.04] px-2 py-0.5 text-micro text-[var(--text-muted)]">
                      {plugin.source} · v{plugin.version}
                    </span>
                  </div>
                  <p className="truncate text-caption text-[var(--text-faint)]">
                    {plugin.id} · generation #{plugin.generation} · rev #{plugin.revision} · health: {plugin.health}
                  </p>
                  {plugin.failure_reason && (
                    <p className="text-caption text-[#ff6b6b]">{plugin.failure_reason}</p>
                  )}
                </div>

                <div className="flex shrink-0 items-center gap-2">
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => void toggleLifecycle(plugin)}
                    className="btn-glass px-3 py-1.5 text-caption font-medium disabled:opacity-50"
                  >
                    {busy ? "处理中…" : plugin.enabled ? "停用" : "启用"}
                  </button>
                  <Link
                    to={`/settings/plugins/${encodeURIComponent(plugin.id)}`}
                    className="btn-accent rounded-full px-3.5 py-1.5 text-caption font-semibold"
                  >
                    管理
                  </Link>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
