"use client";

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { PluginSurfaceHost } from "@/components/plugins/PluginSurfaceHost";
import { fetchPluginUiNavigation, type UiNavigationItemView } from "@/lib/api/plugins";
import { useSession } from "@/lib/session";
import { useTheme } from "@/lib/ui-prefs";

function pinStorageKey(userId: string): string {
  return `cm_plugin_dashboard_pins:${userId}`;
}

function readPinnedKeys(userId: string): string[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(pinStorageKey(userId));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function writePinnedKeys(userId: string, keys: string[]): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(pinStorageKey(userId), JSON.stringify(keys));
  } catch {
    /* ignore storage failures */
  }
}

function surfaceKey(item: Pick<UiNavigationItemView, "plugin_id" | "surface_id">): string {
  return `${item.plugin_id}:${item.surface_id}`;
}

export function PluginDashboardSlot() {
  const { session } = useSession();
  const toast = useToast();
  const confirm = useConfirm();
  const theme = useTheme();
  const [available, setAvailable] = useState<UiNavigationItemView[]>([]);
  const [pinned, setPinned] = useState<string[]>(() => readPinnedKeys(session.id));
  const [managing, setManaging] = useState(false);

  useEffect(() => {
    setAvailable([]);
    const ac = new AbortController();
    fetchPluginUiNavigation(ac.signal)
      .then((items) => {
        if (!ac.signal.aborted) {
          setAvailable(
            items.filter((i) => i.kind === "dashboard" && i.subject_user_id === session.id),
          );
        }
      })
      .catch(() => {
        if (!ac.signal.aborted) {
          setAvailable([]);
        }
      });
    return () => ac.abort();
  }, [session.id]);

  useEffect(() => {
    setPinned(readPinnedKeys(session.id));
  }, [session.id]);

  function togglePin(key: string) {
    setPinned((prev) => {
      const next = prev.includes(key) ? prev.filter((k) => k !== key) : [...prev, key];
      writePinnedKeys(session.id, next);
      return next;
    });
  }

  const pinnedItems = available.filter((item) => pinned.includes(surfaceKey(item)));

  return (
    <div className="scroll-thin scroll-safe h-full overflow-y-auto">
      <div className="mx-auto max-w-6xl space-y-6 px-6 pb-20 pt-10 max-md:px-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h1 className="text-[22px] font-semibold text-[var(--text)]">插件工作台看板</h1>
            <p className="mt-1 text-sub text-[var(--text-muted)]">
              按需固定已信任插件的看板卡片，不会因安装而自动执行未添加的插件脚本。
            </p>
          </div>
          <div className="flex items-center gap-2">
            <Link to="/settings/plugins" className="btn-glass px-3.5 py-1.5 text-sub font-medium">
              插件管理
            </Link>
            <button
              type="button"
              onClick={() => setManaging((v) => !v)}
              className="btn-accent rounded-full px-4 py-1.5 text-sub font-semibold"
            >
              {managing ? "完成编辑" : "+ 添加看板卡片"}
            </button>
          </div>
        </div>

        {managing && (
          <div className="space-y-2 rounded-xl border border-white/10 bg-white/[0.03] p-4">
            <h2 className="text-sub font-semibold text-[var(--text)]">可用插件看板组件</h2>
            {available.length === 0 ? (
              <p className="text-caption text-[var(--text-faint)]">暂无已信任且启用的 dashboard 卡片。</p>
            ) : (
              <div className="flex flex-wrap gap-2">
                {available.map((item) => {
                  const key = surfaceKey(item);
                  const active = pinned.includes(key);
                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => togglePin(key)}
                      className={`rounded-lg border px-3 py-1.5 text-caption font-medium transition ${
                        active
                          ? "border-[var(--accent)] bg-[var(--accent)]/20 text-[var(--text)]"
                          : "border-white/10 bg-white/[0.04] text-[var(--text-muted)]"
                      }`}
                    >
                      {active ? "✓ " : "+ "}
                      {item.plugin_name} · {item.title}
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        )}

        {pinnedItems.length === 0 ? (
          <div className="rounded-xl border border-white/10 bg-white/[0.03] p-8 text-center text-sub text-[var(--text-muted)]">
            尚未固定任何看板卡片。点击右上角「+ 添加看板卡片」选择需要展示的插件状态卡。
          </div>
        ) : (
          <div className="grid gap-5 md:grid-cols-2">
            {pinnedItems.map((item) => {
              const key = surfaceKey(item);
              return (
                <section
                  key={key}
                  className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5"
                >
                  <div className="flex items-center justify-between gap-2">
                    <div>
                      <h2 className="text-body font-semibold text-[var(--text)]">{item.title}</h2>
                      <p className="text-caption text-[var(--text-faint)]">{item.plugin_name}</p>
                    </div>
                    <button
                      type="button"
                      onClick={() => togglePin(key)}
                      className="text-caption text-[var(--text-faint)] hover:text-[var(--text)]"
                    >
                      移除
                    </button>
                  </div>
                  <PluginSurfaceHost
                    pluginId={item.plugin_id}
                    surfaceId={item.surface_id}
                    expectedKind="dashboard"
                    deps={{
                      theme: theme.id,
                      notifyToast: (msg) =>
                        msg.level === "error" ? toast.error(msg.message) : toast.info(msg.message),
                      openConfirm: (c) =>
                        confirm({ title: c.title, description: c.message, confirmLabel: c.confirmLabel }),
                    }}
                  />
                </section>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
