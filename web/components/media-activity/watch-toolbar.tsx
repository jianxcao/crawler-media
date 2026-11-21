"use client";

import type { ReactNode } from "react";

import type { MediaActivityScope } from "@/lib/api/playback";
import { WATCH_VIEW_LABELS, type WatchViewName } from "@/lib/task-center";

export const SCOPE_OPTIONS: readonly { value: MediaActivityScope; label: string; hint: string }[] = [
  { value: "visible", label: "我的浏览范围", hint: "对自己隐藏的库只报个数，不出片名" },
  { value: "all", label: "全部", hint: "跨成员、跨库，管理视角" },
] as const;

/** 「观看」视角工具栏：左侧三切片，右侧作用范围与筛选器。 */
export function WatchToolbar({
  view,
  onViewChange,
  liveCount,
  children,
}: {
  view: WatchViewName;
  onViewChange: (view: WatchViewName) => void;
  liveCount: number;
  children: ReactNode;
}) {
  return (
    <div className="mt-6 flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-white/[0.08] max-md:mt-5">
      <div className="scroll-thin flex min-w-0 flex-1 gap-1 overflow-x-auto pb-2 max-md:basis-full">
        {WATCH_VIEW_LABELS.map((item) => (
          <button
            key={item.id}
            type="button"
            aria-pressed={view === item.id}
            onClick={() => onViewChange(item.id)}
            className={`shrink-0 rounded-full px-3.5 py-1.5 text-ui font-medium transition ${
              view === item.id
                ? "bg-white/[0.14] text-white"
                : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-white"
            }`}
          >
            {item.label}
            {item.id === "playing" && liveCount > 0 && (
              <span className="tnum ml-1.5 text-caption text-white/45">{liveCount}</span>
            )}
          </button>
        ))}
      </div>
      <div className="scroll-thin flex shrink-0 items-center gap-1.5 pb-2 max-md:min-w-0 max-md:basis-full max-md:overflow-x-auto">
        {children}
      </div>
    </div>
  );
}
