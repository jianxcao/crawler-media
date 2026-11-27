"use client";

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { PluginSurfaceHost, type PluginSurfaceHostDeps } from "@/components/plugins/PluginSurfaceHost";
import { useTheme } from "@/lib/ui-prefs";

interface ShellActionItem {
  id: string;
  label: string;
  onSelect: () => void;
}

export function PluginAppPage({
  pluginId,
  surfaceId,
  deps,
}: {
  pluginId: string;
  surfaceId: string;
  deps?: PluginSurfaceHostDeps;
}) {
  const toast = useToast();
  const confirm = useConfirm();
  const theme = useTheme();
  const [title, setTitle] = useState(`${pluginId} · ${surfaceId}`);
  const [shellActions, setShellActions] = useState<ShellActionItem[]>([]);
  const [shellTabs, setShellTabs] = useState<readonly ShellActionItem[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);

  useEffect(() => {
    setTitle(`${pluginId} · ${surfaceId}`);
    setActiveTabId(null);
  }, [pluginId, surfaceId]);

  return (
    <div className="scroll-thin scroll-safe h-full overflow-y-auto">
      <div className="mx-auto max-w-6xl space-y-5 px-6 pb-20 pt-8 max-md:px-4">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-white/10 pb-4">
          <div className="min-w-0">
            <p className="text-caption text-[var(--text-faint)]">插件应用 · {pluginId}</p>
            <h1 className="truncate text-[20px] font-semibold text-[var(--text)]">{title}</h1>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            {shellActions.map((act) => (
              <button
                key={act.id}
                type="button"
                onClick={() => act.onSelect()}
                className="btn-glass px-3.5 py-1.5 text-sub font-medium"
              >
                {act.label}
              </button>
            ))}
            <Link
              to={`/settings/plugins/${encodeURIComponent(pluginId)}`}
              className="btn-glass px-3.5 py-1.5 text-sub font-medium"
            >
              插件设置与详情
            </Link>
          </div>
        </div>

        {shellTabs.length > 0 && (
          <div className="flex flex-wrap gap-2 border-b border-white/10 pb-3" role="tablist">
            {shellTabs.map((tabItem) => {
              const selected = (activeTabId ?? shellTabs[0]?.id) === tabItem.id;
              return (
                <button
                  key={tabItem.id}
                  type="button"
                  role="tab"
                  aria-selected={selected}
                  onClick={() => {
                    setActiveTabId(tabItem.id);
                    tabItem.onSelect();
                  }}
                  className={`rounded-full px-3.5 py-1.5 text-sub font-medium transition ${
                    selected
                      ? "bg-[var(--accent)] text-white"
                      : "bg-white/[0.05] text-[var(--text-muted)] hover:bg-white/[0.09] hover:text-[var(--text)]"
                  }`}
                >
                  {tabItem.label}
                </button>
              );
            })}
          </div>
        )}

        <PluginSurfaceHost
          pluginId={pluginId}
          surfaceId={surfaceId}
          expectedKind="app"
          onTitle={setTitle}
          onRegisterActions={setShellActions}
          onRegisterTabs={setShellTabs}
          deps={{
            theme: theme.id,
            notifyToast: (msg) =>
              msg.level === "error" ? toast.error(msg.message) : toast.info(msg.message),
            openConfirm: (c) =>
              confirm({ title: c.title, description: c.message, confirmLabel: c.confirmLabel }),
            ...deps,
          }}
        />
      </div>
    </div>
  );
}
