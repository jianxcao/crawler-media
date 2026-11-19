"use client";

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
} from "react";

import type { PlaybackSession } from "@/lib/api/playback";
import type { PlaybackEngine } from "@/lib/player/engine";
import { isBusy, type PlayerEvent, type PlayerPhase } from "@/lib/player/machine";
import type { PlaybackMode } from "@/lib/player/playback-mode";
import type { QoeEvent } from "@/lib/player/qoe";
import {
  afterScrubFollow,
  initialScrubFollowState,
  planScrubFollow,
  seekAlreadyInFlight,
  type ScrubFollowState,
} from "@/lib/player/scrub-follow";
import { nextSeekTarget, seekBatchWindowMs } from "@/lib/player/seek-batch";
import { clampSeekTarget, isWithinRanges, planSeek, toFileMs, toSessionSeconds } from "@/lib/player/timeline";

import { PENDING_COMMIT_MAX_WAIT_MS } from "./player-types";

export function usePlayerSeek({
  unitKey,
  video,
  session,
  phase,
  startMs,
  mode,
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
  freezeFrame,
  qoe,
  bumpChromeActivity,
  setPositionMs,
  dispatch,
}: {
  unitKey: string;
  video: HTMLVideoElement | null;
  session: PlaybackSession | null;
  phase: PlayerPhase;
  startMs: number | null;
  mode: PlaybackMode | null;
  durationMs: number | null;
  engineRef: MutableRefObject<PlaybackEngine | null>;
  pendingFileMsRef: MutableRefObject<number>;
  startMsRef: MutableRefObject<number>;
  positionRef: MutableRefObject<number>;
  durationMsRef: MutableRefObject<number | null>;
  sessionIdRef: MutableRefObject<string | null>;
  wantsPlayRef: MutableRefObject<boolean>;
  pendingCommitRef: MutableRefObject<number | null>;
  pendingCommitTimerRef: MutableRefObject<number | null>;
  clearPendingCommit: () => void;
  freezeFrame: (targetFileMs?: number) => void;
  qoe: (event: QoeEvent) => void;
  bumpChromeActivity: () => void;
  setPositionMs: (ms: number) => void;
  dispatch: Dispatch<PlayerEvent>;
}) {
  const sessionId = session?.session_id ?? null;

  const [pendingSeekMs, setPendingSeekMs] = useState<number | null>(null);
  const pendingSeekRef = useRef<{ targetMs: number | null; timer: number | null }>({
    targetMs: null,
    timer: null,
  });
  const scrubRef = useRef<ScrubFollowState>(initialScrubFollowState());
  const scrubTimerRef = useRef<number | null>(null);

  const cancelPendingSeek = useCallback(() => {
    const pending = pendingSeekRef.current;
    if (pending.timer !== null) window.clearTimeout(pending.timer);
    pending.timer = null;
    pending.targetMs = null;
    setPendingSeekMs(null);
  }, []);

  const cancelScrubFollow = useCallback(() => {
    if (scrubTimerRef.current !== null) window.clearTimeout(scrubTimerRef.current);
    scrubTimerRef.current = null;
    scrubRef.current = { ...scrubRef.current, pendingMs: null };
  }, []);

  const flushPendingCommit = useCallback(() => {
    const commit = pendingCommitRef.current;
    if (commit === null || !video) return;
    clearPendingCommit();
    scrubRef.current = { ...scrubRef.current, pendingMs: null };
    if (engineRef.current?.seek) engineRef.current.seek(commit);
    else video.currentTime = commit;
    setPositionMs(toFileMs(commit, startMsRef.current));
  }, [clearPendingCommit, engineRef, pendingCommitRef, setPositionMs, startMsRef, video]);
  const flushPendingCommitRef = useRef(flushPendingCommit);
  flushPendingCommitRef.current = flushPendingCommit;

  const seekToFileMs = useCallback(
    (rawFileMs: number) => {
      if (!video) return;
      const fileMs = clampSeekTarget(rawFileMs, durationMs);
      if (!session) {
        if (!isBusy(phase) || startMs === null) return;
        qoe({ type: "seek-requested", at: performance.now() });
        pendingFileMsRef.current = fileMs;
        setPositionMs(fileMs);
        dispatch({ type: "restart", startMs: fileMs });
        return;
      }
      const seekable = video.seekable;
      const plan = planSeek(fileMs, {
        startMs: startMsRef.current,
        seekableEndSeconds: seekable.length ? seekable.end(seekable.length - 1) : 0,
        hasSession: Boolean(sessionId) && (mode?.seekBeyondBufferedRestarts ?? false),
      });
      if (plan.kind === "native") {
        const seconds = Math.max(0, plan.seconds);
        const inFlight = seekAlreadyInFlight({
          currentTimeSeconds: video.currentTime,
          targetSeconds: seconds,
        });
        if (inFlight && !video.seeking) {
          setPositionMs(toFileMs(seconds, startMsRef.current));
          return;
        }
        qoe({ type: "seek-requested", at: performance.now() });
        if (!isWithinRanges(video.buffered, seconds)) freezeFrame(fileMs);
        if (!inFlight) {
          if (video.seeking) {
            clearPendingCommit();
            pendingCommitRef.current = seconds;
            pendingCommitTimerRef.current = window.setTimeout(() => {
              pendingCommitTimerRef.current = null;
              flushPendingCommitRef.current();
            }, PENDING_COMMIT_MAX_WAIT_MS);
          } else if (engineRef.current?.seek) {
            engineRef.current.seek(seconds);
          } else {
            video.currentTime = seconds;
          }
        }
        setPositionMs(toFileMs(seconds, startMsRef.current));
        return;
      }
      qoe({ type: "seek-requested", at: performance.now() });
      freezeFrame(plan.startMs);
      video.pause();
      wantsPlayRef.current = true;
      pendingFileMsRef.current = plan.startMs;
      clearPendingCommit();
      setPositionMs(plan.startMs);
      dispatch({ type: "restart", startMs: plan.startMs });
    },
    [
      clearPendingCommit,
      dispatch,
      durationMs,
      engineRef,
      freezeFrame,
      mode,
      pendingCommitRef,
      pendingCommitTimerRef,
      pendingFileMsRef,
      phase,
      qoe,
      session,
      sessionId,
      setPositionMs,
      startMs,
      startMsRef,
      video,
      wantsPlayRef,
    ],
  );
  const seekToFileMsRef = useRef(seekToFileMs);
  seekToFileMsRef.current = seekToFileMs;

  const isCheapSeek = useCallback(
    (fileMs: number) => {
      if (!video) return false;
      return isWithinRanges(video.buffered, toSessionSeconds(fileMs, startMsRef.current));
    },
    [startMsRef, video],
  );
  const isCheapSeekRef = useRef(isCheapSeek);
  isCheapSeekRef.current = isCheapSeek;

  const canScrubFollow = useCallback(
    (fileMs: number) => mode?.engine === "direct" || isCheapSeek(fileMs),
    [mode, isCheapSeek],
  );
  const canScrubFollowRef = useRef(canScrubFollow);
  canScrubFollowRef.current = canScrubFollow;

  const seekBy = useCallback(
    (seconds: number) => {
      const target = nextSeekTarget({
        pendingMs: pendingSeekRef.current.targetMs,
        positionMs: positionRef.current,
        deltaMs: seconds * 1000,
        durationMs: durationMsRef.current,
      });
      const windowMs = seekBatchWindowMs({
        hasSession: Boolean(sessionIdRef.current),
        buffered: isCheapSeekRef.current(target),
      });
      if (windowMs <= 0) {
        cancelPendingSeek();
        seekToFileMs(target);
        return;
      }
      const pending = pendingSeekRef.current;
      pending.targetMs = target;
      setPendingSeekMs(target);
      if (pending.timer !== null) window.clearTimeout(pending.timer);
      pending.timer = window.setTimeout(() => {
        pending.timer = null;
        const committed = pending.targetMs;
        pending.targetMs = null;
        setPendingSeekMs(null);
        if (committed !== null) seekToFileMsRef.current(committed);
      }, windowMs);
    },
    [cancelPendingSeek, durationMsRef, positionRef, seekToFileMs, sessionIdRef],
  );
  const seekByRef = useRef(seekBy);
  seekByRef.current = seekBy;

  useEffect(() => {
    const pending = pendingSeekRef.current;
    cancelPendingSeek();
    cancelScrubFollow();
    clearPendingCommit();
    return () => {
      if (pending.timer !== null) window.clearTimeout(pending.timer);
      if (scrubTimerRef.current !== null) window.clearTimeout(scrubTimerRef.current);
    };
  }, [unitKey, cancelPendingSeek, cancelScrubFollow, clearPendingCommit]);

  const applyScrubFollow = useCallback(
    (rawFileMs: number) => {
      if (!video) return;
      const fileMs = clampSeekTarget(rawFileMs, durationMs);
      const seconds = toSessionSeconds(fileMs, startMsRef.current);
      if (seconds < 0 || !canScrubFollowRef.current(fileMs)) return;
      if (video.seeking) {
        scrubRef.current = { ...scrubRef.current, pendingMs: fileMs };
        return;
      }
      scrubRef.current = afterScrubFollow(performance.now());
      if (isCheapSeekRef.current(fileMs) && typeof video.fastSeek === "function") {
        video.fastSeek(seconds);
      } else {
        video.currentTime = seconds;
      }
    },
    [durationMs, startMsRef, video],
  );
  const applyScrubFollowRef = useRef(applyScrubFollow);
  applyScrubFollowRef.current = applyScrubFollow;

  useEffect(() => {
    if (!video) return;
    const onLanded = () => {
      if (video.seeking) return;
      if (pendingCommitRef.current !== null) {
        flushPendingCommitRef.current();
        return;
      }
      const pending = scrubRef.current.pendingMs;
      if (pending === null || scrubTimerRef.current !== null) return;
      applyScrubFollowRef.current(pending);
    };
    video.addEventListener("timeupdate", onLanded);
    video.addEventListener("seeked", onLanded);
    return () => {
      video.removeEventListener("timeupdate", onLanded);
      video.removeEventListener("seeked", onLanded);
    };
  }, [pendingCommitRef, video]);

  const scrubTo = useCallback(
    (fileMs: number) => {
      bumpChromeActivity();
      if (!video) return;
      const plan = planScrubFollow({
        now: performance.now(),
        state: scrubRef.current,
        cheap: isCheapSeek(fileMs),
        reachable: toSessionSeconds(fileMs, startMsRef.current) >= 0,
        settleOnly: mode?.engine === "direct",
      });
      if (plan.kind === "skip") return;
      if (plan.kind === "follow") {
        cancelScrubFollow();
        applyScrubFollow(fileMs);
        return;
      }
      scrubRef.current = { ...scrubRef.current, pendingMs: fileMs };
      if (scrubTimerRef.current !== null) window.clearTimeout(scrubTimerRef.current);
      scrubTimerRef.current = window.setTimeout(() => {
        scrubTimerRef.current = null;
        const target = scrubRef.current.pendingMs;
        if (target !== null) applyScrubFollowRef.current(target);
      }, plan.delayMs);
    },
    [applyScrubFollow, bumpChromeActivity, cancelScrubFollow, isCheapSeek, mode, startMsRef, video],
  );

  const commitSeek = useCallback(
    (fileMs: number) => {
      cancelPendingSeek();
      cancelScrubFollow();
      seekToFileMs(fileMs);
    },
    [cancelPendingSeek, cancelScrubFollow, seekToFileMs],
  );
  const commitSeekRef = useRef(commitSeek);
  commitSeekRef.current = commitSeek;

  return {
    pendingSeekMs,
    cancelPendingSeek,
    cancelScrubFollow,
    seekBy,
    seekByRef,
    scrubTo,
    commitSeek,
    commitSeekRef,
  };
}
