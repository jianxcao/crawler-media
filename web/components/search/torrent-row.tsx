"use client";

import { memo, useState } from "react";

import {
  DownloadButton,
  GrabButton,
  TorrentActionsSheet,
} from "@/components/search/torrent-actions";
import type { Release, TorrentHit } from "@/lib/api/search";
import { formatDateTime, formatRelativeTime } from "@/lib/time";
import {
  parsedTitleOf,
  seasonEpLabel,
  seederTone,
  specSummary,
  torrentPromoBadges,
  torrentSizeLabel,
} from "@/lib/torrent-search-view";

/** 每行常显的属性徽标上限；超出部分折叠成 +N（悬停看全部）。 */
const MAX_ATTR_BADGES = 4;

/**
 * 行布局采用「左弹性标题区 + 右固定数字网格 + 操作区」三段式，
 * 列表静止时收起操作区，hover/focus-within 时浮现；移动端走底部操作抽屉。
 */
export const TorrentRow = memo(function TorrentRow({
  hit,
  grouped = false,
  showRawTitles = false,
}: {
  hit: TorrentHit;
  grouped?: boolean;
  /** 列表模式直接展示站点原始种子名，不使用解析出的片名。 */
  showRawTitles?: boolean;
}) {
  const [actionsOpen, setActionsOpen] = useState(false);
  const size = torrentSizeLabel(hit.size_bytes);
  const parsedTitle = showRawTitles ? null : parsedTitleOf(hit);
  const asSpecRow = grouped && parsedTitle !== null;
  const spec = asSpecRow ? specSummary(hit.release) : null;
  const seeders = hit.seeders ?? 0;
  const leechers = hit.leechers ?? 0;
  const snatched = hit.snatched ?? 0;

  return (
    <li className="card-row-surface card-row-surface--interactive group relative rounded-2xl px-4 py-3.5 transition-all [contain-intrinsic-size:auto_72px] [content-visibility:auto] hover:-translate-y-px">
      {(hit.detail_url || hit.enclosure) && (
        <button
          type="button"
          aria-label={`打开「${hit.title}」的资源操作`}
          onClick={() => setActionsOpen(true)}
          className="absolute inset-0 z-10 hidden cursor-pointer rounded-2xl max-md:block [@media(hover:none)]:block"
        />
      )}
      <div className="flex items-center gap-5">
        <div className="min-w-0 flex-1">
          {asSpecRow ? (
            <p
              title={hit.title}
              className="truncate text-ui font-medium leading-5 text-[var(--text)]"
            >
              {spec ?? hit.title}
            </p>
          ) : parsedTitle ? (
            <p
              title={hit.title}
              className="truncate text-ui font-medium leading-5 text-[var(--text)]"
            >
              {parsedTitle}
              {hit.release.year != null && (
                <span className="tnum ml-2 text-sub font-normal text-[var(--text-faint)]">
                  {hit.release.year}
                </span>
              )}
            </p>
          ) : (
            <p className="truncate text-ui font-medium leading-5 text-[var(--text)]">
              {hit.title}
            </p>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-1.5">
            <span className="shrink-0 rounded-md bg-white/[0.06] px-1.5 py-0.5 text-micro font-medium text-[var(--accent-2)]">
              {hit.site_name}
            </span>
            <PromoBadges hit={hit} />
            {!asSpecRow && <AttrBadges release={hit.release} />}
          </div>
        </div>

        <div className="tnum hidden shrink-0 grid-cols-[70px_78px_78px] gap-x-3 text-right text-caption lg:grid">
          <Metric label="大小" value={size ?? "—"} />
          <Metric
            label="做种 / 下载"
            value={
              <>
                <span className={seederTone(seeders)}>{seeders}</span>
                <span className="text-[var(--text-faint)]"> / {leechers}</span>
              </>
            }
          />
          <Metric
            label={snatched > 0 ? `完成 ${snatched}` : "发布时间"}
            value={hit.upload_time ? formatRelativeTime(hit.upload_time) : "—"}
            title={hit.upload_time ? formatDateTime(hit.upload_time) : undefined}
          />
        </div>

        {(hit.detail_url || hit.enclosure) && (
          <div className="pointer-events-none absolute inset-y-0 right-2 flex items-center gap-1.5 rounded-r-2xl bg-gradient-to-l from-[var(--row-fade-bg)] via-[var(--row-fade-bg)] via-65% to-transparent pl-16 pr-2 opacity-0 transition-opacity duration-150 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 max-md:hidden [@media(hover:none)]:hidden">
            {hit.detail_url && (
              <a
                href={hit.detail_url}
                target="_blank"
                rel="noreferrer"
                className="btn-glass h-7 px-3 text-caption text-[var(--text-muted)]"
              >
                详情
              </a>
            )}
            <GrabButton
              hit={hit}
              className="flex h-7 items-center rounded-full border border-[var(--info)]/40 bg-[var(--info)]/15 px-3 text-caption font-medium text-[var(--info)] transition-colors hover:bg-[var(--info)]/25"
            />
            <DownloadButton
              hit={hit}
              className="btn-accent flex h-7 items-center rounded-full px-3 text-caption font-medium"
            />
          </div>
        )}
      </div>
      <TorrentActionsSheet
        hit={hit}
        open={actionsOpen}
        onClose={() => setActionsOpen(false)}
      />
    </li>
  );
});

/** 列表指标单元：固定标签 + 数值的两级层次。 */
export function Metric({
  label,
  value,
  title,
}: {
  label: string;
  value: React.ReactNode;
  title?: string;
}) {
  return (
    <span title={title} className="flex min-w-0 flex-col gap-0.5">
      <span className="truncate text-micro text-[var(--text-faint)]">{label}</span>
      <span className="truncate text-[var(--text-muted)]">{value}</span>
    </span>
  );
}

/** 促销角标：免费 / H&R 考核。 */
export function PromoBadges({ hit }: { hit: TorrentHit }) {
  const badges = torrentPromoBadges(hit, "row");
  if (!badges.length) return null;
  return (
    <>
      {badges.map((b) => (
        <span
          key={b.text}
          className={`rounded-md px-1.5 py-0.5 text-micro font-semibold ${b.cls}`}
        >
          {b.text}
        </span>
      ))}
    </>
  );
}

/** 标题解析属性徽标：按信息量优先级取前 4 个常显，其余折叠成 +N。 */
export function AttrBadges({ release }: { release: Release }) {
  const chips: { key: string; text: string; cls?: string }[] = [];
  const seasonEp = seasonEpLabel(release);
  if (seasonEp) chips.push({ key: "se", text: seasonEp, cls: "text-[var(--accent-2)]" });
  if (release.resolution) chips.push({ key: "res", text: release.resolution });
  if (release.hdr) chips.push({ key: "hdr", text: release.hdr, cls: "text-[#c8a6ff]" });
  if (release.group) chips.push({ key: "group", text: release.group, cls: "text-[var(--accent)]" });
  if (release.source) chips.push({ key: "source", text: release.source });
  if (release.codec) chips.push({ key: "codec", text: release.codec });
  if (!chips.length) return null;

  const shown = chips.slice(0, MAX_ATTR_BADGES);
  const folded = chips.slice(MAX_ATTR_BADGES);
  return (
    <>
      {shown.map((c) => (
        <span
          key={c.key}
          className={`rounded-md bg-white/[0.05] px-1.5 py-0.5 text-micro ${c.cls ?? "text-[var(--text-muted)]"}`}
        >
          {c.text}
        </span>
      ))}
      {folded.length > 0 && (
        <span
          title={folded.map((c) => c.text).join(" · ")}
          className="tnum cursor-default rounded-md bg-white/[0.05] px-1.5 py-0.5 text-micro text-[var(--text-faint)]"
        >
          +{folded.length}
        </span>
      )}
    </>
  );
}
