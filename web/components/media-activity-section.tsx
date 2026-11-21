"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import { FilterMenu } from "@/components/filter-menu";
import { ChevronRightIcon, DownloadIcon, PlayIcon } from "@/components/icons";
import {
  EmptyAction,
  EmptyState,
  HiddenCountRow,
  PlaybackHistoryList,
  STATS_PERIODS,
} from "@/components/playback-stats-section";
import { WatchStatsPanel } from "@/components/watch-stats-panel";
import { loadActivityScope, saveActivityScope } from "@/lib/activity-scope";
import { listMembers, type MemberView } from "@/lib/api/members";
import {
  endDevicePlayback,
  fetchMediaActivity,
  revokePlaybackDevice,
  type MediaActivityScope,
  type MediaActivitySnapshot,
} from "@/lib/api/playback";
import { formatRate } from "@/lib/media-activity-helpers";
import type { WatchViewName } from "@/lib/task-center";
import { useVisiblePolling } from "@/lib/use-visible-polling";

import { DownloadCard, SectionHeading, SessionCard } from "./media-activity/activity-cards";
import {
  EndPlaybackDialog,
  RevokeDeviceDialog,
  type RevokeTarget,
} from "./media-activity/device-action-dialogs";
import { SCOPE_OPTIONS, WatchToolbar } from "./media-activity/watch-toolbar";

const POLL_INTERVAL_MS = 8_000;

const EMPTY_SNAPSHOT: MediaActivitySnapshot = {
  sessions: [],
  downloads: [],
  hidden_session_count: 0,
  hidden_download_count: 0,
};

export interface MediaActivityState {
  snapshot: MediaActivitySnapshot;
  enabled: boolean;
  loading: boolean;
  error: string | null;
  refresh: () => void;
  scope: MediaActivityScope;
  setScope: (scope: MediaActivityScope) => void;
}

/**
 * 媒体库活动快照的轮询装载。
 */
export function useMediaActivity(enabled: boolean): MediaActivityState {
  const [snapshot, setSnapshot] = useState<MediaActivitySnapshot>(EMPTY_SNAPSHOT);
  const [loading, setLoading] = useState(enabled);
  const [error, setError] = useState<string | null>(null);
  const [scope, setScopeState] = useState<MediaActivityScope>(loadActivityScope);
  const pending = useRef(false);
  const loaded = useRef(false);
  const requestScope = useRef(scope);

  const refresh = useCallback(() => {
    if (!enabled || pending.current) return;
    pending.current = true;
    if (!loaded.current) setLoading(true);
    const target = requestScope.current;
    void (async () => {
      try {
        const next = await fetchMediaActivity(target);
        if (requestScope.current !== target) return;
        setSnapshot(next);
        setError(null);
        loaded.current = true;
      } catch (caught) {
        setError((caught as Error).message || "媒体库活动加载失败");
      } finally {
        pending.current = false;
        setLoading(false);
      }
    })();
  }, [enabled]);

  const setScope = useCallback(
    (next: MediaActivityScope) => {
      if (next === requestScope.current) return;
      requestScope.current = next;
      setScopeState(next);
      saveActivityScope(next);
      refresh();
    },
    [refresh],
  );

  useVisiblePolling(refresh, enabled ? POLL_INTERVAL_MS : null, { leading: true });
  return { snapshot, enabled, loading, error, refresh, scope, setScope };
}

/** 「全部」视图顶部的媒体库活动汇总导航。 */
export function MediaActivitySummaryNav({
  snapshot,
  onOpen,
}: {
  snapshot: MediaActivitySnapshot;
  onOpen: () => void;
}) {
  const playing = snapshot.sessions.length + snapshot.hidden_session_count;
  const downloading = snapshot.downloads.length + snapshot.hidden_download_count;
  if (playing === 0 && downloading === 0) return null;
  const allPaused = playing > 0 && snapshot.sessions.every((s) => s.paused);
  const totalRate =
    snapshot.sessions.reduce((sum, s) => sum + (s.rate_bytes_per_second ?? 0), 0) +
    snapshot.downloads.reduce((sum, d) => sum + d.rate_bytes_per_second, 0);
  const parts: string[] = [];
  if (playing > 0) parts.push(`正在播放 ${playing}`);
  if (downloading > 0) parts.push(`正在下载 ${downloading}`);
  if (totalRate > 0) parts.push(formatRate(totalRate));
  return (
    <button
      type="button"
      onClick={onOpen}
      className="mt-4 flex w-full items-center gap-3 rounded-xl border border-[var(--info)]/25 bg-[var(--info)]/[0.07] px-4 py-3 text-left transition hover:border-[var(--info)]/40 hover:bg-[var(--info)]/[0.12]"
    >
      <span className="relative flex size-2 shrink-0" aria-hidden="true">
        {!allPaused && (
          <span className="absolute inline-flex size-2 motion-safe:animate-ping rounded-full bg-[var(--info)]/60" />
        )}
        <span className="relative inline-flex size-2 rounded-full bg-[var(--info)]" />
      </span>
      <span className="min-w-0 flex-1 truncate text-sub text-white/85">
        <span className="font-semibold">媒体库</span>
        <span className="tnum ml-2.5 text-white/60">{parts.join(" · ")}</span>
      </span>
      <span className="flex shrink-0 items-center gap-0.5 text-caption font-semibold text-[var(--info)]">
        查看明细
        <ChevronRightIcon className="size-3.5" />
      </span>
    </button>
  );
}

/** 「观看」视角主体：正在播放 / 播放记录 / 观看统计。 */
export function MediaActivityPanel({
  snapshot,
  enabled,
  loading,
  error,
  refresh,
  scope,
  setScope,
  view,
  onViewChange,
}: MediaActivityState & {
  view: WatchViewName;
  onViewChange: (view: WatchViewName) => void;
}) {
  const toast = useToast();
  const [pendingRevoke, setPendingRevoke] = useState<RevokeTarget | null>(null);
  const [pendingEnd, setPendingEnd] = useState<RevokeTarget | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [memberId, setMemberId] = useState<string | null>(null);
  const [days, setDays] = useState(30);
  const [members, setMembers] = useState<MemberView[]>([]);
  const liveCount = snapshot.sessions.length + snapshot.downloads.length;
  const hiddenLiveCount = snapshot.hidden_session_count + snapshot.hidden_download_count;
  const showAll = useCallback(() => setScope("all"), [setScope]);
  const actionKey = (target: RevokeTarget) => `${target.userId}:${target.deviceId}`;

  useEffect(() => {
    if (!enabled || view === "playing" || members.length > 0) return;
    void listMembers()
      .then(setMembers)
      .catch(() => undefined);
  }, [enabled, view, members.length]);
  const memberOptions = useMemo(
    () => [
      { value: "", label: "全部成员" },
      ...members.map((m) => ({ value: m.id, label: m.login })),
    ],
    [members],
  );

  const requestRevoke = useCallback((userId: string, deviceId: string, label: string) => {
    setPendingRevoke({ userId, deviceId, label });
  }, []);
  const requestEnd = useCallback((userId: string, deviceId: string, label: string) => {
    setPendingEnd({ userId, deviceId, label });
  }, []);

  async function confirmEnd(target: RevokeTarget) {
    if (revoking != null) return;
    setRevoking(actionKey(target));
    try {
      toast.success(await endDevicePlayback(target.userId, target.deviceId));
      setPendingEnd(null);
      refresh();
    } catch (caught) {
      toast.error((caught as Error).message || "结束播放失败");
    } finally {
      setRevoking(null);
    }
  }

  async function confirmRevoke(target: RevokeTarget) {
    if (revoking != null) return;
    setRevoking(actionKey(target));
    try {
      toast.success(await revokePlaybackDevice(target.userId, target.deviceId));
      setPendingRevoke(null);
      refresh();
    } catch (caught) {
      toast.error((caught as Error).message || "注销设备失败");
    } finally {
      setRevoking(null);
    }
  }

  const scopeFilter = (
    <FilterMenu label="范围" value={scope} options={SCOPE_OPTIONS} onChange={setScope} />
  );
  const memberLabel =
    memberId == null ? null : (memberOptions.find((o) => o.value === memberId)?.label ?? null);

  return (
    <div>
      <WatchToolbar view={view} onViewChange={onViewChange} liveCount={liveCount + hiddenLiveCount}>
        {view === "stats" && (
          <FilterMenu label="周期" value={days} options={STATS_PERIODS} onChange={setDays} />
        )}
        {view !== "playing" && (
          <FilterMenu
            label="成员"
            value={memberId ?? ""}
            options={memberOptions}
            onChange={(value) => setMemberId(value === "" ? null : value)}
          />
        )}
        {scopeFilter}
      </WatchToolbar>

      {error && (
        <p className="mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub leading-6 text-amber-100">
          {error}
        </p>
      )}

      {view === "playing" && (
        <div className="mt-4">
          {loading && liveCount === 0 && hiddenLiveCount === 0 ? (
            <div className="flex items-center justify-center gap-2.5 py-16 text-ui text-[var(--text-muted)]">
              <BrandLoader className="size-5" />
              正在读取媒体库活动…
            </div>
          ) : snapshot.sessions.length === 0 && snapshot.hidden_session_count === 0 ? (
            <EmptyState
              icon={<PlayIcon className="size-5" />}
              title="现在没有人在看"
              description="设备开始播放后几秒内会出现在这里，网页播放器和 Jellyfin 客户端都算；想看之前谁看了什么，去最近播放。"
              actions={
                <EmptyAction onClick={() => onViewChange("plays")}>查看最近播放</EmptyAction>
              }
            />
          ) : (
            <div className="space-y-2.5">
              {snapshot.sessions.map((session) => (
                <SessionCard
                  key={`${session.user_id}-${session.device_id}-${session.media.media_item_id}-${session.media.season_number}-${session.media.episode_number}`}
                  session={session}
                  onEnd={requestEnd}
                  onRevoke={requestRevoke}
                  busy={revoking != null}
                />
              ))}
              {snapshot.hidden_session_count > 0 && (
                <div className="rounded-2xl border border-white/[0.07] bg-white/[0.02]">
                  <HiddenCountRow
                    count={snapshot.hidden_session_count}
                    noun="台设备在播放的内容"
                    onShowAll={showAll}
                  />
                </div>
              )}
            </div>
          )}

          {(snapshot.downloads.length > 0 || snapshot.hidden_download_count > 0) && (
            <section className="mt-7" aria-label="正在下载">
              <SectionHeading
                icon={<DownloadIcon className="size-4 text-[var(--info)]" />}
                title="正在下载"
                count={snapshot.downloads.length + snapshot.hidden_download_count}
              />
              <div className="space-y-2.5">
                {snapshot.downloads.map((download) => (
                  <DownloadCard
                    key={`${download.user_id}-${download.device_id}-${download.file_name}`}
                    download={download}
                    onRevoke={requestRevoke}
                    busy={revoking != null}
                  />
                ))}
                {snapshot.hidden_download_count > 0 && (
                  <div className="rounded-2xl border border-white/[0.07] bg-white/[0.02]">
                    <HiddenCountRow
                      count={snapshot.hidden_download_count}
                      noun="条下载"
                      onShowAll={showAll}
                    />
                  </div>
                )}
              </div>
            </section>
          )}
        </div>
      )}

      {view === "plays" && enabled && (
        <div className="mt-4">
          <PlaybackHistoryList
            scope={scope}
            memberId={memberId}
            memberLabel={memberLabel}
            onClearMember={() => setMemberId(null)}
            onShowAll={showAll}
          />
        </div>
      )}

      {view === "stats" && enabled && (
        <div className="mt-4">
          <WatchStatsPanel
            scope={scope}
            days={days}
            memberId={memberId}
            memberLabel={memberLabel}
            onMemberSelect={setMemberId}
            onDaysChange={setDays}
            onShowAll={showAll}
          />
        </div>
      )}

      {pendingEnd && (
        <EndPlaybackDialog
          target={pendingEnd}
          busy={revoking === actionKey(pendingEnd)}
          onClose={() => {
            if (revoking == null) setPendingEnd(null);
          }}
          onConfirm={() => void confirmEnd(pendingEnd)}
        />
      )}
      {pendingRevoke && (
        <RevokeDeviceDialog
          target={pendingRevoke}
          busy={revoking === actionKey(pendingRevoke)}
          onClose={() => {
            if (revoking == null) setPendingRevoke(null);
          }}
          onConfirm={() => void confirmRevoke(pendingRevoke)}
        />
      )}
    </div>
  );
}
