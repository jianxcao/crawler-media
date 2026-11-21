"use client";

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
} from "react";

import {
  reportPlaybackMetric,
  reportPlaybackMetricOnUnload,
  reportPlaybackProgress,
  reportPlaybackProgressOnUnload,
  type PlaybackApiScope,
  type PlaybackMetricPayload,
  type PlaybackSession,
  type PlaybackUnit,
} from "@/lib/api/playback";
import {
  DIRECT_SHORTFALL_SAMPLES,
  directDownlinkShort,
  formatBandwidth,
} from "@/lib/player/bandwidth";
import { getPlayerDeviceId } from "@/lib/player/device";
import type { PlaybackEngine } from "@/lib/player/engine";
import { createFrameDropTracker } from "@/lib/player/framedrop";
import { startPlaybackHeartbeats } from "@/lib/player/heartbeat";
import type { PlayerEvent, PlayerPhase } from "@/lib/player/machine";
import { bindPlaybackPageLifecycle } from "@/lib/player/page-lifecycle";
import { initialQoe, isReportable, summarize, type QoeEvent } from "@/lib/player/qoe";
import { toFileMs } from "@/lib/player/timeline";

import {
  PROGRESS_INTERVAL_MS,
  type CurrentPageRefValue,
  type InFlightRenewalState,
  type PageRestoreState,
} from "./player-types";

type QoeAccumulator = ReturnType<typeof initialQoe>;

export function usePlayerTelemetry({
  unitRef,
  unitKey,
  apiRef,
  video,
  session,
  phase,
  selectedSubtitle,
  engineRef,
  playSessionIdRef,
  eventSequenceRef,
  startMsRef,
  pendingFileMsRef,
  positionRef,
  reportedStartRef,
  wantsPlayRef,
  lastDownlinkRef,
  sourceBitrateRef,
  directShortRef,
  directHintShownRef,
  qoeRef,
  qoe,
  flashNotice,
  dispatch,
  pageGenerationRef,
  pageSuspendedRef,
  currentPageRef,
  pageRestoreRef,
  inFlightRenewalRef,
  endedByAdminRef,
  onEndedByAdmin,
  setPositionMs,
}: {
  unitRef: MutableRefObject<PlaybackUnit>;
  unitKey: string;
  apiRef: MutableRefObject<PlaybackApiScope>;
  video: HTMLVideoElement | null;
  session: PlaybackSession | null;
  phase: PlayerPhase;
  selectedSubtitle: string | null;
  engineRef: MutableRefObject<PlaybackEngine | null>;
  playSessionIdRef: MutableRefObject<string | null>;
  eventSequenceRef: MutableRefObject<number>;
  startMsRef: MutableRefObject<number>;
  pendingFileMsRef: MutableRefObject<number>;
  positionRef: MutableRefObject<number>;
  reportedStartRef: MutableRefObject<string | null>;
  wantsPlayRef: MutableRefObject<boolean>;
  lastDownlinkRef: MutableRefObject<number | null>;
  sourceBitrateRef: MutableRefObject<number | null>;
  directShortRef: MutableRefObject<number>;
  directHintShownRef: MutableRefObject<boolean>;
  qoeRef: MutableRefObject<QoeAccumulator>;
  qoe: (event: QoeEvent) => void;
  flashNotice: (text: string) => void;
  dispatch: Dispatch<PlayerEvent>;
  pageGenerationRef: MutableRefObject<number>;
  pageSuspendedRef: MutableRefObject<boolean>;
  currentPageRef: MutableRefObject<CurrentPageRefValue>;
  pageRestoreRef: MutableRefObject<PageRestoreState | null>;
  inFlightRenewalRef: MutableRefObject<InFlightRenewalState | null>;
  endedByAdminRef: MutableRefObject<boolean>;
  onEndedByAdmin: () => void;
  setPositionMs: (ms: number) => void;
}) {
  const [speedLabel, setSpeedLabel] = useState<string | null>(null);
  const heartbeatStopRef = useRef<(() => void) | undefined>(undefined);
  const qoeSnapshotRef = useRef<() => PlaybackMetricPayload | null>(() => null);

  useEffect(() => {
    setSpeedLabel(null);
  }, [unitKey]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      const engine = engineRef.current;
      if (!engine) return;
      const stats = engine.stats();
      const label = formatBandwidth(stats.downlinkBps);
      if (label !== null) setSpeedLabel(label);
      if (stats.downlinkBps !== null) lastDownlinkRef.current = stats.downlinkBps;
      if (stats.engine === "direct" && !directHintShownRef.current) {
        const short = directDownlinkShort({
          downlinkBps: stats.downlinkBps,
          sourceBitrateBps: sourceBitrateRef.current,
        });
        directShortRef.current = short ? directShortRef.current + 1 : 0;
        if (directShortRef.current >= DIRECT_SHORTFALL_SAMPLES) {
          directHintShownRef.current = true;
          flashNotice("线路速度低于片源码率，可在设置里选更低画质");
        }
      }
    }, 1000);
    return () => window.clearInterval(timer);
  }, [
    directHintShownRef,
    directShortRef,
    engineRef,
    flashNotice,
    lastDownlinkRef,
    sourceBitrateRef,
  ]);

  const trackRefs = useCallback(
    () => ({
      audio_track: session?.decision.audio?.track_ref ?? undefined,
      subtitle_track: selectedSubtitle ?? "off",
    }),
    [session, selectedSubtitle],
  );
  const trackRefsRef = useRef(trackRefs);
  trackRefsRef.current = trackRefs;

  const sendProgress = useCallback(
    (paused: boolean | undefined) => {
      const playSessionId = playSessionIdRef.current;
      if (!playSessionId || pageSuspendedRef.current) return;
      eventSequenceRef.current += 1;
      void reportPlaybackProgress(
        {
          ...unitRef.current,
          event: "progress",
          position_ms: positionRef.current,
          paused,
          play_session_id: playSessionId,
          event_sequence: eventSequenceRef.current,
          ...trackRefsRef.current(),
        },
        apiRef.current,
      )
        .then((watch) => {
          if (
            playSessionIdRef.current === playSessionId &&
            !pageSuspendedRef.current &&
            watch.ended_by_admin
          ) {
            onEndedByAdmin();
          }
        })
        .catch(() => undefined);
    },
    [
      apiRef,
      eventSequenceRef,
      onEndedByAdmin,
      pageSuspendedRef,
      playSessionIdRef,
      positionRef,
      unitRef,
    ],
  );
  const sendProgressRef = useRef(sendProgress);
  sendProgressRef.current = sendProgress;

  useEffect(() => {
    const playSessionId = playSessionIdRef.current;
    if (!playSessionId || pageSuspendedRef.current) return;
    const startKey = `${unitKey}:${playSessionId}`;
    if (phase === "playing" && reportedStartRef.current !== startKey) {
      reportedStartRef.current = startKey;
      eventSequenceRef.current += 1;
      void reportPlaybackProgress(
        {
          ...unitRef.current,
          event: "start",
          play_session_id: playSessionId,
          event_sequence: eventSequenceRef.current,
          ...trackRefs(),
        },
        apiRef.current,
      )
        .then((watch) => {
          if (
            playSessionIdRef.current === playSessionId &&
            !pageSuspendedRef.current &&
            watch.ended_by_admin
          ) {
            onEndedByAdmin();
          }
        })
        .catch(() => undefined);
    }
    if (!video) return;
    const stop = startPlaybackHeartbeats(
      phase,
      reportedStartRef.current === startKey,
      video,
      sendProgress,
      PROGRESS_INTERVAL_MS,
      () => pageSuspendedRef.current,
    );
    heartbeatStopRef.current = stop;
    return () => {
      stop?.();
      if (heartbeatStopRef.current === stop) heartbeatStopRef.current = undefined;
    };
  }, [
    apiRef,
    eventSequenceRef,
    onEndedByAdmin,
    pageSuspendedRef,
    phase,
    playSessionIdRef,
    reportedStartRef,
    sendProgress,
    trackRefs,
    unitKey,
    unitRef,
    video,
  ]);

  useEffect(() => {
    if (!video) return;
    const report = (paused: boolean) => {
      if (
        pageSuspendedRef.current ||
        reportedStartRef.current !== `${unitKey}:${playSessionIdRef.current ?? ""}`
      ) {
        return;
      }
      sendProgressRef.current(paused);
    };
    const onPlaying = () => report(false);
    const onPause = () => report(true);
    video.addEventListener("playing", onPlaying);
    video.addEventListener("pause", onPause);
    return () => {
      video.removeEventListener("playing", onPlaying);
      video.removeEventListener("pause", onPause);
    };
  }, [pageSuspendedRef, playSessionIdRef, reportedStartRef, video, unitKey]);

  const mediaItemId = unitRef.current.media_item_id;
  const qoeSnapshot = useCallback(() => {
    const summary = summarize(qoeRef.current);
    if (!isReportable(summary)) return null;
    const decision = session?.decision;
    if (!decision || decision.tier == null) return null;
    return {
      media_item_id: String(mediaItemId),
      library_file_id: decision.file_id ?? null,
      tier: decision.tier,
      degraded_from: decision.degraded_from ?? null,
      engine: engineRef.current?.stats().engine ?? "",
      hw_backend: session?.hw_backend ?? "",
      ...summary,
    };
  }, [engineRef, mediaItemId, qoeRef, session]);

  useEffect(() => {
    qoeSnapshotRef.current = qoeSnapshot;
  }, [qoeSnapshot]);

  const flushUnmountStopRef = useRef((snapshot: PlaybackUnit) => {
    void snapshot;
  });
  flushUnmountStopRef.current = (snapshot: PlaybackUnit) => {
    const playSessionId = playSessionIdRef.current;
    if (reportedStartRef.current === null || !playSessionId) return;
    reportedStartRef.current = null;
    playSessionIdRef.current = null;
    eventSequenceRef.current += 1;
    void reportPlaybackProgress(
      {
        ...snapshot,
        event: "stop",
        position_ms: positionRef.current,
        play_session_id: playSessionId,
        event_sequence: eventSequenceRef.current,
        ...trackRefsRef.current(),
      },
      apiRef.current,
    ).catch(() => undefined);
    const metric = qoeSnapshotRef.current();
    if (metric) void reportPlaybackMetric(metric, apiRef.current).catch(() => undefined);
    qoeRef.current = initialQoe();
  };

  useEffect(() => {
    const snapshot = { ...unitRef.current };
    return () => {
      flushUnmountStopRef.current(snapshot);
    };
  }, [unitKey, unitRef]);

  /** BFCache 只挂起；返回时从持久前驱签发新 SID 和流地址。 */
  useEffect(
    () =>
      bindPlaybackPageLifecycle(window, {
        snapshot: () => ({
          unitKey: currentPageRef.current.unitKey,
          // Reporting is cleared before renewal; authorization still belongs to its predecessor
          // until the replacement grant is bound, including another cache trip while in flight.
          playSessionId:
            playSessionIdRef.current ??
            (pageRestoreRef.current?.unitKey === unitKey
              ? pageRestoreRef.current.playSessionId
              : null),
          renewedFromPlaySessionId:
            playSessionIdRef.current === null && pageRestoreRef.current?.unitKey === unitKey
              ? (pageRestoreRef.current.renewedFromPlaySessionId ?? null)
              : null,
          phase: currentPageRef.current.state.phase,
          positionMs:
            video && currentPageRef.current.state.session
              ? toFileMs(video.currentTime, startMsRef.current)
              : pendingFileMsRef.current,
          wantsPlay: wantsPlayRef.current,
          announced: reportedStartRef.current !== null,
          endedByAdmin: endedByAdminRef.current,
        }),
        suspend: (saved) => {
          pageGenerationRef.current += 1;
          pageSuspendedRef.current =
            !saved.endedByAdmin && !["error", "consent", "ended", "idle"].includes(saved.phase);
          heartbeatStopRef.current?.();
          heartbeatStopRef.current = undefined;
          if (saved.announced && saved.playSessionId) {
            eventSequenceRef.current += 1;
            reportPlaybackProgressOnUnload(
              {
                ...unitRef.current,
                event: "progress",
                paused: true,
                position_ms: saved.positionMs,
                play_session_id: saved.playSessionId,
                event_sequence: eventSequenceRef.current,
                ...trackRefsRef.current(),
              },
              apiRef.current,
            );
          }
          qoe({ type: "suspend" });
          video?.pause();
        },
        restore: (saved) => {
          const tracks = trackRefsRef.current();
          const deviceId = getPlayerDeviceId();
          if (
            pageRestoreRef.current?.unitKey !== saved.unitKey ||
            pageRestoreRef.current.playSessionId !== saved.playSessionId
          ) {
            pageRestoreRef.current = {
              unitKey: saved.unitKey,
              playSessionId: saved.playSessionId,
              renewedFromPlaySessionId: null,
              deviceId,
              audioTrack: tracks.audio_track,
              subtitleTrack: tracks.subtitle_track,
            };
          } else {
            pageRestoreRef.current = {
              ...pageRestoreRef.current,
              renewedFromPlaySessionId: null,
              deviceId,
            };
          }
          playSessionIdRef.current = null;
          reportedStartRef.current = null;
          eventSequenceRef.current = 0;
          wantsPlayRef.current = saved.wantsPlay;
          pageSuspendedRef.current = false;
          pendingFileMsRef.current = saved.positionMs;
          setPositionMs(saved.positionMs);
          dispatch({ type: "restart", startMs: saved.positionMs });
        },
        stop: (saved) => {
          pageGenerationRef.current += 1;
          pageSuspendedRef.current = true;
          pageRestoreRef.current = null;
          inFlightRenewalRef.current = null;
          heartbeatStopRef.current?.();
          heartbeatStopRef.current = undefined;
          reportedStartRef.current = null;
          playSessionIdRef.current = null;
          if (saved.announced && saved.playSessionId) {
            eventSequenceRef.current += 1;
            reportPlaybackProgressOnUnload(
              {
                ...unitRef.current,
                event: "stop",
                position_ms: saved.positionMs,
                play_session_id: saved.playSessionId,
                event_sequence: eventSequenceRef.current,
                ...trackRefsRef.current(),
              },
              apiRef.current,
            );
          }
          const metric = qoeSnapshotRef.current();
          if (metric) reportPlaybackMetricOnUnload(metric, apiRef.current);
          qoeRef.current = initialQoe();
        },
      }),
    [
      apiRef,
      currentPageRef,
      dispatch,
      endedByAdminRef,
      eventSequenceRef,
      inFlightRenewalRef,
      pageGenerationRef,
      pageRestoreRef,
      pageSuspendedRef,
      pendingFileMsRef,
      playSessionIdRef,
      qoe,
      qoeRef,
      reportedStartRef,
      setPositionMs,
      startMsRef,
      unitKey,
      unitRef,
      video,
      wantsPlayRef,
    ],
  );

  useEffect(() => {
    if (!video) return;
    const withFrameCallback = video as HTMLVideoElement & {
      requestVideoFrameCallback?: (cb: () => void) => number;
      cancelVideoFrameCallback?: (handle: number) => void;
    };
    if (!withFrameCallback.requestVideoFrameCallback) return;
    const handle = withFrameCallback.requestVideoFrameCallback(() => {
      qoe({ type: "first-frame", at: performance.now() });
    });
    return () => withFrameCallback.cancelVideoFrameCallback?.(handle);
  }, [video, session?.session_id, qoe]);

  useEffect(() => {
    if (!video) return;
    const tracker = createFrameDropTracker();
    const videoIsCopy = session?.decision.video?.action === "copy";
    const onVisibility = () => tracker.reset();
    const onSeeking = () => tracker.reset();
    document.addEventListener("visibilitychange", onVisibility);
    video.addEventListener("seeking", onSeeking);
    const timer = window.setInterval(() => {
      qoe({ type: "tick", at: performance.now(), playing: !video.paused && !video.ended });
      const stats = engineRef.current?.stats();
      if (stats?.totalFrames == null || stats.droppedFrames == null) return;
      qoe({ type: "frames", dropped: stats.droppedFrames, total: stats.totalFrames });
      if (!videoIsCopy || document.visibilityState !== "visible" || video.paused) return;
      const verdict = tracker.sample({ dropped: stats.droppedFrames, total: stats.totalFrames });
      if (verdict.degrade) {
        dispatch({
          type: "failed",
          reason: `直通播放持续掉帧（${Math.round((verdict.ratio ?? 0) * 100)}%），正在换转码重试`,
        });
      }
    }, 1000);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
      video.removeEventListener("seeking", onSeeking);
    };
  }, [dispatch, engineRef, qoe, session, video]);

  return { speedLabel };
}
