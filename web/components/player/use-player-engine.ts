"use client";

import { useEffect, useRef, useState, type Dispatch, type MutableRefObject } from "react";

import {
  reportPlaybackClientLog,
  resolveStreamUrl,
  type ClientCapability,
  type PlaybackApiScope,
  type PlaybackSession,
} from "@/lib/api/playback";
import { bandwidthDegradeWanted, bandwidthRestartWanted } from "@/lib/player/bandwidth";
import { createEngine, type PlaybackEngine } from "@/lib/player/engine";
import type { PlayerEvent } from "@/lib/player/machine";
import { shouldApplyPostAttachSeek, type PlaybackMode } from "@/lib/player/playback-mode";
import type { QoeEvent } from "@/lib/player/qoe";
import { bufferedAhead } from "@/lib/player/stall";
import { toFileMs, toSessionSeconds } from "@/lib/player/timeline";

export function usePlayerEngine({
  unitKey,
  startMsOverride,
  video,
  session,
  mode,
  apiRef,
  capabilityRef,
  activeSessionRef,
  engineRef,
  pendingFileMsRef,
  startMsRef,
  positionRef,
  wantsPlayRef,
  lastDownlinkRef,
  sourceBitrateRef,
  pendingCommitRef,
  tryAutoplay,
  freezeFrame,
  flashNotice,
  qoe,
  setPaused,
  setPositionMs,
  dispatch,
}: {
  unitKey: string;
  startMsOverride?: number;
  video: HTMLVideoElement | null;
  session: PlaybackSession | null;
  mode: PlaybackMode | null;
  apiRef: MutableRefObject<PlaybackApiScope>;
  capabilityRef: MutableRefObject<ClientCapability | null>;
  activeSessionRef: MutableRefObject<PlaybackSession | null>;
  engineRef: MutableRefObject<PlaybackEngine | null>;
  pendingFileMsRef: MutableRefObject<number>;
  startMsRef: MutableRefObject<number>;
  positionRef: MutableRefObject<number>;
  wantsPlayRef: MutableRefObject<boolean>;
  lastDownlinkRef: MutableRefObject<number | null>;
  sourceBitrateRef: MutableRefObject<number | null>;
  pendingCommitRef: MutableRefObject<number | null>;
  tryAutoplay: () => Promise<void>;
  freezeFrame: (targetFileMs?: number) => void;
  flashNotice: (text: string) => void;
  qoe: (event: QoeEvent) => void;
  setPaused: (paused: boolean) => void;
  setPositionMs: (ms: number) => void;
  dispatch: Dispatch<PlayerEvent>;
}) {
  const [bufferedEndMs, setBufferedEndMs] = useState<number | null>(null);
  const [videoDurationMs, setVideoDurationMs] = useState<number | null>(null);
  const bandwidthRestartedRef = useRef(false);
  const directShortRef = useRef(0);
  const directHintShownRef = useRef(false);

  useEffect(() => {
    setBufferedEndMs(null);
    setVideoDurationMs(null);
  }, [unitKey, startMsOverride]);

  useEffect(() => {
    if (!session?.stream_url || !video || !mode) return;

    let disposed = false;
    bandwidthRestartedRef.current = false;
    directShortRef.current = 0;
    directHintShownRef.current = false;
    sourceBitrateRef.current = session.source?.bit_rate ?? null;
    const engine = createEngine({
      video,
      streamUrl: resolveStreamUrl(mode.streamUrl),
      container: session.decision.container ?? "mp4",
      hasMse: capabilityRef.current?.mse !== "none",
      mse: capabilityRef.current?.mse ?? "none",
      preferNativeHls: mode.engine === "native-hls",
      sourceBitrateBps: session.source?.bit_rate ?? null,
      startPositionS: Math.max(0, toSessionSeconds(pendingFileMsRef.current, mode.originMs)),
      telemetry: apiRef.current.telemetry,
      onFailed: (reason, cause) => {
        if (disposed) return;
        const stats = engine.stats();
        if (
          bandwidthRestartWanted({
            cause,
            videoAction: session.decision.video?.action,
            downlinkBps: stats.downlinkBps,
            bitrateBps: stats.bitrate,
            alreadyRestarted: bandwidthRestartedRef.current,
          })
        ) {
          bandwidthRestartedRef.current = true;
          reportPlaybackClientLog(
            "bandwidth-restart",
            {
              reason,
              downlink_bps: Math.round(stats.downlinkBps ?? 0),
              bitrate_bps: Math.round(stats.bitrate ?? 0),
            },
            apiRef.current,
          );
          freezeFrame();
          video.pause();
          wantsPlayRef.current = true;
          pendingFileMsRef.current = positionRef.current;
          dispatch({ type: "restart", startMs: positionRef.current });
          return;
        }
        const sourceBitrate = stats.bitrate ?? sourceBitrateRef.current;
        if (
          bandwidthDegradeWanted({
            cause,
            videoAction: session.decision.video?.action,
            downlinkBps: stats.downlinkBps,
            bitrateBps: sourceBitrate,
          })
        ) {
          lastDownlinkRef.current = stats.downlinkBps;
          reportPlaybackClientLog(
            "bandwidth-degrade",
            {
              reason,
              tier: session.decision.tier,
              downlink_bps: Math.round(stats.downlinkBps ?? 0),
              bitrate_bps: Math.round(sourceBitrate ?? 0),
            },
            apiRef.current,
          );
          flashNotice("线路带宽装不下原片码率，改用转码降码率播放");
          freezeFrame();
          video.pause();
          wantsPlayRef.current = true;
          pendingFileMsRef.current = positionRef.current;
          dispatch({ type: "bandwidth-degrade" });
          return;
        }
        dispatch({ type: "failed", reason });
      },
      onNetworkDead: () => {
        if (disposed) return;
        freezeFrame();
        video.pause();
        pendingFileMsRef.current = positionRef.current;
        dispatch({ type: "network-dead", startMs: positionRef.current });
      },
    });
    engineRef.current = engine;

    void engine
      .attach()
      .then(() => {
        if (disposed) {
          engine.destroy();
          return;
        }
        const target = toSessionSeconds(pendingFileMsRef.current, mode.originMs);
        if (shouldApplyPostAttachSeek(mode.engine, target)) {
          const seek = () => {
            if (Math.abs((video.currentTime || 0) - target) < 1) return;
            video.currentTime = target;
          };
          if (video.readyState >= 1) seek();
          else video.addEventListener("loadedmetadata", seek, { once: true });
        }
        void tryAutoplay();
      })
      .catch((error: unknown) => {
        if (disposed) return;
        dispatch({
          type: "fatal",
          message: error instanceof Error ? error.message : "播放引擎启动失败",
        });
      });

    return () => {
      disposed = true;
      engine.destroy();
      engineRef.current = null;
    };
  }, [
    apiRef,
    capabilityRef,
    dispatch,
    engineRef,
    flashNotice,
    freezeFrame,
    lastDownlinkRef,
    mode,
    pendingFileMsRef,
    positionRef,
    session,
    sourceBitrateRef,
    tryAutoplay,
    video,
    wantsPlayRef,
  ]);

  useEffect(() => {
    if (!video || !session) return;
    const isCurrentSession = () => activeSessionRef.current === session;
    const onPlaying = () => {
      if (!isCurrentSession()) return;
      setPaused(false);
      qoe({ type: "playing", at: performance.now() });
      dispatch({ type: "playing" });
    };
    const onPause = () => {
      if (isCurrentSession()) setPaused(true);
    };
    const onWaiting = () => {
      if (!isCurrentSession()) return;
      qoe({ type: "waiting", at: performance.now() });
      dispatch({ type: "buffering" });
    };
    const onSeeking = () => {
      if (!isCurrentSession()) return;
      qoe({ type: "seeking", at: performance.now() });
      dispatch({ type: "seeking" });
    };
    const onSeeked = () => {
      if (isCurrentSession()) qoe({ type: "seeked", at: performance.now() });
    };
    const onEnded = () => {
      if (isCurrentSession()) dispatch({ type: "ended" });
    };
    const onDurationChange = () => {
      if (!isCurrentSession()) return;
      setVideoDurationMs(
        Number.isFinite(video.duration) && video.duration > 0
          ? Math.round(video.duration * 1000)
          : null,
      );
    };
    const onBuffered = () => {
      if (!isCurrentSession()) return;
      setBufferedEndMs(toFileMs(video.currentTime + bufferedAhead(video), startMsRef.current));
    };
    const onTimeUpdate = () => {
      if (!isCurrentSession()) return;
      if (pendingCommitRef.current === null) {
        setPositionMs(toFileMs(video.currentTime, startMsRef.current));
      }
      onBuffered();
    };
    const onReady = () => {
      if (isCurrentSession()) void tryAutoplay();
    };

    video.addEventListener("playing", onPlaying);
    video.addEventListener("pause", onPause);
    video.addEventListener("waiting", onWaiting);
    video.addEventListener("seeking", onSeeking);
    video.addEventListener("seeked", onSeeked);
    video.addEventListener("ended", onEnded);
    video.addEventListener("durationchange", onDurationChange);
    video.addEventListener("timeupdate", onTimeUpdate);
    video.addEventListener("progress", onBuffered);
    video.addEventListener("loadedmetadata", onReady);
    video.addEventListener("canplay", onReady);
    return () => {
      video.removeEventListener("playing", onPlaying);
      video.removeEventListener("pause", onPause);
      video.removeEventListener("waiting", onWaiting);
      video.removeEventListener("seeking", onSeeking);
      video.removeEventListener("seeked", onSeeked);
      video.removeEventListener("ended", onEnded);
      video.removeEventListener("durationchange", onDurationChange);
      video.removeEventListener("timeupdate", onTimeUpdate);
      video.removeEventListener("progress", onBuffered);
      video.removeEventListener("loadedmetadata", onReady);
      video.removeEventListener("canplay", onReady);
    };
  }, [
    activeSessionRef,
    dispatch,
    pendingCommitRef,
    qoe,
    session,
    setPaused,
    setPositionMs,
    startMsRef,
    tryAutoplay,
    video,
  ]);

  return {
    bufferedEndMs,
    videoDurationMs,
    directShortRef,
    directHintShownRef,
  };
}
