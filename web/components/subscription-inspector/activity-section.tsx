"use client";

import { useState } from "react";

import { ChevronDownIcon } from "@/components/icons";
import type { SubscriptionActivity, WantedItem } from "@/lib/api/subscriptions";
import { activityColor } from "@/lib/subscription-inspector";
import { formatDateTime, formatRelativeTime } from "@/lib/time";

/**
 * 搜索轮次摘要行。
 */
export function SearchRoundBar({
  activities,
  wanted,
}: {
  activities: SubscriptionActivity[];
  wanted: WantedItem[];
}) {
  const latest = activities.find((a) => a.type === "searched");
  if (!latest) return null;

  const p = latest.payload as Record<string, unknown>;
  const hasStats = typeof p.results === "number" && !p.failed;
  const nextDue = wanted
    .filter((w) => w.status === "wanted" && w.next_search_at)
    .map((w) => w.next_search_at as string)
    .sort()[0];

  return (
    <div className="css-glass rounded-2xl">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4 py-2.5">
        <span
          className="size-1.5 shrink-0 rounded-full bg-[var(--info)]"
          style={{ boxShadow: "0 0 0 3px color-mix(in oklab, var(--info) 14%, transparent)" }}
        />
        <span className="text-sub font-medium text-white/85">
          {formatRelativeTime(latest.created_at)}搜了一轮
        </span>
        <span className="tnum basis-full text-caption leading-5 text-[var(--text-faint)]">
          {hasStats ? (
            <>
              {String(p.results)} 结果 · 命中 {String(p.identity_hits ?? 0)} · 拒绝{" "}
              {String(p.rejected ?? 0)} · 投递 {String(p.dispatched_units ?? 0)} 个单元
              {nextDue ? ` · 下轮 ${formatDateTime(nextDue)}` : ""}
            </>
          ) : (
            latest.message
          )}
        </span>
      </div>
    </div>
  );
}

/**
 * 排查记录：全量活动的折叠区，默认收起。
 */
export function ActivityLogSection({ activities }: { activities: SubscriptionActivity[] }) {
  const [open, setOpen] = useState(false);
  if (activities.length === 0) return null;
  return (
    <div className="css-glass overflow-hidden rounded-2xl">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-4 py-2.5 text-left text-sub font-semibold text-white/70 transition-colors hover:bg-white/[0.035]"
      >
        排查记录
        <span className="font-normal text-caption text-[var(--text-faint)] max-md:hidden">
          按时间回放系统做过的每一步
        </span>
        <span className="tnum font-normal text-[var(--text-faint)]">{activities.length}</span>
        <ChevronDownIcon
          className={`ml-auto size-4 shrink-0 text-[var(--text-faint)] transition-transform ${
            open ? "rotate-180" : ""
          }`}
        />
      </button>
      {open && (
        <div className="border-t border-white/[0.06]">
          <ActivityTimeline activities={activities} bare />
        </div>
      )}
    </div>
  );
}

function ActivityTimeline({
  activities,
  bare = false,
}: {
  activities: SubscriptionActivity[];
  bare?: boolean;
}) {
  if (activities.length === 0) {
    return (
      <p className="css-glass rounded-2xl p-5 text-sub leading-6 text-[var(--text-muted)]">
        暂无活动记录。系统开始搜索、匹配或投递后，每个动作都会记录在这里。
      </p>
    );
  }
  return (
    <div
      className={
        bare
          ? "px-6 py-5 max-md:px-4"
          : "css-glass rounded-2xl px-6 py-5"
      }
    >
      <ol>
        {activities.map((a, i) => {
          const last = i === activities.length - 1;
          const color = activityColor(a.type);
          return (
            <li key={a.id} className="flex gap-4">
              <div className="flex flex-col items-center">
                <span
                  className="mt-[7px] size-2 shrink-0 rounded-full"
                  style={{
                    backgroundColor: color,
                    boxShadow: `0 0 8px color-mix(in oklab, ${color} 33%, transparent)`,
                  }}
                />
                {!last && <span className="mt-1.5 w-px flex-1 bg-white/[0.08]" />}
              </div>
              <div
                className={`flex min-w-0 flex-1 items-baseline gap-5 ${last ? "" : "pb-5"}`}
              >
                <p className="min-w-0 flex-1 text-ui leading-6 text-white/85">{a.message}</p>
                <span
                  className="tnum shrink-0 text-caption text-[var(--text-faint)]"
                  title={formatDateTime(a.created_at)}
                >
                  {formatRelativeTime(a.created_at)}
                </span>
              </div>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
