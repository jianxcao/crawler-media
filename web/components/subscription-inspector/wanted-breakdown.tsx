"use client";

import { useState } from "react";

import { ChevronDownIcon } from "@/components/icons";
import type {
  SubscriptionActivity,
  SubscriptionDownload,
  WantedItem,
} from "@/lib/api/subscriptions";
import {
  defaultOpenSeasons,
  downloadNote,
  milestonesOf,
  SHORT_STATUS,
  stuckIndex,
  wantedPresentation,
  type Milestone,
} from "@/lib/subscription-inspector";

/**
 * 追踪项按季分组展开；电影是单项的退化形态。
 */
export function WantedBreakdown({
  wanted,
  isMovie,
  downloads,
  failures,
}: {
  wanted: WantedItem[];
  isMovie: boolean;
  downloads: Map<string, SubscriptionDownload>;
  failures: Map<string, SubscriptionActivity>;
}) {
  const [openSeasons, setOpenSeasons] = useState(() => defaultOpenSeasons(wanted));
  const toggleSeason = (season: number) => {
    setOpenSeasons((prev) => {
      const next = new Set(prev);
      if (next.has(season)) next.delete(season);
      else next.add(season);
      return next;
    });
  };
  const [openWanted, setOpenWanted] = useState<string | null>(null);
  if (wanted.length === 0) {
    return (
      <p className="css-glass rounded-2xl p-5 text-sub leading-6 text-[var(--text-muted)]">
        当前没有追踪项。开启「自动续订」后，新集播出会自动加入。
      </p>
    );
  }

  const seasons = new Map<number, WantedItem[]>();
  for (const w of wanted) {
    const list = seasons.get(w.season_number) ?? [];
    list.push(w);
    seasons.set(w.season_number, list);
  }
  const collapsible = !isMovie && seasons.size > 1;

  return (
    <div className="space-y-4">
      {[...seasons.entries()]
        .sort(([a], [b]) => b - a)
        .map(([season, items]) => {
          const open = !collapsible || openSeasons.has(season);
          const arranged = items.filter((w) => w.status !== "wanted").length;
          return (
            <div
              key={season}
              className="css-glass overflow-hidden rounded-2xl"
            >
              {!isMovie && (
                <div
                  role={collapsible ? "button" : undefined}
                  tabIndex={collapsible ? 0 : undefined}
                  aria-expanded={collapsible ? open : undefined}
                  onClick={collapsible ? () => toggleSeason(season) : undefined}
                  onKeyDown={
                    collapsible
                      ? (e) => {
                          if (e.key === "Enter" || e.key === " ") {
                            e.preventDefault();
                            toggleSeason(season);
                          }
                        }
                      : undefined
                  }
                  className={`flex items-center gap-2 px-5 py-2.5 text-sub font-semibold text-white/80 ${
                    open ? "border-b border-white/[0.06]" : ""
                  } ${
                    collapsible
                      ? "cursor-pointer transition-colors hover:bg-white/[0.035] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-white/25"
                      : ""
                  }`}
                >
                  {collapsible && (
                    <ChevronDownIcon
                      className={`size-4 shrink-0 text-[var(--text-faint)] transition-transform ${
                        open ? "rotate-180" : ""
                      }`}
                    />
                  )}
                  <span className="min-w-0 flex-1 truncate">
                    {isMovie ? "正片" : season === 0 ? "特别篇" : `第 ${season} 季`}
                    {!isMovie && (
                      <span
                        className="tnum ml-2 font-normal text-[var(--text-faint)]"
                        title={`${arranged}/${items.length} 已安排`}
                      >
                        {arranged}/{items.length}
                      </span>
                    )}
                    {items.some((w) => w.upgrade?.active) && (
                      <span className="tnum ml-2 font-normal text-[var(--ok)]">
                        洗版 {items.filter((w) => w.upgrade?.active).length}
                      </span>
                    )}
                  </span>
                  {collapsible && !open && (
                    <span className="h-1 w-12 shrink-0 overflow-hidden rounded-full bg-white/[0.1]">
                      <span
                        className="block h-full rounded-full"
                        style={{
                          width: `${Math.round((arranged / items.length) * 100)}%`,
                          backgroundColor: arranged === items.length ? "var(--ok)" : "var(--warn)",
                        }}
                      />
                    </span>
                  )}
                </div>
              )}
              {open && (
                <ul className="divide-y divide-white/[0.05]">
                  {items.map((w) => (
                    <WantedRow
                      key={w.id}
                      wanted={w}
                      isMovie={isMovie}
                      download={w.info_hash ? downloads.get(w.info_hash) : undefined}
                      failure={failures.get(w.id)}
                      expanded={openWanted === w.id}
                      onToggle={() => setOpenWanted((cur) => (cur === w.id ? null : w.id))}
                    />
                  ))}
                </ul>
              )}
            </div>
          );
        })}
    </div>
  );
}

function WantedRow({
  wanted: w,
  isMovie,
  download,
  failure,
  expanded,
  onToggle,
}: {
  wanted: WantedItem;
  isMovie: boolean;
  download?: SubscriptionDownload;
  failure?: SubscriptionActivity;
  expanded: boolean;
  onToggle: () => void;
}) {
  const stuckOnImport = failure?.type === "import_failed" ? failure : undefined;
  const { label, color, note: statusNote } = stuckOnImport
    ? { label: "入库失败", color: "var(--danger)", note: stuckOnImport.message }
    : wantedPresentation(w);
  const live =
    !stuckOnImport && (w.status === "grabbed" || w.status === "downloaded") ? download : undefined;
  const chain = milestonesOf(w, isMovie, live, failure);
  const lit = stuckIndex(chain);
  const complete = chain.every((m) => m.state === "done");
  const collapsible = !isMovie;
  const open = !collapsible || expanded;
  const headerClass = `block w-full px-5 py-2.5 text-left max-md:px-4 ${
    collapsible
      ? `cursor-pointer transition-colors hover:bg-white/[0.035] ${
          expanded ? "bg-white/[0.05]" : ""
        }`
      : ""
  }`;
  const header = (
    <>
      <span className="flex items-center gap-x-3 gap-y-1 max-md:flex-wrap">
        <span className="tnum w-11 shrink-0 text-sub font-semibold text-white/90">
          {isMovie ? "正片" : `E${String(w.episode_number).padStart(2, "0")}`}
        </span>
        <span className="tnum min-w-0 flex-1 truncate text-sub leading-5 text-[var(--text-muted)] max-md:order-last max-md:basis-full max-md:whitespace-normal max-md:line-clamp-2">
          {live ? downloadNote(live) : statusNote}
        </span>
        <span
          className="flex shrink-0 items-center gap-[7px] rounded-full px-2.5 py-[3px] text-micro font-semibold max-md:ml-auto"
          style={{
            backgroundColor: `color-mix(in oklab, ${color} 14%, transparent)`,
            color,
          }}
          title={`第 ${lit + 1}/${chain.length} 站：${chain[lit].lab}${complete ? "（已收齐）" : ""}`}
        >
          <span className="flex w-[42px] shrink-0 items-center gap-[3px]">
            {chain.map((m, i) => (
              <span
                key={m.lab}
                className="size-[4.5px] shrink-0 rounded-full"
                style={{
                  backgroundColor:
                    i === lit
                      ? color
                      : m.state === "done"
                        ? "color-mix(in oklab, var(--text) 38%, transparent)"
                        : "color-mix(in oklab, var(--text) 14%, transparent)",
                  boxShadow:
                    i === lit
                      ? `0 0 5px color-mix(in oklab, ${color} 60%, transparent)`
                      : undefined,
                }}
              />
            ))}
          </span>
          {SHORT_STATUS[label] ?? label}
        </span>
        {collapsible && (
          <ChevronDownIcon
            className={`size-3.5 shrink-0 text-white/30 transition-transform ${
              expanded ? "rotate-180 text-[var(--text-muted)]" : ""
            }`}
          />
        )}
      </span>
      {live && live.progress != null && live.state !== "missing" && (
        <span
          className="mt-2 block h-1 overflow-hidden rounded-full bg-white/[0.08]"
          role="progressbar"
          aria-valuenow={Math.round(live.progress * 100)}
          aria-valuemin={0}
          aria-valuemax={100}
        >
          <span
            className="block h-full rounded-full bg-[var(--ok)] transition-[width] duration-700"
            style={{ width: `${Math.min(100, Math.max(1, live.progress * 100))}%` }}
          />
        </span>
      )}
    </>
  );
  return (
    <li>
      {collapsible ? (
        <button
          type="button"
          aria-expanded={expanded}
          onClick={onToggle}
          className={headerClass}
        >
          {header}
        </button>
      ) : (
        <div className={headerClass}>{header}</div>
      )}
      {open && <MilestoneChain chain={chain} color={color} />}
    </li>
  );
}

function MilestoneChain({ chain, color }: { chain: Milestone[]; color: string }) {
  const stuck = stuckIndex(chain);
  return (
    <ol className="bg-white/[0.05] py-3 pl-[76px] pr-5 max-md:px-4">
      {chain.map((m, i) => {
        const isNow = m.state === "now" || m.state === "fail";
        return (
          <li key={m.lab} className="flex gap-3">
            <span className="flex flex-col items-center">
              <span
                className={`shrink-0 rounded-full ${
                  isNow ? "mt-1.5 size-2.5" : "mt-[7px] size-[9px]"
                } ${m.state === "done" ? "bg-white/30" : "border-[1.5px] border-white/[0.15]"}`}
                style={
                  isNow
                    ? {
                        borderColor: color,
                        borderWidth: 2.5,
                        boxShadow: `0 0 9px color-mix(in oklab, ${color} 40%, transparent)`,
                      }
                    : undefined
                }
              />
              {i < chain.length - 1 && (
                <span
                  className={`mt-1 w-[1.5px] flex-1 rounded-full ${
                    i < stuck ? "bg-white/[0.16]" : "bg-white/[0.07]"
                  }`}
                />
              )}
            </span>
            <div
              className={`flex min-w-0 flex-1 flex-wrap items-baseline gap-x-3 ${
                i === chain.length - 1 ? "pb-1" : "pb-3.5"
              }`}
            >
              <span
                className={`text-sub ${
                  isNow
                    ? "font-semibold"
                    : m.state === "done"
                      ? "font-medium text-white/65"
                      : "font-medium text-[var(--text-faint)]"
                }`}
                style={isNow ? { color } : undefined}
              >
                {m.lab}
              </span>
              {m.time && (
                <span className="tnum ml-auto shrink-0 text-caption text-[var(--text-faint)]">
                  {m.time}
                </span>
              )}
              <span
                className={`tnum basis-full text-sub leading-6 ${
                  m.state === "done"
                    ? "text-[var(--text-faint)]"
                    : m.state === "todo"
                      ? "text-white/25"
                      : "text-white/80"
                }`}
              >
                {m.det}
              </span>
              {m.why && (
                <span className="basis-full text-caption leading-5 text-[var(--danger)]">
                  {m.why}
                </span>
              )}
              {m.src?.map((s) => (
                <span
                  key={s}
                  className="basis-full truncate text-caption leading-5 text-[var(--text-faint)]"
                >
                  {s}
                </span>
              ))}
            </div>
          </li>
        );
      })}
    </ol>
  );
}
