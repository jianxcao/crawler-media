"use client";

import { useEffect, type MutableRefObject } from "react";

import { useVideoContentBox } from "@/components/player/subtitle-layer";
import type { PlaybackUnit } from "@/lib/api/playback";
import type { PlayerPhase } from "@/lib/player/machine";
import { planSystemTrackModes, type PlaybackMode } from "@/lib/player/playback-mode";
import type { SubtitleOption, SubtitleTracks } from "@/lib/player/subtitles";

export function usePlayerSystemIntegration({
  video,
  mode,
  subtitles,
  activeSubtitle,
  pipSubtitleUrl,
  systemSubtitles,
  chromeVisible,
  positionMs,
  durationMs,
  paused,
  phase,
  title,
  episodeLabel,
  posterUrl,
  next,
  prev,
  onPlayNext,
  onPlayPrev,
  togglePlay,
  seekBy,
  commitSeekRef,
}: {
  video: HTMLVideoElement | null;
  mode: PlaybackMode | null;
  subtitles: SubtitleTracks;
  activeSubtitle: SubtitleOption | null;
  pipSubtitleUrl: string | null;
  systemSubtitles: boolean;
  chromeVisible: boolean;
  positionMs: number;
  durationMs: number | null;
  paused: boolean;
  phase: PlayerPhase;
  title: string | null;
  episodeLabel: string | null;
  posterUrl: string | null;
  next: { unit: PlaybackUnit; label: string } | null;
  prev: { unit: PlaybackUnit; label: string } | null;
  onPlayNext: () => void;
  onPlayPrev: () => void;
  togglePlay: () => void;
  seekBy: (seconds: number) => void;
  commitSeekRef: MutableRefObject<(fileMs: number) => void>;
}) {
  const contentBox = useVideoContentBox(systemSubtitles ? video : null);
  const cueFontPx = systemSubtitles ? Math.max(16, contentBox.height * 0.052) : 0;
  let cueLiftPx = 0;
  if (systemSubtitles && contentBox.height > 0) {
    const elementH = contentBox.height + contentBox.bottomInset * 2;
    const lineH = cueFontPx * 1.3;
    const currentTop = elementH * 0.84;
    const safeTop = elementH - contentBox.bottomInset - contentBox.height * 0.08 - lineH;
    const chromeTop = elementH - 172 - lineH;
    const targetTop = chromeVisible ? Math.min(safeTop, chromeTop) : safeTop;
    cueLiftPx = Math.max(0, currentTop - targetTop);
  }

  useEffect(() => {
    const mediaSession = navigator.mediaSession;
    if (!mediaSession) return;
    const shownTitle = title ?? "正在播放";
    mediaSession.metadata = new MediaMetadata({
      title: episodeLabel ? `${shownTitle} · ${episodeLabel}` : shownTitle,
      artist: episodeLabel ? shownTitle : undefined,
      artwork: posterUrl ? [{ src: posterUrl, sizes: "512x512" }] : undefined,
    });
    mediaSession.setActionHandler("play", () => togglePlay());
    mediaSession.setActionHandler("pause", () => togglePlay());
    mediaSession.setActionHandler("seekbackward", () => seekBy(-10));
    mediaSession.setActionHandler("seekforward", () => seekBy(10));
    mediaSession.setActionHandler("nexttrack", next ? () => onPlayNext() : null);
    mediaSession.setActionHandler("previoustrack", prev ? () => onPlayPrev() : null);
    try {
      mediaSession.setActionHandler("seekto", (details) => {
        if (typeof details.seekTime !== "number") return;
        commitSeekRef.current(details.seekTime * 1000);
      });
    } catch {
      // Unsupported by older browsers.
    }
    try {
      mediaSession.setActionHandler("enterpictureinpicture" as MediaSessionAction, () => {
        void video?.requestPictureInPicture?.().catch(() => undefined);
      });
    } catch {
      // Unsupported by browsers without auto-PiP action.
    }
    return () => {
      mediaSession.metadata = null;
      for (const action of [
        "play",
        "pause",
        "seekbackward",
        "seekforward",
        "nexttrack",
        "previoustrack",
      ] as const) {
        mediaSession.setActionHandler(action, null);
      }
      try {
        mediaSession.setActionHandler("seekto", null);
      } catch {
        // Not registered.
      }
      try {
        mediaSession.setActionHandler("enterpictureinpicture" as MediaSessionAction, null);
      } catch {
        // Not registered.
      }
    };
  }, [
    commitSeekRef,
    episodeLabel,
    next,
    onPlayNext,
    onPlayPrev,
    posterUrl,
    prev,
    seekBy,
    title,
    togglePlay,
    video,
  ]);

  useEffect(() => {
    if (!systemSubtitles || !video) return;
    const apply = () => {
      const domTracks = new Set(Array.from(video.querySelectorAll("track"), (el) => el.track));
      const list = Array.from(video.textTracks).filter(
        (t) => !domTracks.has(t) && (t.kind === "subtitles" || t.kind === "captions"),
      );
      const textOptions = subtitles.options.filter((o) => o.kind !== "pgs");
      const target = activeSubtitle
        ? textOptions.findIndex((o) => o.ref === activeSubtitle.ref)
        : -1;
      const modes = planSystemTrackModes(list.length, target);
      list.forEach((track, i) => {
        track.mode = modes[i];
      });
    };
    apply();
    video.textTracks.addEventListener("addtrack", apply);
    return () => video.textTracks.removeEventListener("addtrack", apply);
  }, [systemSubtitles, video, activeSubtitle, subtitles]);

  useEffect(() => {
    if (!video || !mode?.pipPatchTrack) return;
    const applyMode = () => {
      const webkitMode = (video as { webkitPresentationMode?: string }).webkitPresentationMode;
      const nativeSurface =
        document.pictureInPictureElement === video ||
        webkitMode === "picture-in-picture" ||
        webkitMode === "fullscreen" ||
        document.visibilityState === "hidden";
      for (const track of Array.from(video.textTracks)) {
        track.mode = nativeSurface ? "showing" : "hidden";
      }
    };
    applyMode();
    video.addEventListener("enterpictureinpicture", applyMode);
    video.addEventListener("leavepictureinpicture", applyMode);
    video.addEventListener("webkitpresentationmodechanged", applyMode);
    document.addEventListener("visibilitychange", applyMode);
    return () => {
      video.removeEventListener("enterpictureinpicture", applyMode);
      video.removeEventListener("leavepictureinpicture", applyMode);
      video.removeEventListener("webkitpresentationmodechanged", applyMode);
      document.removeEventListener("visibilitychange", applyMode);
    };
  }, [video, pipSubtitleUrl, mode?.pipPatchTrack]);

  useEffect(() => {
    const mediaSession = navigator.mediaSession;
    if (!mediaSession?.setPositionState || !durationMs || !video) return;
    try {
      mediaSession.setPositionState({
        duration: durationMs / 1000,
        position: Math.min(positionMs, durationMs) / 1000,
        playbackRate: video.playbackRate || 1,
      });
    } catch {
      // Transient session mismatch.
    }
  }, [positionMs, durationMs, video]);

  useEffect(() => {
    if (paused || phase !== "playing") return;
    let sentinel: WakeLockSentinel | null = null;
    let released = false;
    const acquire = () => {
      if (released || sentinel || document.visibilityState !== "visible") return;
      void navigator.wakeLock
        ?.request("screen")
        .then((lock) => {
          if (released) {
            void lock.release().catch(() => undefined);
            return;
          }
          sentinel = lock;
          lock.addEventListener?.("release", () => {
            if (sentinel === lock) sentinel = null;
          });
        })
        .catch(() => undefined);
    };
    acquire();
    document.addEventListener("visibilitychange", acquire);
    return () => {
      released = true;
      document.removeEventListener("visibilitychange", acquire);
      void sentinel?.release().catch(() => undefined);
    };
  }, [paused, phase]);

  return { cueFontPx, cueLiftPx };
}
