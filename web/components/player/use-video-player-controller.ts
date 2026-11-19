"use client";

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";

import {
  DEFAULT_PLAYBACK_SCOPE,
  type ClientCapability,
  type PlaybackApiScope,
} from "@/lib/api/playback";
import type { AutoplayOutcome } from "@/lib/player/autoplay";
import { chromeMustStayVisible } from "@/lib/player/chrome";
import type { PlaybackEngine } from "@/lib/player/engine";
import { awaitsUserDecision, initialPlayerState, isBusy, playerReducer } from "@/lib/player/machine";
import { unitKeyOf } from "@/lib/player/player-ui";
import { initialQoe, liveStats, reduceQoe, type QoeEvent } from "@/lib/player/qoe";
import { saveSubtitleStyle, type SubtitleStyle } from "@/lib/player/subtitles";
import { isInEndCredits } from "@/lib/player/timeline";
import type { TrickplayIndex } from "@/lib/player/trickplay";

import { IDLE_HIDE_MS, type VideoPlayerProps } from "./player-types";
import { useFreezeFrame } from "./use-freeze-frame";
import { usePlayerEngine } from "./use-player-engine";
import { usePlayerFeedback } from "./use-player-feedback";
import { usePlayerGestures } from "./use-player-gestures";
import { usePlayerSeek } from "./use-player-seek";
import { usePlayerSession } from "./use-player-session";
import { usePlayerSystemIntegration } from "./use-player-system-integration";
import { usePlayerTelemetry } from "./use-player-telemetry";
import { usePlayerViewport } from "./use-player-viewport";

export function useVideoPlayerController(props: VideoPlayerProps) {
  const {
    unit,
    title,
    episodeLabel,
    posterUrl,
    startMsOverride,
    next,
    prev,
    onPlayNext,
    onPlayPrev,
    onExit,
    api = DEFAULT_PLAYBACK_SCOPE,
  } = props;

  const unitKey = unitKeyOf(unit);
  const unitRef = useRef(unit);
  unitRef.current = unit;
  const apiRef = useRef<PlaybackApiScope>(api);
  apiRef.current = api;

  const [state, dispatch] = useReducer(playerReducer, initialPlayerState);
  const [video, setVideo] = useState<HTMLVideoElement | null>(null);
  const [positionMs, setPositionMs] = useState(0);
  const [paused, setPaused] = useState(true);
  const [diagnosticsOpen, setDiagnosticsOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [scrubbing, setScrubbing] = useState(false);
  const [chromeVisible, setChromeVisible] = useState(true);
  const [chromeActivity, setChromeActivity] = useState(0);
  const lastActivityBumpRef = useRef(0);
  const bumpChromeActivity = useCallback(() => {
    const now = performance.now();
    if (now - lastActivityBumpRef.current < 500) return;
    lastActivityBumpRef.current = now;
    setChromeActivity((n) => n + 1);
  }, []);
  const [nextDismissed, setNextDismissed] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const engineRef = useRef<PlaybackEngine | null>(null);
  const activeSessionRef = useRef(state.session);
  activeSessionRef.current = state.session;
  const capabilityRef = useRef<ClientCapability | null>(null);
  const pendingFileMsRef = useRef(0);
  const startMsRef = useRef(0);
  const positionRef = useRef(0);
  const durationMsRef = useRef<number | null>(null);
  const reportedStartRef = useRef<string | null>(null);
  const playSessionIdRef = useRef<string | null>(null);
  const eventSequenceRef = useRef(0);
  const wantsPlayRef = useRef(true);
  const autoplayAttemptsRef = useRef(0);
  const autoplayLastRef = useRef<AutoplayOutcome | null>(null);
  const lastDownlinkRef = useRef<number | null>(null);
  const sourceBitrateRef = useRef<number | null>(null);
  const trickplayRef = useRef<TrickplayIndex | null>(null);
  const qoeRef = useRef(initialQoe());
  const qoe = useCallback((event: QoeEvent) => {
    qoeRef.current = reduceQoe(qoeRef.current, event);
  }, []);

  const pendingCommitRef = useRef<number | null>(null);
  const pendingCommitTimerRef = useRef<number | null>(null);
  const clearPendingCommit = useCallback(() => {
    pendingCommitRef.current = null;
    if (pendingCommitTimerRef.current !== null) window.clearTimeout(pendingCommitTimerRef.current);
    pendingCommitTimerRef.current = null;
  }, []);

  const sessionId = state.session?.session_id ?? null;
  const sessionIdRef = useRef<string | null>(null);
  sessionIdRef.current = sessionId;

  const feedback = usePlayerFeedback();
  const freeze = useFreezeFrame({
    video,
    phase: state.phase,
    unitKey,
    trickplayRef,
    pendingCommitRef,
  });
  const session = usePlayerSession({
    unitRef,
    unitKey,
    startMsOverride,
    apiRef,
    state,
    dispatch,
    video,
    positionRef,
    pendingFileMsRef,
    reportedStartRef,
    playSessionIdRef,
    eventSequenceRef,
    capabilityRef,
    wantsPlayRef,
    autoplayAttemptsRef,
    autoplayLastRef,
    lastDownlinkRef,
    clearPendingCommit,
    freezeFrame: freeze.freezeFrame,
    trickplayRef,
    qoe,
    setPositionMs,
  });

  startMsRef.current = session.mode?.originMs ?? 0;
  positionRef.current = positionMs;

  const engine = usePlayerEngine({
    unitKey,
    startMsOverride,
    video,
    session: state.session,
    mode: session.mode,
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
    tryAutoplay: session.tryAutoplay,
    freezeFrame: freeze.freezeFrame,
    flashNotice: feedback.flashNotice,
    qoe,
    setPaused,
    setPositionMs,
    dispatch,
  });

  const durationMs = useMemo(() => {
    if (session.resume?.duration_ms) return session.resume.duration_ms;
    return sessionId ? null : engine.videoDurationMs;
  }, [session.resume?.duration_ms, sessionId, engine.videoDurationMs]);
  durationMsRef.current = durationMs;

  const seek = usePlayerSeek({
    unitKey,
    video,
    session: state.session,
    phase: state.phase,
    startMs: state.startMs,
    mode: session.mode,
    durationMs,
    engineRef,
    pendingFileMsRef,
    startMsRef,
    positionRef,
    durationMsRef,
    sessionIdRef,
    wantsPlayRef,
    pendingCommitRef,
    pendingCommitTimerRef,
    clearPendingCommit,
    freezeFrame: freeze.freezeFrame,
    qoe,
    bumpChromeActivity,
    setPositionMs,
    dispatch,
  });

  const telemetry = usePlayerTelemetry({
    unitRef,
    unitKey,
    startMsOverride,
    apiRef,
    video,
    session: state.session,
    phase: state.phase,
    selectedSubtitle: session.selectedSubtitle,
    engineRef,
    playSessionIdRef,
    eventSequenceRef,
    positionRef,
    reportedStartRef,
    lastDownlinkRef,
    sourceBitrateRef,
    directShortRef: engine.directShortRef,
    directHintShownRef: engine.directHintShownRef,
    qoeRef,
    qoe,
    flashNotice: feedback.flashNotice,
    dispatch,
  });

  const viewport = usePlayerViewport({
    video,
    containerRef,
    flashNotice: feedback.flashNotice,
    onExit,
  });

  const { cueFontPx, cueLiftPx } = usePlayerSystemIntegration({
    video,
    mode: session.mode,
    subtitles: session.subtitles,
    activeSubtitle: session.activeSubtitle,
    pipSubtitleUrl: session.pipSubtitleUrl,
    systemSubtitles: session.systemSubtitles,
    chromeVisible,
    positionMs,
    durationMs,
    paused,
    phase: state.phase,
    title,
    episodeLabel,
    posterUrl,
    next,
    prev,
    onPlayNext,
    onPlayPrev,
    togglePlay: session.togglePlay,
    seekBy: seek.seekBy,
    commitSeekRef: seek.commitSeekRef,
  });

  const gestures = usePlayerGestures({
    video,
    paused,
    chromeVisible,
    setChromeVisible,
    bumpChromeActivity,
    fakeLandscape: viewport.fakeLandscape,
    durationMs,
    durationMsRef,
    positionRef,
    frameRate: state.session?.source?.frame_rate,
    subtitlesOptions: session.subtitles.options,
    setSelectedSubtitle: session.setSelectedSubtitle,
    togglePlay: session.togglePlay,
    seekBy: seek.seekBy,
    seekByRef: seek.seekByRef,
    commitSeek: seek.commitSeek,
    cancelPendingSeek: seek.cancelPendingSeek,
    toggleFullscreen: viewport.toggleFullscreen,
    showSeekPreview: feedback.showSeekPreview,
    hideSeekPreview: feedback.hideSeekPreview,
    flashSeek: feedback.flashSeek,
    flashAdjust: feedback.flashAdjust,
    flashNotice: feedback.flashNotice,
  });

  useEffect(() => {
    setNextDismissed(false);
  }, [unitKey]);

  const awaitingUser = awaitsUserDecision(state.phase);
  useEffect(() => {
    if (gestures.locked) return;
    if (chromeMustStayVisible({ paused, menuOpen, awaitingUser, scrubbing })) {
      setChromeVisible(true);
      return;
    }
    const timer = window.setTimeout(() => setChromeVisible(false), IDLE_HIDE_MS);
    return () => window.clearTimeout(timer);
  }, [gestures.locked, paused, menuOpen, awaitingUser, scrubbing, chromeVisible, chromeActivity]);

  const showNextCard =
    next !== null &&
    !nextDismissed &&
    (isInEndCredits(positionMs, durationMs) || state.phase === "ended");
  const busy = isBusy(state.phase) && session.autoplay !== "blocked";
  const showPauseOverlay =
    paused &&
    !busy &&
    !wantsPlayRef.current &&
    state.phase !== "error" &&
    state.phase !== "consent" &&
    positionMs > 0;
  const overrideMs =
    feedback.seekPreview && !feedback.seekPreview.leaving
      ? feedback.seekPreview.targetMs
      : seek.pendingSeekMs;

  const { setSubtitleStyle } = session;
  const handleSubtitleStyleChange = useCallback(
    (style: SubtitleStyle) => {
      setSubtitleStyle(style);
      saveSubtitleStyle(style);
    },
    [setSubtitleStyle],
  );
  const toggleDiagnostics = useCallback(() => setDiagnosticsOpen((open) => !open), []);
  const liveQoeStats = useCallback(() => liveStats(qoeRef.current), []);
  const retryFromCurrent = useCallback(
    () => dispatch({ type: "request", startMs: positionRef.current }),
    [],
  );

  return {
    containerRef,
    engineRef,
    state,
    dispatch,
    video,
    setVideo,
    positionMs,
    paused,
    durationMs,
    chromeVisible,
    setChromeVisible,
    setMenuOpen,
    setScrubbing,
    diagnosticsOpen,
    setDiagnosticsOpen,
    toggleDiagnostics,
    setNextDismissed,
    cueFontPx,
    cueLiftPx,
    showNextCard,
    busy,
    showPauseOverlay,
    overrideMs,
    handleSubtitleStyleChange,
    liveQoeStats,
    retryFromCurrent,
    feedback,
    freeze,
    session,
    engine,
    seek,
    telemetry,
    viewport,
    gestures,
  };
}
