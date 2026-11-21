"use client";

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";

import { shouldHideOnPointerLeave } from "@/lib/player/chrome";
import {
  canHoldSpeed,
  HOLD_SPEED_DELAY_MS,
  HOLD_SPEED_RATE,
  holdSpeedReducer,
  type HoldSpeedEvent,
  type HoldSpeedState,
} from "@/lib/player/hold-speed";
import { FALLBACK_FRAME_RATE, isEditableTarget, resolveShortcut } from "@/lib/player/shortcuts";
import type { SubtitleOption } from "@/lib/player/subtitles";
import { resolveTap } from "@/lib/player/tap";
import { clampSeekTarget } from "@/lib/player/timeline";
import {
  applySwipe,
  classifyIntent,
  classifyTouchZone,
  EDGE_GUARD_PX,
  pointerOffsetX,
  seekDeltaMs,
  toLayoutPoint,
  type AdjustKind,
  type SwipeIntent,
} from "@/lib/player/touch-adjust";

export function usePlayerGestures({
  video,
  paused,
  chromeVisible,
  setChromeVisible,
  bumpChromeActivity,
  fakeLandscape,
  durationMs,
  durationMsRef,
  positionRef,
  frameRate,
  subtitlesOptions,
  setSelectedSubtitle,
  togglePlay,
  seekBy,
  seekByRef,
  commitSeek,
  cancelPendingSeek,
  toggleFullscreen,
  showSeekPreview,
  hideSeekPreview,
  flashSeek,
  flashAdjust,
  flashNotice,
}: {
  video: HTMLVideoElement | null;
  paused: boolean;
  chromeVisible: boolean;
  setChromeVisible: Dispatch<SetStateAction<boolean>>;
  bumpChromeActivity: () => void;
  fakeLandscape: boolean;
  durationMs: number | null;
  durationMsRef: MutableRefObject<number | null>;
  positionRef: MutableRefObject<number>;
  frameRate?: number | null;
  subtitlesOptions: SubtitleOption[];
  setSelectedSubtitle: Dispatch<SetStateAction<string | null>>;
  togglePlay: () => void;
  seekBy: (seconds: number) => void;
  seekByRef: MutableRefObject<(seconds: number) => void>;
  commitSeek: (fileMs: number) => void;
  cancelPendingSeek: () => void;
  toggleFullscreen: () => void;
  showSeekPreview: (targetMs: number, deltaMs: number) => void;
  hideSeekPreview: () => void;
  flashSeek: (seconds: number) => void;
  flashAdjust: (next: {
    kind: AdjustKind;
    value: number;
    unsupported?: boolean;
    muted?: boolean;
  }) => void;
  flashNotice: (text: string) => void;
}) {
  const [locked, setLocked] = useState(false);
  const [lockHint, setLockHint] = useState(false);
  const lockHintTimerRef = useRef<number | null>(null);
  const [brightness, setBrightness] = useState(1);
  const [holdSpeed, setHoldSpeed] = useState(false);

  const lockedRef = useRef(false);
  lockedRef.current = locked;
  const suppressClickRef = useRef(false);
  const tapRef = useRef<{ at: number; chromeBefore: boolean } | null>(null);
  const chromeWasVisibleRef = useRef(true);
  const lastPointerTypeRef = useRef("");
  const swipeGestureRef = useRef<{
    kind: AdjustKind;
    startX: number;
    startY: number;
    startValue: number;
    startPositionMs: number;
    intent: SwipeIntent | null;
    targetMs: number;
  } | null>(null);
  const holdRef = useRef<{ state: HoldSpeedState; timer: number | null; rate: number }>({
    state: "idle",
    timer: null,
    rate: 1,
  });
  const volumeAdjustableRef = useRef<boolean | null>(null);
  const fakeLandscapeRef = useRef(false);
  fakeLandscapeRef.current = fakeLandscape;
  const brightnessRef = useRef(1);
  brightnessRef.current = brightness;

  const revealLock = useCallback(() => {
    setLockHint(true);
    if (lockHintTimerRef.current !== null) window.clearTimeout(lockHintTimerRef.current);
    lockHintTimerRef.current = window.setTimeout(() => {
      lockHintTimerRef.current = null;
      setLockHint(false);
    }, 3000);
  }, []);

  useEffect(() => {
    if (locked) setChromeVisible(false);
  }, [locked, setChromeVisible]);

  useEffect(
    () => () => {
      if (lockHintTimerRef.current !== null) window.clearTimeout(lockHintTimerRef.current);
    },
    [],
  );

  const onSurfaceClick = useCallback(
    (event: React.MouseEvent<HTMLVideoElement>) => {
      if (lockedRef.current) {
        revealLock();
        return;
      }
      if (suppressClickRef.current) {
        suppressClickRef.current = false;
        return;
      }
      const { offset, length } = pointerOffsetX(
        event,
        event.currentTarget.getBoundingClientRect(),
        fakeLandscapeRef.current,
      );
      const xRatio = length > 0 ? offset / length : 0.5;
      const now = performance.now();
      const previous = tapRef.current;
      const action =
        lastPointerTypeRef.current === "mouse"
          ? ({ type: "chrome" } as const)
          : resolveTap({ nowMs: now, lastTapMs: previous?.at ?? null, xRatio });
      if (action.type === "seek") {
        if (previous) setChromeVisible(previous.chromeBefore);
        tapRef.current = { at: now, chromeBefore: previous?.chromeBefore ?? chromeVisible };
        seekByRef.current(action.seconds);
        flashSeek(action.seconds);
        return;
      }
      tapRef.current = { at: now, chromeBefore: chromeWasVisibleRef.current };
      setChromeVisible(!chromeWasVisibleRef.current);
    },
    [chromeVisible, flashSeek, revealLock, seekByRef, setChromeVisible],
  );

  const onSurfaceDoubleClick = useCallback(() => {
    if (lastPointerTypeRef.current !== "mouse") return;
    toggleFullscreen();
  }, [toggleFullscreen]);

  const onContainerPointerMove = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (event.pointerType === "mouse") {
        setChromeVisible(true);
        bumpChromeActivity();
      }
    },
    [bumpChromeActivity, setChromeVisible],
  );

  const onContainerPointerDown = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      chromeWasVisibleRef.current = chromeVisible;
      lastPointerTypeRef.current = event.pointerType;
      if (chromeVisible) bumpChromeActivity();
    },
    [bumpChromeActivity, chromeVisible],
  );

  const onContainerPointerLeave = useCallback(
    (event: React.PointerEvent<HTMLDivElement>) => {
      if (shouldHideOnPointerLeave({ pointerType: event.pointerType, paused })) {
        setChromeVisible(false);
      }
    },
    [paused, setChromeVisible],
  );

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const action = resolveShortcut({
        key: event.key,
        inEditable: isEditableTarget(event.target),
        ctrlKey: event.ctrlKey,
        metaKey: event.metaKey,
        altKey: event.altKey,
      });
      if (!action) return;
      event.preventDefault();
      setChromeVisible(true);
      bumpChromeActivity();
      switch (action.type) {
        case "toggle-play":
          togglePlay();
          break;
        case "seek-by":
          seekBy(action.seconds);
          flashSeek(action.seconds);
          break;
        case "seek-percent":
          if (durationMs) commitSeek((durationMs * action.percent) / 100);
          break;
        case "volume-by":
          if (video) {
            video.volume = Math.min(1, Math.max(0, video.volume + action.delta));
            flashAdjust({ kind: "volume", value: video.volume, muted: video.muted });
          }
          break;
        case "toggle-mute":
          if (video) {
            video.muted = !video.muted;
            flashAdjust({ kind: "volume", value: video.volume, muted: video.muted });
          }
          break;
        case "toggle-fullscreen":
          toggleFullscreen();
          break;
        case "step-frame":
          if (video?.paused) {
            cancelPendingSeek();
            const fps = frameRate || FALLBACK_FRAME_RATE;
            video.currentTime = Math.max(0, video.currentTime + action.direction / fps);
          }
          break;
        case "toggle-subtitles":
          setSelectedSubtitle((current) =>
            current ? null : (subtitlesOptions[0]?.ref ?? null),
          );
          break;
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    bumpChromeActivity,
    cancelPendingSeek,
    commitSeek,
    durationMs,
    flashAdjust,
    flashSeek,
    frameRate,
    seekBy,
    setChromeVisible,
    setSelectedSubtitle,
    subtitlesOptions,
    toggleFullscreen,
    togglePlay,
    video,
  ]);

  useEffect(() => {
    const onTouchStart = (event: TouchEvent) => {
      const touch = event.touches[0];
      if (!touch) return;
      if (!(event.target instanceof HTMLVideoElement)) return;
      const nearEdge =
        touch.clientX <= EDGE_GUARD_PX || touch.clientX >= window.innerWidth - EDGE_GUARD_PX;
      if (nearEdge) event.preventDefault();
    };
    document.addEventListener("touchstart", onTouchStart, { passive: false });
    return () => document.removeEventListener("touchstart", onTouchStart);
  }, []);

  useEffect(() => {
    if (!video) return;
    const original = video.volume;
    const probe = original > 0.5 ? original - 0.06 : original + 0.06;
    video.volume = probe;
    const timer = window.setTimeout(() => {
      volumeAdjustableRef.current = Math.abs(video.volume - probe) < 0.01;
      video.volume = original;
    }, 250);
    return () => {
      window.clearTimeout(timer);
      video.volume = original;
    };
  }, [video]);

  useEffect(() => {
    if (!video) return;
    const hold = holdRef.current;
    const setRate = (rate: number) => {
      video.preservesPitch = true;
      (video as HTMLVideoElement & { webkitPreservesPitch?: boolean }).webkitPreservesPitch = true;
      video.playbackRate = rate;
    };
    const dispatchHold = (event: HoldSpeedEvent) => {
      const next = holdSpeedReducer(hold.state, event);
      if (next === hold.state) return;
      if (hold.timer !== null) {
        window.clearTimeout(hold.timer);
        hold.timer = null;
      }
      if (next === "pending") {
        hold.timer = window.setTimeout(() => dispatchHold("elapsed"), HOLD_SPEED_DELAY_MS);
      } else if (next === "active") {
        hold.rate = video.playbackRate || 1;
        setRate(HOLD_SPEED_RATE);
        setHoldSpeed(true);
      } else if (hold.state === "active") {
        setRate(hold.rate);
        setHoldSpeed(false);
        if (event === "release") suppressClickRef.current = true;
        if (event === "starve") flashNotice("缓冲跟不上，已退出 2 倍速");
      }
      hold.state = next;
    };
    const onWaiting = () => dispatchHold("starve");
    video.addEventListener("waiting", onWaiting);

    const onTouchStart = (event: TouchEvent) => {
      suppressClickRef.current = false;
      if (lockedRef.current) {
        swipeGestureRef.current = null;
        return;
      }
      if (event.touches.length !== 1) {
        if (swipeGestureRef.current?.intent === "horizontal") hideSeekPreview();
        swipeGestureRef.current = null;
        dispatchHold("release");
        return;
      }
      const touch = event.touches[0];
      const kind = classifyTouchZone(touch.clientX, touch.clientY, {
        width: window.innerWidth,
        height: window.innerHeight,
        fakeLandscape: fakeLandscapeRef.current,
      });
      if (!kind) return;
      swipeGestureRef.current = {
        kind,
        startX: touch.clientX,
        startY: touch.clientY,
        startValue: kind === "brightness" ? brightnessRef.current : video.volume,
        startPositionMs: positionRef.current,
        intent: null,
        targetMs: positionRef.current,
      };
      if (canHoldSpeed({ paused: video.paused, locked: lockedRef.current, touchCount: 1 })) {
        dispatchHold("press");
      }
    };
    const onTouchMove = (event: TouchEvent) => {
      const gesture = swipeGestureRef.current;
      if (!gesture || event.touches.length !== 1) return;
      const touch = event.touches[0];
      const viewport = {
        width: window.innerWidth,
        height: window.innerHeight,
        fakeLandscape: fakeLandscapeRef.current,
      };
      const start = toLayoutPoint(gesture.startX, gesture.startY, viewport);
      const now = toLayoutPoint(touch.clientX, touch.clientY, viewport);
      const deltaX = now.x - start.x;
      const deltaY = now.y - start.y;
      if (!gesture.intent) {
        const intent = classifyIntent(deltaX, deltaY);
        if (!intent) return;
        dispatchHold("move");
        if (intent === "horizontal" && !durationMsRef.current) {
          swipeGestureRef.current = null;
          return;
        }
        gesture.intent = intent;
      }
      event.preventDefault();
      if (gesture.intent === "horizontal") {
        const target = clampSeekTarget(
          gesture.startPositionMs + seekDeltaMs(deltaX, now.width),
          durationMsRef.current,
        );
        gesture.targetMs = target;
        showSeekPreview(target, target - gesture.startPositionMs);
        return;
      }
      const value = applySwipe(gesture.kind, gesture.startValue, deltaY, now.height);
      if (gesture.kind === "brightness") {
        setBrightness(value);
        flashAdjust({ kind: "brightness", value });
        return;
      }
      video.volume = value;
      if (volumeAdjustableRef.current !== false) {
        if (value > gesture.startValue && video.muted) video.muted = false;
        flashAdjust({ kind: "volume", value, muted: video.muted });
      } else {
        flashAdjust({ kind: "volume", value: video.volume, unsupported: true });
      }
    };
    const finishGesture = (commit: boolean) => {
      dispatchHold("release");
      const gesture = swipeGestureRef.current;
      swipeGestureRef.current = null;
      if (gesture?.intent === "horizontal") {
        if (commit) commitSeek(gesture.targetMs);
        hideSeekPreview();
        return;
      }
      if (gesture?.intent !== "vertical" || gesture.kind !== "volume") return;
      if (volumeAdjustableRef.current === false) return;
      const expected = video.volume;
      window.setTimeout(() => {
        if (Math.abs(video.volume - expected) > 0.05) {
          volumeAdjustableRef.current = false;
        }
      }, 300);
    };
    const onTouchEnd = () => finishGesture(true);
    const onTouchCancel = () => finishGesture(false);
    video.addEventListener("touchstart", onTouchStart, { passive: true });
    video.addEventListener("touchmove", onTouchMove, { passive: false });
    video.addEventListener("touchend", onTouchEnd);
    video.addEventListener("touchcancel", onTouchCancel);
    return () => {
      video.removeEventListener("touchstart", onTouchStart);
      video.removeEventListener("touchmove", onTouchMove);
      video.removeEventListener("touchend", onTouchEnd);
      video.removeEventListener("touchcancel", onTouchCancel);
      video.removeEventListener("waiting", onWaiting);
      dispatchHold("release");
      if (swipeGestureRef.current?.intent === "horizontal") hideSeekPreview();
      swipeGestureRef.current = null;
    };
  }, [
    commitSeek,
    durationMsRef,
    flashAdjust,
    flashNotice,
    hideSeekPreview,
    positionRef,
    showSeekPreview,
    video,
  ]);

  return {
    locked,
    setLocked,
    lockHint,
    revealLock,
    brightness,
    holdSpeed,
    onSurfaceClick,
    onSurfaceDoubleClick,
    onContainerPointerMove,
    onContainerPointerDown,
    onContainerPointerLeave,
  };
}
