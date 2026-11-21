"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import type { AdjustKind } from "@/lib/player/touch-adjust";

import type {
  AdjustFeedback,
  NoticeFeedback,
  SeekFlashFeedback,
  SeekPreviewFeedback,
} from "./player-types";

export function usePlayerFeedback() {
  const [adjust, setAdjust] = useState<AdjustFeedback | null>(null);
  const [seekFlash, setSeekFlash] = useState<SeekFlashFeedback | null>(null);
  const [seekPreview, setSeekPreview] = useState<SeekPreviewFeedback | null>(null);
  const [notice, setNotice] = useState<NoticeFeedback | null>(null);

  const adjustTimersRef = useRef<{ hide: number | null; gone: number | null }>({
    hide: null,
    gone: null,
  });
  const seekFlashTimersRef = useRef<{ hide: number | null; gone: number | null }>({
    hide: null,
    gone: null,
  });
  const noticeTimersRef = useRef<{ hide: number | null; gone: number | null }>({
    hide: null,
    gone: null,
  });
  const seekPreviewTimerRef = useRef<number | null>(null);

  const flashAdjust = useCallback(
    (next: { kind: AdjustKind; value: number; unsupported?: boolean; muted?: boolean }) => {
      const timers = adjustTimersRef.current;
      if (timers.hide !== null) window.clearTimeout(timers.hide);
      if (timers.gone !== null) window.clearTimeout(timers.gone);
      timers.gone = null;
      setAdjust({ ...next, leaving: false });
      timers.hide = window.setTimeout(() => {
        timers.hide = null;
        setAdjust((prev) => (prev ? { ...prev, leaving: true } : prev));
        timers.gone = window.setTimeout(() => {
          timers.gone = null;
          setAdjust(null);
        }, 180);
      }, 900);
    },
    [],
  );

  const flashSeek = useCallback((seconds: number) => {
    const timers = seekFlashTimersRef.current;
    if (timers.hide !== null) window.clearTimeout(timers.hide);
    if (timers.gone !== null) window.clearTimeout(timers.gone);
    timers.gone = null;
    setSeekFlash((prev) => ({
      seconds:
        prev && !prev.leaving && Math.sign(prev.seconds) === Math.sign(seconds)
          ? prev.seconds + seconds
          : seconds,
      leaving: false,
    }));
    timers.hide = window.setTimeout(() => {
      timers.hide = null;
      setSeekFlash((prev) => (prev ? { ...prev, leaving: true } : prev));
      timers.gone = window.setTimeout(() => {
        timers.gone = null;
        setSeekFlash(null);
      }, 180);
    }, 800);
  }, []);

  const showSeekPreview = useCallback((targetMs: number, deltaMs: number) => {
    if (seekPreviewTimerRef.current !== null) {
      window.clearTimeout(seekPreviewTimerRef.current);
      seekPreviewTimerRef.current = null;
    }
    setSeekPreview({ targetMs, deltaMs, leaving: false });
  }, []);

  const hideSeekPreview = useCallback(() => {
    setSeekPreview((prev) => (prev ? { ...prev, leaving: true } : prev));
    seekPreviewTimerRef.current = window.setTimeout(() => {
      seekPreviewTimerRef.current = null;
      setSeekPreview(null);
    }, 180);
  }, []);

  const flashNotice = useCallback((text: string) => {
    const timers = noticeTimersRef.current;
    if (timers.hide !== null) window.clearTimeout(timers.hide);
    if (timers.gone !== null) window.clearTimeout(timers.gone);
    timers.gone = null;
    setNotice({ text, leaving: false });
    timers.hide = window.setTimeout(() => {
      timers.hide = null;
      setNotice((prev) => (prev ? { ...prev, leaving: true } : prev));
      timers.gone = window.setTimeout(() => {
        timers.gone = null;
        setNotice(null);
      }, 180);
    }, 2600);
  }, []);

  useEffect(
    () => () => {
      for (const timers of [
        adjustTimersRef.current,
        seekFlashTimersRef.current,
        noticeTimersRef.current,
      ]) {
        if (timers.hide !== null) window.clearTimeout(timers.hide);
        if (timers.gone !== null) window.clearTimeout(timers.gone);
      }
      if (seekPreviewTimerRef.current !== null) {
        window.clearTimeout(seekPreviewTimerRef.current);
      }
    },
    [],
  );

  return {
    adjust,
    flashAdjust,
    seekFlash,
    flashSeek,
    seekPreview,
    showSeekPreview,
    hideSeekPreview,
    notice,
    flashNotice,
  };
}
