"use client";

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import type { PlaybackChapterMark } from "@/lib/api/playback";
import {
  acceptsNativeScrubValue,
  isScrubPointer,
  scrubCommitTarget,
} from "@/lib/player/scrub-follow";
import { livePositionMs, progressRatio, shownPositionMs } from "@/lib/player/timeline";
import { pointerOffsetX } from "@/lib/player/touch-adjust";
import { type TrickplayIndex, tileAt } from "@/lib/player/trickplay";

export function usePlayerScrub({
  positionMs,
  video,
  startMs,
  overrideMs,
  durationMs,
  bufferedEndMs,
  onSeek,
  onScrub,
  onScrubCancel,
  onScrubbingChange,
  trickplay,
  chapters,
  fakeLandscape,
}: {
  positionMs: number;
  video: HTMLVideoElement | null;
  startMs: number | null;
  overrideMs: number | null;
  durationMs: number | null;
  bufferedEndMs: number | null;
  onSeek: (fileMs: number) => void;
  onScrub: (fileMs: number) => void;
  onScrubCancel: () => void;
  onScrubbingChange: (active: boolean) => void;
  trickplay: TrickplayIndex | null;
  chapters: PlaybackChapterMark[];
  fakeLandscape: boolean;
}) {
  const [dragging, setDragging] = useState<number | null>(null);
  const [hover, setHover] = useState<{ ms: number; x: number } | null>(null);

  const pointerRef = useRef<{ x: number; length: number } | null>(null);
  const pointerFrameRef = useRef(0);
  const pointerDragRef = useRef(false);
  const activePointerIdRef = useRef<number | null>(null);
  const flushedPointerMsRef = useRef<number | null>(null);
  const latestRef = useRef({ durationMs, dragging, onScrub });
  latestRef.current = { durationMs, dragging, onScrub };

  useEffect(() => {
    if (!durationMs) {
      setDragging(null);
      onScrubCancel();
      onScrubbingChange(false);
      activePointerIdRef.current = null;
    }
  }, [durationMs, onScrubCancel, onScrubbingChange]);

  const pointerMs = useCallback(() => {
    const point = pointerRef.current;
    const total = latestRef.current.durationMs;
    if (!point || !total || point.length <= 0) return null;
    return Math.round(Math.min(1, Math.max(0, point.x / point.length)) * total);
  }, []);

  const cancelPointerFrame = useCallback(() => {
    if (pointerFrameRef.current) cancelAnimationFrame(pointerFrameRef.current);
    pointerFrameRef.current = 0;
  }, []);

  const flushPointer = useCallback(() => {
    pointerFrameRef.current = 0;
    const point = pointerRef.current;
    const next = pointerMs();
    if (point === null || next === null) return;
    setHover({ ms: next, x: point.x });
    if (latestRef.current.dragging === null) return;
    setDragging(next);
    flushedPointerMsRef.current = next;
    latestRef.current.onScrub(next);
  }, [pointerMs]);

  const trackPointer = useCallback(
    (event: React.PointerEvent<HTMLElement>) => {
      if (
        !isScrubPointer({
          activePointerId: activePointerIdRef.current,
          pointerId: event.pointerId,
          isPrimary: event.isPrimary,
        })
      ) {
        return;
      }
      const { offset, length } = pointerOffsetX(
        event,
        event.currentTarget.getBoundingClientRect(),
        fakeLandscape,
      );
      pointerRef.current = { x: Math.min(Math.max(offset, 0), length), length };
      if (!pointerFrameRef.current) {
        pointerFrameRef.current = requestAnimationFrame(flushPointer);
      }
    },
    [fakeLandscape, flushPointer],
  );

  useEffect(() => cancelPointerFrame, [cancelPointerFrame]);

  const shown = shownPositionMs({
    draggingMs: dragging,
    overrideMs,
    livePositionMs: null,
    positionMs,
  });

  const previewTile = useMemo(() => (hover ? tileAt(trickplay, hover.ms) : null), [hover, trickplay]);
  const chapterMarks = useMemo(
    () =>
      durationMs
        ? chapters
            .filter((mark) => mark.start_ms > 0 && mark.start_ms < durationMs)
            .map((mark) => ({
              start_ms: mark.start_ms,
              ratio: progressRatio(mark.start_ms, durationMs),
            }))
        : [],
    [chapters, durationMs],
  );
  const hoverChapter = useMemo(() => {
    if (!hover) return null;
    let title: string | null = null;
    for (const mark of chapters) {
      if (mark.start_ms > hover.ms) break;
      title = mark.title;
    }
    return title;
  }, [chapters, hover]);

  const bubbleRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const bubble = bubbleRef.current;
    if (!bubble || !hover) return;
    const trackWidth = bubble.parentElement?.clientWidth ?? 0;
    const half = bubble.offsetWidth / 2;
    const max = Math.max(half, trackWidth - half);
    bubble.style.left = `${Math.min(Math.max(hover.x, half), max)}px`;
  }, [hover, previewTile, hoverChapter]);

  const buffered =
    durationMs && bufferedEndMs ? Math.min(100, (bufferedEndMs / durationMs) * 100) : 0;

  const playedRef = useRef<HTMLDivElement>(null);
  const thumbRef = useRef<HTMLDivElement>(null);
  const paintInputRef = useRef({ video, startMs, durationMs, positionMs, dragging, overrideMs });
  paintInputRef.current = { video, startMs, durationMs, positionMs, dragging, overrideMs };

  const paint = useCallback(() => {
    const {
      video: el,
      startMs: origin,
      durationMs: total,
      positionMs: state,
      dragging: draggingMs,
      overrideMs: override,
    } = paintInputRef.current;
    if (!total) {
      if (playedRef.current) playedRef.current.style.width = "0%";
      if (thumbRef.current) thumbRef.current.style.left = "0%";
      return;
    }
    const live = el
      ? livePositionMs({
          originMs: origin,
          currentTimeSeconds: el.currentTime,
          paused: el.paused,
          seeking: el.seeking,
          readyState: el.readyState,
        })
      : null;
    const ratio = progressRatio(
      shownPositionMs({ draggingMs, overrideMs: override, livePositionMs: live, positionMs: state }),
      total,
    );
    const percent = `${ratio * 100}%`;
    if (playedRef.current) playedRef.current.style.width = percent;
    if (thumbRef.current) thumbRef.current.style.left = percent;
  }, []);

  useEffect(() => {
    if (!video) {
      paint();
      return;
    }
    let frame = 0;
    let loop = 0;
    const schedule = () => {
      if (loop) return;
      if (frame) cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        frame = 0;
        paint();
      });
    };
    const tick = () => {
      paint();
      loop = requestAnimationFrame(tick);
    };
    const start = () => {
      if (!loop) loop = requestAnimationFrame(tick);
    };
    const stop = () => {
      if (loop) cancelAnimationFrame(loop);
      loop = 0;
      schedule();
    };
    video.addEventListener("playing", start);
    video.addEventListener("play", start);
    video.addEventListener("pause", stop);
    video.addEventListener("ended", stop);
    video.addEventListener("seeked", schedule);
    video.addEventListener("timeupdate", schedule);
    if (!video.paused) start();
    else schedule();
    return () => {
      video.removeEventListener("playing", start);
      video.removeEventListener("play", start);
      video.removeEventListener("pause", stop);
      video.removeEventListener("ended", stop);
      video.removeEventListener("seeked", schedule);
      video.removeEventListener("timeupdate", schedule);
      if (loop) cancelAnimationFrame(loop);
      if (frame) cancelAnimationFrame(frame);
    };
  }, [video, paint]);

  useEffect(paint, [paint, shown, positionMs, durationMs, startMs]);

  const handleTrackPointerLeave = () => {
    if (latestRef.current.dragging === null) cancelPointerFrame();
    setHover(null);
  };

  const handleRangeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const next = Number(e.target.value);
    if (
      !acceptsNativeScrubValue({
        nativeValue: next,
        shownMs: shown,
        durationMs,
        pointerDragging: pointerDragRef.current,
      })
    ) {
      return;
    }
    setDragging(next);
    onScrubbingChange(true);
  };

  const handleRangePointerDown = (e: React.PointerEvent<HTMLInputElement>) => {
    if (!durationMs || e.button !== 0 || !e.isPrimary) return;
    pointerDragRef.current = true;
    activePointerIdRef.current = e.pointerId;
    flushedPointerMsRef.current = null;
    onScrubbingChange(true);
    e.currentTarget.setPointerCapture(e.pointerId);
    const { offset, length } = pointerOffsetX(
      e,
      e.currentTarget.getBoundingClientRect(),
      fakeLandscape,
    );
    pointerRef.current = { x: Math.min(Math.max(offset, 0), length), length };
    const ratio = Math.min(1, Math.max(0, offset / length));
    setDragging(Math.round(ratio * durationMs));
  };

  const handleRangePointerUp = (e: React.PointerEvent<HTMLInputElement>) => {
    if (activePointerIdRef.current === null || e.pointerId !== activePointerIdRef.current) return;
    activePointerIdRef.current = null;
    cancelPointerFrame();
    pointerDragRef.current = false;
    onScrubbingChange(false);
    const last = pointerMs();
    if (dragging !== null) {
      onSeek(
        scrubCommitTarget({
          pointerType: e.pointerType,
          lastPointerMs: last,
          flushedMs: flushedPointerMsRef.current,
          draggingMs: dragging,
        }),
      );
    }
    flushedPointerMsRef.current = null;
    pointerRef.current = null;
    setDragging(null);
  };

  const handleRangePointerCancel = (e: React.PointerEvent<HTMLInputElement>) => {
    if (activePointerIdRef.current === null || e.pointerId !== activePointerIdRef.current) return;
    activePointerIdRef.current = null;
    cancelPointerFrame();
    pointerDragRef.current = false;
    onScrubbingChange(false);
    flushedPointerMsRef.current = null;
    pointerRef.current = null;
    onScrubCancel();
    setDragging(null);
  };

  const handleRangeKeyUp = () => {
    if (pointerDragRef.current) return;
    if (dragging !== null) onSeek(dragging);
    setDragging(null);
    onScrubbingChange(false);
  };

  const handleRangeBlur = () => {
    if (pointerDragRef.current) return;
    setDragging(null);
    onScrubbingChange(false);
  };

  return {
    dragging,
    hover,
    shown,
    previewTile,
    chapterMarks,
    hoverChapter,
    bubbleRef,
    buffered,
    playedRef,
    thumbRef,
    trackPointer,
    handleTrackPointerLeave,
    handleRangeChange,
    handleRangePointerDown,
    handleRangePointerUp,
    handleRangePointerCancel,
    handleRangeKeyUp,
    handleRangeBlur,
  };
}
