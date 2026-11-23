"use client";

import type { ReactNode } from "react";
import { Link } from "react-router-dom";

import { FilmIcon, MoreIcon, RefreshIcon, SearchIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import type { UpNextItem } from "@/lib/api/playback";
import type {
  ReleaseForecastDay,
  SubscriptionDetail,
  SubscriptionStatus,
} from "@/lib/api/subscriptions";
import { imageUrl } from "@/lib/image-proxy";
import { playHref } from "@/lib/player/play-links";
import { weekdayLabel } from "@/lib/subscription-inspector";
import { formatDateTime } from "@/lib/time";

import { SubscriptionManageMenu } from "./subscription-manage-controls";

export function SubscriptionHeroCard({
  detail,
  detailStatus,
  metaColor,
  statusLabel,
  isMovie,
  poster,
  upgradingCount,
  ruleSetName,
  searchJob,
  busy,
  canSubscribe,
  canManageSubscriptions,
  canSearch,
  upNext,
  forecast,
  seasonMarking,
  onToggleKeepOldVersions,
  onFetchModeChange,
  onSearchIntervalChange,
  onMarkWholeSeason,
  onSearchNow,
  onAdjust,
  onUpgradeRun,
  onToggleFollowFuture,
  onSwitchRule,
  onTogglePause,
  onRemove,
  onOpenManageSheet,
}: {
  detail: SubscriptionDetail;
  detailStatus: SubscriptionStatus;
  metaColor: string;
  statusLabel: string;
  isMovie: boolean;
  poster: string | null;
  upgradingCount: number;
  ruleSetName: string;
  searchJob: { next_run_after?: string | number | null } | null;
  busy: boolean;
  canSubscribe: boolean;
  canManageSubscriptions: boolean;
  canSearch: boolean;
  upNext: UpNextItem | null;
  forecast: ReleaseForecastDay[] | null;
  seasonMarking: boolean;
  onToggleKeepOldVersions: () => void;
  onFetchModeChange: (mode: "search" | "rss" | "both") => void;
  onSearchIntervalChange: (secs: number) => void;
  onMarkWholeSeason: (played: boolean) => void;
  onSearchNow: () => void;
  onAdjust: () => void;
  onUpgradeRun: () => void;
  onToggleFollowFuture: () => void;
  onSwitchRule: () => void;
  onTogglePause: () => void;
  onRemove: () => void;
  onOpenManageSheet: () => void;
}) {
  return (
    <section className="sub-hero-card relative mt-7 overflow-hidden rounded-2xl bg-[#0d111b] shadow-[0_24px_70px_-18px_rgba(0,0,0,0.58)] ring-1 ring-white/10 max-md:mt-4">
      {poster && (
        <PosterImage
          src={poster}
          alt=""
          className="absolute inset-0 size-full scale-110 object-cover opacity-25 blur-3xl brightness-[0.42] saturate-[0.9]"
        />
      )}
      <div className="sub-hero-tint absolute inset-0 bg-gradient-to-r from-[rgba(7,10,17,0.97)] via-[rgba(9,13,22,0.91)] to-[rgba(10,14,23,0.84)]" />

      <div className="relative z-10 flex items-center justify-between gap-4 px-6 pt-5 max-md:px-4 max-md:pt-4">
        <p className="text-caption font-semibold tracking-[0.2em] text-[var(--accent-2)]">
          {isMovie ? "电影订阅" : "剧集订阅"}
        </p>
        <span className="flex shrink-0 items-center gap-2 text-sub text-white/65">
          <span
            className="size-1.5 rounded-full shadow-[0_0_0_4px_rgba(255,255,255,0.05)]"
            style={{ backgroundColor: metaColor }}
          />
          {statusLabel}
        </span>
      </div>

      <div className="relative z-10 grid grid-cols-[112px_minmax(0,1fr)] gap-5 px-6 pb-6 pt-4 max-md:grid-cols-[80px_minmax(0,1fr)] max-md:gap-4 max-md:px-4 max-md:pb-4 max-md:pt-3">
        <Link
          to={`/media/${detail.media.kind}/${detail.media.tmdb_id}`}
          aria-label={`查看《${detail.media.title}》详情`}
          className="block w-28 shrink-0 self-start overflow-hidden rounded-xl bg-white/[0.05] shadow-[0_16px_40px_rgba(0,0,0,0.45)] ring-1 ring-white/15 max-md:w-20 max-md:rounded-lg"
        >
          {poster ? (
            <PosterImage
              src={poster}
              alt={`${detail.media.title} 海报`}
              className="aspect-[2/3] w-full object-cover"
            />
          ) : (
            <span className="flex aspect-[2/3] flex-col items-center justify-center gap-2 text-caption text-white/35">
              <FilmIcon className="size-6" />
              暂无海报
            </span>
          )}
        </Link>

        <div className="contents md:flex md:min-w-0 md:flex-col">
          <div className="min-w-0 self-center md:self-auto">
            <h1 className="flex min-w-0 items-baseline gap-2.5 text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[20px]">
              <Link
                to={`/media/${detail.media.kind}/${detail.media.tmdb_id}`}
                className="min-w-0 truncate hover:underline"
              >
                {detail.media.title}
              </Link>
              <span className="tnum shrink-0 text-body font-normal text-white/45">
                {detail.media.year ?? ""}
              </span>
            </h1>
            <p className="mt-2 flex flex-wrap gap-x-2 gap-y-1 text-sub text-white/55">
              {detail.media.original_title &&
                detail.media.original_title !== detail.media.title && (
                  <span className="truncate">{detail.media.original_title}</span>
                )}
              <span className="shrink-0">订阅于 {formatDateTime(detail.created_at)}</span>
            </p>
          </div>

          <div className="mt-4 grid grid-cols-[repeat(auto-fit,minmax(80px,1fr))] gap-3 border-y border-white/[0.08] py-3 max-md:col-span-2 max-md:mt-1">
            <SubscriptionFact
              label="收录范围"
              value={
                isMovie
                  ? "正片"
                  : detail.coverage.kind === "tv"
                    ? `第 ${detail.coverage.season} 季`
                    : "未勾选季"
              }
            />
            {!isMovie && (
              <SubscriptionFact
                label="自动续订"
                value={detail.follow_future ? "已开启" : "已关闭"}
              />
            )}
            {detail.wash_cut && (
              <SubscriptionFact
                label="保留旧版本"
                value={
                  <button
                    type="button"
                    disabled={busy}
                    onClick={onToggleKeepOldVersions}
                    className="rounded-full border border-white/10 px-2.5 py-1 text-caption font-medium text-white/75 transition hover:bg-white/[0.08] disabled:opacity-40"
                  >
                    {detail.keep_old_versions ? "开启（新旧并存）" : "关闭（替换即删）"}
                  </button>
                }
              />
            )}
            {canManageSubscriptions && <SubscriptionFact label="规则组" value={ruleSetName} />}
            {canSubscribe && (
              <label className="flex flex-col gap-1 text-sub text-white/55">
                搜种模式
                <select
                  className="h-9 rounded-lg border border-white/10 bg-white/[0.04] px-2 text-ui text-white"
                  value={detail.fetch_mode}
                  disabled={busy}
                  onChange={(event) =>
                    onFetchModeChange(event.target.value as "search" | "rss" | "both")
                  }
                >
                  <option value="search">站点搜索</option>
                  <option value="rss">RSS</option>
                  <option value="both">搜索 + RSS</option>
                </select>
              </label>
            )}
            <SubscriptionFact
              label="下次搜索"
              value={
                detail.tracking_state === "paused"
                  ? "已暂停"
                  : searchJob?.next_run_after
                    ? formatDateTime(searchJob.next_run_after)
                    : "排队中"
              }
            />
            {canSubscribe && (
              <label className="flex flex-col gap-1 text-sub text-white/55">
                搜索间隔
                <select
                  className="h-9 rounded-lg border border-white/10 bg-white/[0.04] px-2 text-ui text-white"
                  value={String(detail.search_interval_secs || 1800)}
                  disabled={busy}
                  onChange={(event) => onSearchIntervalChange(Number(event.target.value))}
                >
                  <option value="900">15 分钟</option>
                  <option value="1800">30 分钟</option>
                  <option value="3600">1 小时</option>
                  <option value="21600">6 小时</option>
                  <option value="86400">24 小时</option>
                </select>
              </label>
            )}
          </div>

          {upNext && (
            <div className="mt-4 flex items-center gap-3 rounded-2xl border border-white/[0.08] bg-white/[0.03] p-3">
              <div className="relative h-20 w-14 shrink-0 overflow-hidden rounded-lg bg-white/[0.05] ring-1 ring-white/10">
                <PosterImage
                  src={imageUrl(upNext.poster_url ?? detail.media.poster_url ?? null, "poster-card")}
                  alt={upNext.title}
                  className="size-full object-cover"
                  fallback={
                    <div className="flex size-full flex-col items-center justify-center text-white/30">
                      <FilmIcon className="size-5" />
                    </div>
                  }
                />
              </div>
              <div className="min-w-0 flex-1">
                <p className="text-caption text-white/45">
                  下一集 · 第 {upNext.episode_number} 集
                  {upNext.episode_title ? ` · ${upNext.episode_title}` : ""}
                </p>
                <p className="mt-0.5 truncate text-sub font-semibold text-white/90">{upNext.title}</p>
                {upNext.progress_percent != null && (
                  <div className="mt-1.5 h-1 w-full overflow-hidden rounded-full bg-white/10">
                    <div
                      className="h-full rounded-full bg-[var(--accent)]"
                      style={{
                        width: `${Math.min(100, Math.max(0, upNext.progress_percent))}%`,
                      }}
                    />
                  </div>
                )}
              </div>
              <Link
                to={playHref(detail.media.id, {
                  season: upNext.season_number,
                  episode: upNext.episode_number,
                  tSeconds: upNext.position_ms > 0 ? Math.floor(upNext.position_ms / 1000) : undefined,
                })}
                className="btn-accent inline-flex h-9 shrink-0 items-center gap-1 rounded-full px-3.5 text-sub font-semibold"
              >
                {upNext.position_ms > 0 ? "继续看" : "播放"}
              </Link>
            </div>
          )}

          {forecast && forecast.length > 0 && (
            <div className="mt-4 rounded-2xl border border-white/[0.08] bg-white/[0.03] p-4">
              <p className="text-caption font-semibold text-white/70">未来 7 天发布预估</p>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {forecast.map((day) => (
                  <span
                    key={day.date}
                    className="rounded-lg border border-white/10 bg-white/[0.05] px-2 py-1 text-caption text-white/75"
                    title={day.date}
                  >
                    {weekdayLabel(day.weekday)} · {day.count} 部
                  </span>
                ))}
              </div>
              <p className="mt-1.5 text-micro text-white/35">
                按近期命中种子的上传时间星期分布预估
              </p>
            </div>
          )}

          {detail.coverage.kind === "tv" && (
            <div className="mt-2 flex flex-wrap gap-1.5">
              <button
                type="button"
                disabled={seasonMarking}
                onClick={() => onMarkWholeSeason(true)}
                className="rounded-full border border-white/10 px-2.5 py-1 text-caption font-medium text-white/70 transition hover:bg-white/[0.08] disabled:opacity-40"
              >
                整季已看
              </button>
              <button
                type="button"
                disabled={seasonMarking}
                onClick={() => onMarkWholeSeason(false)}
                className="rounded-full border border-white/10 px-2.5 py-1 text-caption font-medium text-white/70 transition hover:bg-white/[0.08] disabled:opacity-40"
              >
                整季未看
              </button>
            </div>
          )}

          <ProgressStrip
            progress={detail.progress}
            unitLabel={isMovie ? "部" : "集"}
            upgrading={upgradingCount}
          />

          <div className="mt-auto flex flex-wrap justify-end gap-2 pt-4 max-md:col-span-2 max-md:grid max-md:grid-flow-col max-md:auto-cols-fr max-md:pt-3">
            {canSubscribe && detail.progress.missing > 0 && detailStatus !== "paused" && (
              <button
                type="button"
                disabled={busy}
                onClick={onSearchNow}
                className="btn-accent inline-flex h-10 min-w-0 items-center justify-center gap-1.5 rounded-full px-4 text-sub font-semibold disabled:opacity-40 max-md:h-11 max-md:px-2"
              >
                <RefreshIcon className="size-4 shrink-0" />
                <span className="whitespace-nowrap">立即搜索</span>
              </button>
            )}
            {canSubscribe &&
              canSearch &&
              (detail.progress.missing > 0 || detail.wanted.some((w) => w.upgrade)) && (
                <Link
                  to={`/search?q=${encodeURIComponent(detail.media.title)}&for_sub=${detail.id}`}
                  className="btn-glass inline-flex h-10 min-w-0 items-center justify-center gap-1.5 border border-white/10 bg-white/[0.05] px-4 text-sub font-medium backdrop-blur-md max-md:h-11 max-md:px-2"
                  title="到站点资源搜索里挑一条种子，直接投给本订阅（跳过规则组限制；替换已入库版本时按入库实测裁决，证明更优才替换）"
                >
                  <SearchIcon className="size-4 shrink-0" />
                  <span className="whitespace-nowrap">手动选种</span>
                </Link>
              )}
            {(canSubscribe || canManageSubscriptions) && (
              <>
                <span className="max-md:hidden">
                  <SubscriptionManageMenu
                    busy={busy}
                    paused={detailStatus === "paused"}
                    completed={detailStatus === "completed"}
                    canSubscribe={canSubscribe}
                    canManageSubscriptions={canManageSubscriptions}
                    followFuture={isMovie ? null : detail.follow_future}
                    onAdjust={onAdjust}
                    onUpgradeRun={onUpgradeRun}
                    onToggleFollowFuture={onToggleFollowFuture}
                    onSwitchRule={onSwitchRule}
                    onTogglePause={onTogglePause}
                    onRemove={onRemove}
                  />
                </span>
                <button
                  type="button"
                  disabled={busy}
                  onClick={onOpenManageSheet}
                  className="hidden h-11 min-w-0 items-center justify-center gap-1.5 rounded-full bg-white/[0.035] px-2 text-sub font-medium text-white/65 transition hover:bg-white/[0.09] hover:text-white disabled:opacity-40 max-md:flex"
                >
                  <MoreIcon className="size-4 shrink-0" />
                  <span className="whitespace-nowrap">更多</span>
                </button>
              </>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

function SubscriptionFact({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="min-w-0">
      <p className="text-caption text-white/40">{label}</p>
      <p
        className="mt-1 truncate text-sub font-medium text-white/85"
        title={typeof value === "string" ? value : undefined}
      >
        {value}
      </p>
    </div>
  );
}

function ProgressStrip({
  progress,
  unitLabel,
  upgrading = 0,
}: {
  progress: SubscriptionDetail["progress"];
  unitLabel: "部" | "集";
  upgrading?: number;
}) {
  const { total, missing, grabbing, imported } = progress;
  const denom = Math.max(total, 1);
  const inPipeline = grabbing;
  const importedSettled = Math.max(imported - upgrading, 0);
  return (
    <div className="mt-4 max-md:col-span-2 max-md:mt-1">
      <div className="flex items-center justify-between gap-4 text-sub">
        <span className="font-semibold text-white/85">收录进度</span>
        <span className="tnum shrink-0 text-white/55">
          {imported} / {total} {unitLabel}
        </span>
      </div>
      <div
        role="img"
        aria-label={
          `共 ${total} ${unitLabel}，已入库 ${imported}` +
          (upgrading > 0 ? `（其中洗版中 ${upgrading}）` : "") +
          `，下载中 ${inPipeline}，缺失 ${missing}`
        }
        className="mt-2 flex h-1.5 w-full overflow-hidden rounded-full bg-white/[0.12]"
      >
        <div className="bg-[var(--ok)]" style={{ width: `${(importedSettled / denom) * 100}%` }} />
        <div className="bg-[#2dd4bf]" style={{ width: `${(upgrading / denom) * 100}%` }} />
        <div className="bg-[var(--info)]" style={{ width: `${(inPipeline / denom) * 100}%` }} />
      </div>
      <div className="tnum mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-caption text-white/45">
        <ProgressLegend color="var(--ok)" label={`已入库 ${importedSettled}`} />
        {upgrading > 0 && <ProgressLegend color="#2dd4bf" label={`洗版中 ${upgrading}`} />}
        <ProgressLegend color="var(--info)" label={`下载中 ${inPipeline}`} />
        <ProgressLegend color="var(--line-strong)" label={`缺失 ${missing}`} />
      </div>
    </div>
  );
}

function ProgressLegend({ color, label }: { color: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className="size-1.5 rounded-full" style={{ backgroundColor: color }} />
      {label}
    </span>
  );
}
