"use client";

import { useEffect, useMemo, useState } from "react";

import { ActivityIcon } from "@/components/icons";
import {
  EmptyAction,
  EmptyState,
  HiddenCountRow,
  TitleText,
  formatWatched,
} from "@/components/playback-stats-section";
import { PosterImage } from "@/components/poster-image";
import {
  fetchPlaybackWatchStats,
  type MediaActivityScope,
  type PlaybackWatchStats,
} from "@/lib/api/playback";
import { imageUrl } from "@/lib/image-proxy";
import type { MetricKey } from "@/lib/watch-stats-helpers";

import { FavoritePodium, StatsSkeleton } from "./watch-stats/favorite-podium";
import { BreakdownPanel, HourHeatmap } from "./watch-stats/watch-stats-breakdown";
import { METRICS, MetricCard, TrendChart } from "./watch-stats/watch-stats-metrics";

/**
 * 观看统计仪表盘：指标卡 → TOP 3 领奖台 → 主趋势图 → 同构分解面板 + 时段热力图。
 */
export function WatchStatsPanel({
  scope,
  days,
  memberId,
  memberLabel,
  onMemberSelect,
  onDaysChange,
  onShowAll,
}: {
  scope: MediaActivityScope;
  days: number;
  memberId: string | null;
  memberLabel: string | null;
  onMemberSelect: (memberId: string | null) => void;
  onDaysChange: (days: number) => void;
  onShowAll: () => void;
}) {
  const [stats, setStats] = useState<PlaybackWatchStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [metricKey, setMetricKey] = useState<MetricKey>("watched_ms");
  const metric = useMemo(() => METRICS.find((m) => m.key === metricKey) ?? METRICS[0], [metricKey]);

  useEffect(() => {
    let cancelled = false;
    void fetchPlaybackWatchStats(days, scope, memberId)
      .then((next) => {
        if (cancelled) return;
        setStats(next);
        setError(null);
      })
      .catch((caught) => {
        if (!cancelled) setError((caught as Error).message || "观看统计加载失败");
      });
    return () => {
      cancelled = true;
    };
  }, [days, scope, memberId]);

  if (error) {
    return (
      <p className="rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub leading-6 text-amber-100">
        {error}
      </p>
    );
  }
  if (!stats) {
    return <StatsSkeleton />;
  }

  const widenActions = (
    <>
      {memberId != null && (
        <EmptyAction onClick={() => onMemberSelect(null)}>查看全部成员</EmptyAction>
      )}
      {days < 90 && <EmptyAction onClick={() => onDaysChange(90)}>看最近 90 天</EmptyAction>}
    </>
  );
  const who = memberId != null ? (memberLabel ?? "这位成员") : null;

  if (stats.current.plays === 0 && !stats.previous_available) {
    return (
      <EmptyState
        icon={<ActivityIcon className="size-5" />}
        title={who ? `${who}最近 ${days} 天没有播放` : `最近 ${days} 天没有播放记录`}
        description="统计从播放日志来：从现在起每一场播放都会计入，看得越久这里越有得看。"
        actions={widenActions}
      />
    );
  }

  const totalWatched = stats.current.watched_ms;
  const tierTotal = stats.by_tier.reduce((sum, r) => sum + r.plays, 0);
  const cards = (
    <div className="grid grid-cols-4 gap-2.5 max-md:grid-cols-2">
      {METRICS.map((m) => (
        <MetricCard
          key={m.key}
          metric={m}
          stats={stats}
          selected={m.key === metricKey}
          onSelect={() => setMetricKey(m.key)}
        />
      ))}
    </div>
  );
  const chart = (
    <div className="rounded-2xl border border-white/[0.08] bg-white/[0.02] px-3 pb-3 pt-3">
      <TrendChart stats={stats} metric={metric} />
    </div>
  );

  if (stats.current.plays === 0) {
    return (
      <div className="space-y-4">
        {cards}
        {chart}
        <EmptyState
          icon={<ActivityIcon className="size-5" />}
          title={who ? `${who}本周期没有播放` : "本周期没有播放"}
          description={`上一周期有 ${stats.previous.plays} 场；本周期一场都没有，没有可以分解的数据。`}
          actions={widenActions}
        />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {cards}

      <FavoritePodium
        favorites={stats.favorites}
        previous={stats.previous_favorites}
        memberId={memberId}
        hiddenCount={stats.hidden_title_count}
        onShowAll={onShowAll}
      />

      {chart}

      <div className="grid grid-cols-2 gap-4 max-md:grid-cols-1">
        <BreakdownPanel
          title="按成员"
          note={memberId != null ? "已钻取到一个成员，再点一次取消" : "点成员名钻取"}
          total={totalWatched}
          unit="个成员"
          formatValue={formatWatched}
          rows={stats.by_member.map((row) => ({
            key: String(row.member_id),
            label: row.member_name,
            value: row.watched_ms,
            valueLabel: formatWatched(row.watched_ms),
            secondary: `${row.plays} 场 · 看完 ${row.completed}`,
            selected: memberId === row.member_id,
            onSelect: () => onMemberSelect(memberId === row.member_id ? null : row.member_id),
          }))}
        />
        <BreakdownPanel
          title="按客户端"
          total={totalWatched}
          unit="个客户端"
          formatValue={formatWatched}
          rows={stats.by_client.map((row) => ({
            key: row.client,
            label: row.client,
            value: row.watched_ms,
            valueLabel: formatWatched(row.watched_ms),
            secondary: `${row.plays} 场`,
          }))}
        />
        <BreakdownPanel
          title="看得最多"
          total={totalWatched}
          unit="部"
          formatValue={formatWatched}
          fold="expand"
          rows={stats.top_titles.map((row, index) => ({
            key: `${row.media.media_item_id}-${index}`,
            label: <TitleText media={row.media} />,
            value: row.watched_ms,
            valueLabel: formatWatched(row.watched_ms),
            secondary: `${row.plays} 场`,
            leading: (
              <PosterImage
                src={row.media.poster_url ? imageUrl(row.media.poster_url) : null}
                alt={row.media.title}
                className="h-9 w-6 shrink-0 rounded-md object-cover ring-1 ring-white/10"
              />
            ),
          }))}
          footer={
            <HiddenCountRow count={stats.hidden_title_count} noun="部作品" onShowAll={onShowAll} />
          }
          emptyText={stats.hidden_title_count > 0 ? null : "播放过的条目已被删除"}
        />
        <BreakdownPanel
          title="按播放方式"
          note="仅网页播放；Jellyfin 客户端恒为直连"
          total={tierTotal}
          unit="种"
          formatValue={(v) => `${v} 场`}
          emptyText="本周期没有网页播放；Jellyfin 客户端不经过转码，不在这里分解"
          rows={stats.by_tier.map((row) => ({
            key: String(row.tier),
            label: row.label,
            value: row.plays,
            valueLabel: `${row.plays} 场`,
          }))}
        />
      </div>

      <HourHeatmap matrix={stats.by_hour} />
    </div>
  );
}
