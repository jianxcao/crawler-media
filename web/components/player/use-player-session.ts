"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
} from "react";

import {
  fetchPlaybackChapters,
  startPlaybackSession,
  type ClientCapability,
  type PlaybackApiScope,
  type PlaybackChapterMark,
  type PlaybackSession,
  type PlaybackUnit,
  type PlaybackWatchState,
} from "@/lib/api/playback";
import { planAudioOptions } from "@/lib/player/audio-tracks";
import { attemptAutoplay, shouldAttemptAutoplay, type AutoplayOutcome } from "@/lib/player/autoplay";
import { downlinkHintBps } from "@/lib/player/bandwidth";
import { getCapabilitySnapshot } from "@/lib/player/capability";
import { getPlayerDeviceId } from "@/lib/player/device";
import { preloadHlsEngine } from "@/lib/player/engine";
import { createInitialUnitStart } from "@/lib/player/initial-unit-start";
import type { PlayerEvent, PlayerState } from "@/lib/player/machine";
import { isEndedByAdminError, isStructuredHttpError } from "@/lib/player/page-lifecycle";
import { resolvePlaybackMode } from "@/lib/player/playback-mode";
import { loadQualityPreference, saveQualityPreference } from "@/lib/player/quality";
import type { QoeEvent } from "@/lib/player/qoe";
import {
  loadSubtitleStyle,
  pickInitialSubtitle,
  planSubtitleTracks,
  type SubtitleStyle,
} from "@/lib/player/subtitles";
import type { TrickplayIndex } from "@/lib/player/trickplay";

import type { CurrentPageRefValue, InFlightRenewalState, PageRestoreState } from "./player-types";

export function usePlayerSession({
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
  freezeFrame,
  trickplayRef,
  qoe,
  setPositionMs,
  pageGenerationRef,
  pageSuspendedRef,
  currentPageRef,
  pageRestoreRef,
  inFlightRenewalRef,
  endedByAdminRef,
  onEndedByAdmin,
}: {
  unitRef: MutableRefObject<PlaybackUnit>;
  unitKey: string;
  startMsOverride?: number;
  apiRef: MutableRefObject<PlaybackApiScope>;
  state: PlayerState;
  dispatch: Dispatch<PlayerEvent>;
  video: HTMLVideoElement | null;
  positionRef: MutableRefObject<number>;
  pendingFileMsRef: MutableRefObject<number>;
  reportedStartRef: MutableRefObject<string | null>;
  playSessionIdRef: MutableRefObject<string | null>;
  eventSequenceRef: MutableRefObject<number>;
  capabilityRef: MutableRefObject<ClientCapability | null>;
  wantsPlayRef: MutableRefObject<boolean>;
  autoplayAttemptsRef: MutableRefObject<number>;
  autoplayLastRef: MutableRefObject<AutoplayOutcome | null>;
  lastDownlinkRef: MutableRefObject<number | null>;
  clearPendingCommit: () => void;
  freezeFrame: (targetFileMs?: number) => void;
  trickplayRef: MutableRefObject<TrickplayIndex | null>;
  qoe: (event: QoeEvent) => void;
  setPositionMs: (ms: number) => void;
  pageGenerationRef: MutableRefObject<number>;
  pageSuspendedRef: MutableRefObject<boolean>;
  currentPageRef: MutableRefObject<CurrentPageRefValue>;
  pageRestoreRef: MutableRefObject<PageRestoreState | null>;
  inFlightRenewalRef: MutableRefObject<InFlightRenewalState | null>;
  endedByAdminRef: MutableRefObject<boolean>;
  onEndedByAdmin: () => void;
}) {
  const initialUnitStartRef = useRef<ReturnType<typeof createInitialUnitStart> | null>(null);
  if (initialUnitStartRef.current === null) {
    initialUnitStartRef.current = createInitialUnitStart(unitKey);
  }

  const [resume, setResume] = useState<PlaybackWatchState | null>(null);
  const [chapterCache, setChapterCache] = useState<{
    unitKey: string;
    fileId: string;
    chapters: PlaybackChapterMark[];
  } | null>(null);
  const [selectedSubtitle, setSelectedSubtitle] = useState<string | null>(null);
  const [requestedAudio, setRequestedAudio] = useState<string | null>(null);
  const [requestedSubtitle, setRequestedSubtitle] = useState<string | null>(null);
  const subtitleTouchedRef = useRef(false);
  const [subtitleStyle, setSubtitleStyle] = useState<SubtitleStyle>(loadSubtitleStyle);
  const [quality, setQuality] = useState<number | null>(loadQualityPreference);
  const [trickplay, setTrickplay] = useState<TrickplayIndex | null>(null);
  trickplayRef.current = trickplay;
  const [autoplay, setAutoplay] = useState<AutoplayOutcome | null>(null);

  const startedKeyRef = useRef<string | null>(null);
  const consentGrantedRef = useRef(false);

  const mode = useMemo(
    () =>
      state.session && capabilityRef.current
        ? resolvePlaybackMode(state.session, capabilityRef.current)
        : null,
    [capabilityRef, state.session],
  );
  const systemSubtitles = mode?.subtitleRenderer === "system-track";
  const failedKey = state.failedTiers.join(",");

  useEffect(() => {
    const session = state.session;
    if (
      !session?.chapters_pending ||
      !session.file_id ||
      !session.play_session_id ||
      apiRef.current.progress !== "server"
    ) {
      return;
    }
    let active = true;
    let attempts = 0;
    let timer: number | null = null;
    const fileId = session.file_id;
    const playSessionId = session.play_session_id;
    const poll = async () => {
      attempts += 1;
      try {
        const result = await fetchPlaybackChapters(playSessionId, apiRef.current);
        if (!active) return;
        if (result.ready) {
          setChapterCache({ unitKey, fileId, chapters: result.chapters });
          return;
        }
      } catch {
        // Ignore transient polling failures.
      }
      if (active && attempts < 30) timer = window.setTimeout(() => void poll(), 2000);
    };
    void poll();
    return () => {
      active = false;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [apiRef, state.session, unitKey]);

  const chapters = useMemo(() => {
    if (chapterCache?.unitKey === unitKey && chapterCache.fileId === state.session?.file_id) {
      return chapterCache.chapters;
    }
    return state.session?.chapters ?? [];
  }, [chapterCache, state.session, unitKey]);

  const subtitles = useMemo(() => {
    const session = state.session;
    if (!session) return { options: [], unavailable: [] };
    return planSubtitleTracks(session.decision.subtitles, session.subtitle_urls);
  }, [state.session]);

  const burnedSubtitle = state.session?.decision.video?.burn_subtitle ?? null;

  const audioOptions = useMemo(
    () =>
      state.session?.decision.tier === 0
        ? []
        : planAudioOptions(state.session?.decision.audio_tracks),
    [state.session],
  );

  const activeSubtitle = useMemo(
    () => subtitles.options.find((option) => option.ref === selectedSubtitle) ?? null,
    [subtitles, selectedSubtitle],
  );

  const pipSubtitleUrl = useMemo(() => {
    if (!mode?.pipPatchTrack) return null;
    if (!activeSubtitle) return null;
    if (activeSubtitle.ref === burnedSubtitle) return null;
    if (activeSubtitle.kind === "vtt") return activeSubtitle.url;
    if (activeSubtitle.kind === "ass") return `${activeSubtitle.url}&format=vtt`;
    return null;
  }, [activeSubtitle, burnedSubtitle, mode?.pipPatchTrack]);

  useEffect(() => {
    preloadHlsEngine();
  }, []);

  useEffect(() => {
    startedKeyRef.current = null;
    reportedStartRef.current = null;
    playSessionIdRef.current = null;
    eventSequenceRef.current = 0;
    pageRestoreRef.current = null;
    inFlightRenewalRef.current = null;
    pageSuspendedRef.current = false;
    pageGenerationRef.current += 1;
    setPositionMs(0);
    setResume(null);
    setSelectedSubtitle(null);
    setRequestedAudio(null);
    setRequestedSubtitle(null);
    subtitleTouchedRef.current = false;
    setTrickplay(null);
    wantsPlayRef.current = true;
    autoplayAttemptsRef.current = 0;
    autoplayLastRef.current = null;
    endedByAdminRef.current = false;
    consentGrantedRef.current = false;
    setAutoplay(null);
    dispatch({ type: "reset" });

    const override = initialUnitStartRef.current?.(unitKey, startMsOverride);
    if (override !== undefined) {
      setPositionMs(override);
      pendingFileMsRef.current = override;
    }
    dispatch({ type: "request", startMs: override ?? null });
  }, [
    autoplayAttemptsRef,
    autoplayLastRef,
    dispatch,
    endedByAdminRef,
    eventSequenceRef,
    inFlightRenewalRef,
    pageGenerationRef,
    pageRestoreRef,
    pageSuspendedRef,
    pendingFileMsRef,
    playSessionIdRef,
    reportedStartRef,
    setPositionMs,
    startMsOverride,
    unitKey,
    wantsPlayRef,
  ]);

  useEffect(() => {
    const phase = state.phase;
    if (pageSuspendedRef.current) return;
    if (phase !== "deciding" && phase !== "degrading" && phase !== "session-starting") return;

    const key = `${unitKey}|${phase}|${state.startMs}|${failedKey}|${state.failureCount}|${state.attempt}|${requestedAudio ?? ""}|${requestedSubtitle ?? ""}|${quality ?? ""}`;
    if (startedKeyRef.current === key) return;
    startedKeyRef.current = key;

    let cancelled = false;
    const pageGeneration = pageGenerationRef.current;
    const attempt = state.attempt;
    const requestDeviceId = getPlayerDeviceId();
    const restore = pageRestoreRef.current?.unitKey === unitKey ? pageRestoreRef.current : null;
    const obsolete = () =>
      cancelled ||
      pageSuspendedRef.current ||
      pageGeneration !== pageGenerationRef.current ||
      currentPageRef.current.unitKey !== unitKey ||
      currentPageRef.current.state.attempt !== attempt;
    const promotePredecessor = (fromSid: string, nextSid: string) => {
      if (
        !endedByAdminRef.current &&
        apiRef.current.progress === "server" &&
        currentPageRef.current.unitKey === unitKey &&
        getPlayerDeviceId() === requestDeviceId &&
        pageRestoreRef.current?.unitKey === unitKey &&
        pageRestoreRef.current.playSessionId === fromSid &&
        (!pageRestoreRef.current.deviceId || pageRestoreRef.current.deviceId === requestDeviceId)
      ) {
        pageRestoreRef.current = {
          ...pageRestoreRef.current,
          playSessionId: nextSid,
          renewedFromPlaySessionId:
            pageRestoreRef.current.renewedFromPlaySessionId ?? fromSid,
        };
      }
    };
    void (async () => {
      try {
        if (!capabilityRef.current) capabilityRef.current = await getCapabilitySnapshot();
        if (obsolete()) return;
        let predecessorSid =
          apiRef.current.progress === "server" ? (restore?.playSessionId ?? null) : null;
        if (
          predecessorSid &&
          inFlightRenewalRef.current &&
          inFlightRenewalRef.current.unitKey === unitKey &&
          inFlightRenewalRef.current.deviceId === requestDeviceId &&
          inFlightRenewalRef.current.predecessorSid === predecessorSid
        ) {
          const inFlight = inFlightRenewalRef.current;
          try {
            const priorSession = await inFlight.promise;
            if (obsolete()) return;
            if (priorSession.play_session_id && getPlayerDeviceId() === requestDeviceId) {
              promotePredecessor(predecessorSid, priorSession.play_session_id);
              predecessorSid = priorSession.play_session_id;
            }
          } catch (priorError) {
            if (obsolete()) return;
            if (isStructuredHttpError(priorError)) throw priorError;
          }
        }
        pendingFileMsRef.current = state.startMs ?? 0;
        qoe({ type: "play-requested", at: performance.now() });
        const requestPromise = startPlaybackSession(
          {
            ...unitRef.current,
            resume_from_play_session_id: predecessorSid ?? undefined,
            capability: capabilityRef.current,
            failed_tiers: state.failedTiers,
            start_ms: state.startMs ?? undefined,
            max_height: quality ?? undefined,
            audio_track: restore?.audioTrack ?? requestedAudio ?? undefined,
            subtitle_track: restore?.subtitleTrack ?? requestedSubtitle ?? undefined,
            downlink_bps: downlinkHintBps(lastDownlinkRef.current),
          },
          apiRef.current,
        );
        if (predecessorSid) {
          inFlightRenewalRef.current = {
            unitKey,
            deviceId: requestDeviceId,
            predecessorSid,
            promise: requestPromise,
          };
        }
        let session: PlaybackSession;
        try {
          session = await requestPromise;
        } finally {
          if (inFlightRenewalRef.current?.promise === requestPromise) {
            inFlightRenewalRef.current = null;
          }
        }
        if (obsolete()) {
          if (predecessorSid && session.play_session_id) {
            promotePredecessor(predecessorSid, session.play_session_id);
          }
          return;
        }
        pageRestoreRef.current = null;
        playSessionIdRef.current = session.play_session_id;
        eventSequenceRef.current = 0;
        if (session.decision.outcome === "consent") {
          if (consentGrantedRef.current) {
            dispatch({
              type: "fatal",
              message: "软件转码开关已保存，但服务端仍在请求开启确认",
              suggestion:
                "开关可能没有生效（例如服务端刚重启）。请刷新页面重试；若反复出现，请查看服务端日志排查。",
            });
            return;
          }
        } else {
          consentGrantedRef.current = false;
        }
        if (session.resume_status === "source_changed") {
          pendingFileMsRef.current = session.start_ms;
          setPositionMs(session.start_ms);
          subtitleTouchedRef.current = false;
          setSelectedSubtitle(null);
          setRequestedAudio(null);
          setRequestedSubtitle(null);
          setResume(session.watch ?? null);
        } else {
          if (state.startMs === null) {
            pendingFileMsRef.current = session.start_ms;
            setPositionMs(session.start_ms);
          }
          if (session.watch) setResume(session.watch);
        }
        dispatch({ type: "session", session });
      } catch (error) {
        if (obsolete()) return;
        pageRestoreRef.current = null;
        if (isEndedByAdminError(error)) {
          onEndedByAdmin();
          return;
        }
        dispatch({
          type: "fatal",
          message: error instanceof Error ? error.message : "播放启动失败",
        });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [
    apiRef,
    capabilityRef,
    currentPageRef,
    dispatch,
    endedByAdminRef,
    eventSequenceRef,
    failedKey,
    inFlightRenewalRef,
    lastDownlinkRef,
    onEndedByAdmin,
    pageGenerationRef,
    pageRestoreRef,
    pageSuspendedRef,
    pendingFileMsRef,
    playSessionIdRef,
    qoe,
    quality,
    requestedAudio,
    requestedSubtitle,
    setPositionMs,
    state.attempt,
    state.failedTiers,
    state.failureCount,
    state.phase,
    state.startMs,
    unitKey,
    unitRef,
  ]);

  const tryAutoplay = useCallback(async () => {
    if (!video || pageSuspendedRef.current) return;
    if (
      !shouldAttemptAutoplay({
        wanted: wantsPlayRef.current,
        paused: video.paused,
        ended: video.ended,
        attempts: autoplayAttemptsRef.current,
        last: autoplayLastRef.current,
      })
    ) {
      return;
    }
    autoplayAttemptsRef.current += 1;
    const outcome = await attemptAutoplay(video);
    autoplayLastRef.current = outcome;
    if (outcome !== "interrupted") setAutoplay(outcome);
  }, [autoplayAttemptsRef, autoplayLastRef, pageSuspendedRef, video, wantsPlayRef]);

  useEffect(() => {
    if (!state.session) return;
    clearPendingCommit();
    autoplayAttemptsRef.current = 0;
    autoplayLastRef.current = null;
    setAutoplay(null);
  }, [autoplayAttemptsRef, autoplayLastRef, clearPendingCommit, state.session]);

  useEffect(() => {
    if (subtitles.options.length === 0) return;
    if (subtitleTouchedRef.current) return;
    if (burnedSubtitle) {
      setSelectedSubtitle(burnedSubtitle);
      return;
    }
    setSelectedSubtitle(pickInitialSubtitle(subtitles.options, resume?.subtitle_track ?? null));
  }, [subtitles, resume?.subtitle_track, burnedSubtitle]);

  const burnFallbackRef = useRef(false);
  useEffect(() => {
    if (state.phase !== "consent") {
      burnFallbackRef.current = false;
      return;
    }
    if (burnFallbackRef.current) return;
    if (requestedSubtitle && requestedSubtitle !== "off") {
      burnFallbackRef.current = true;
      setRequestedSubtitle("off");
      dispatch({ type: "reset" });
      dispatch({ type: "request", startMs: positionRef.current });
    }
  }, [dispatch, positionRef, requestedSubtitle, state.phase]);

  const togglePlay = useCallback(() => {
    if (!video) return;
    if (video.paused) {
      wantsPlayRef.current = true;
      autoplayLastRef.current = null;
      setAutoplay(null);
      void video.play().catch(() => undefined);
    } else {
      wantsPlayRef.current = false;
      video.pause();
    }
  }, [autoplayLastRef, video, wantsPlayRef]);

  const selectAudio = useCallback(
    (ref: string) => {
      setRequestedAudio(ref);
      if (ref === (state.session?.decision.audio?.track_ref ?? null)) return;
      freezeFrame();
      video?.pause();
      wantsPlayRef.current = true;
      pendingFileMsRef.current = positionRef.current;
      dispatch({ type: "request", startMs: positionRef.current });
    },
    [dispatch, freezeFrame, pendingFileMsRef, positionRef, state.session, video, wantsPlayRef],
  );

  const selectSubtitle = useCallback(
    (ref: string | null) => {
      subtitleTouchedRef.current = true;
      setSelectedSubtitle(ref);
      const target = ref ? subtitles.options.find((o) => o.ref === ref) : null;
      const wantBurn = target?.kind === "pgs";
      if (!wantBurn && burnedSubtitle === null) return;
      if (wantBurn && burnedSubtitle === ref) return;
      setRequestedSubtitle(ref ?? "off");
      freezeFrame();
      video?.pause();
      wantsPlayRef.current = true;
      pendingFileMsRef.current = positionRef.current;
      dispatch({ type: "request", startMs: positionRef.current });
    },
    [burnedSubtitle, dispatch, freezeFrame, pendingFileMsRef, positionRef, subtitles, video, wantsPlayRef],
  );

  const selectQuality = useCallback(
    (maxHeight: number | null) => {
      setQuality(maxHeight);
      saveQualityPreference(maxHeight);
      if (maxHeight === quality) return;
      const copying = state.session?.decision.video?.action === "copy";
      if (copying && (maxHeight === null || (video?.videoHeight ?? 0) <= maxHeight)) return;
      freezeFrame();
      video?.pause();
      wantsPlayRef.current = true;
      pendingFileMsRef.current = positionRef.current;
      dispatch({ type: "request", startMs: positionRef.current });
    },
    [dispatch, freezeFrame, pendingFileMsRef, positionRef, quality, state.session, video, wantsPlayRef],
  );

  return {
    resume,
    mode,
    systemSubtitles,
    chapters,
    subtitles,
    burnedSubtitle,
    audioOptions,
    activeSubtitle,
    pipSubtitleUrl,
    selectedSubtitle,
    setSelectedSubtitle,
    subtitleStyle,
    setSubtitleStyle,
    quality,
    trickplay,
    autoplay,
    consentGrantedRef,
    tryAutoplay,
    togglePlay,
    selectAudio,
    selectSubtitle,
    selectQuality,
  };
}
