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
import type { PlaybackEngine } from "@/lib/player/engine";
import { createFrameDropTracker } from "@/lib/player/framedrop";
import { startPlaybackHeartbeats } from "@/lib/player/heartbeat";
import type { PlayerEvent, PlayerPhase } from "@/lib/player/machine";
import { initialQoe, isReportable, summarize, type QoeEvent } from "@/lib/player/qoe";

import { PROGRESS_INTERVAL_MS } from "./player-types";

type QoeAccumulator = ReturnType<typeof initialQoe>;

export function usePlayerTelemetry({
  unitRef,
  unitKey,
  startMsOverride,
  apiRef,
  video,
  session,
  phase,
  selectedSubtitle,
  engineRef,
  playSessionIdRef,
  eventSequenceRef,
  positionRef,
  reportedStartRef,
  lastDownlinkRef,
  sourceBitrateRef,
  directShortRef,
  directHintShownRef,
  qoeRef,
  qoe,
  flashNotice,
  dispatch,
}: {
  unitRef: MutableRefObject<PlaybackUnit>;
  unitKey: string;
  startMsOverride?: number;
  apiRef: MutableRefObject<PlaybackApiScope>;
  video: HTMLVideoElement | null;
  session: PlaybackSession | null;
  phase: PlayerPhase;
  selectedSubtitle: string | null;
  engineRef: MutableRefObject<PlaybackEngine | null>;
  playSessionIdRef: MutableRefObject<string | null>;
  eventSequenceRef: MutableRefObject<number>;
  positionRef: MutableRefObject<number>;
  reportedStartRef: MutableRefObject<string | null>;
  lastDownlinkRef: MutableRefObject<number | null>;
  sourceBitrateRef: MutableRefObject<number | null>;
  directShortRef: MutableRefObject<number>;
  directHintShownRef: MutableRefObject<boolean>;
  qoeRef: MutableRefObject<QoeAccumulator>;
  qoe: (event: QoeEvent) => void;
  flashNotice: (text: string) => void;
  dispatch: Dispatch<PlayerEvent>;
}) {
  const [speedLabel, setSpeedLabel] = useState<string | null>(null);
  const endedByAdminRef = useRef(false);
  const qoeSnapshotRef = useRef<() => PlaybackMetricPayload | null>(() => null);

  useEffect(() => {
    endedByAdminRef.current = false;
  }, [unitKey, startMsOverride]);

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

  const onEndedByAdmin = useCallback(() => {
    if (endedByAdminRef.current) return;
    endedByAdminRef.current = true;
    dispatch({
      type: "fatal",
      message: "管理员已结束本次播放",
      suggestion: "稍后可以重新开始播放；观看进度已经保存。",
    });
  }, [dispatch]);

  const sendProgress = useCallback(
    (paused: boolean | undefined) => {
      const playSessionId = playSessionIdRef.current;
      if (!playSessionId) return;
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
          if (watch.ended_by_admin) onEndedByAdmin();
        })
        .catch(() => undefined);
    },
    [apiRef, eventSequenceRef, onEndedByAdmin, playSessionIdRef, positionRef, unitRef],
  );
  const sendProgressRef = useRef(sendProgress);
  sendProgressRef.current = sendProgress;

  useEffect(() => {
    const playSessionId = playSessionIdRef.current;
    if (!playSessionId) return;
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
          if (watch.ended_by_admin) onEndedByAdmin();
        })
        .catch(() => undefined);
    }
    if (!video) return;
    return startPlaybackHeartbeats(
      phase,
      reportedStartRef.current === startKey,
      video,
      sendProgress,
      PROGRESS_INTERVAL_MS,
    );
  }, [
    apiRef,
    eventSequenceRef,
    onEndedByAdmin,
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
      if (reportedStartRef.current !== `${unitKey}:${playSessionIdRef.current ?? ""}`) return;
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
  }, [playSessionIdRef, reportedStartRef, video, unitKey]);

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

  useEffect(() => {
    const onPageHide = () => {
      const playSessionId = playSessionIdRef.current;
      if (reportedStartRef.current !== null && playSessionId) {
        eventSequenceRef.current += 1;
        reportPlaybackProgressOnUnload(
          {
            ...unitRef.current,
            event: "stop",
            position_ms: positionRef.current,
            play_session_id: playSessionId,
            event_sequence: eventSequenceRef.current,
            ...trackRefsRef.current(),
          },
          apiRef.current,
        );
      }
      const metric = qoeSnapshotRef.current();
      if (metric) reportPlaybackMetricOnUnload(metric, apiRef.current);
    };
    window.addEventListener("pagehide", onPageHide);
    return () => window.removeEventListener("pagehide", onPageHide);
  }, [apiRef, eventSequenceRef, playSessionIdRef, positionRef, reportedStartRef, unitKey, unitRef]);

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
