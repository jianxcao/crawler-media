"use client";

import type { PlaybackChapterMark } from "@/lib/api/playback";
import { SCRUB_INPUT_STEP_MS, scrubInputValue } from "@/lib/player/scrub-follow";
import { formatClock } from "@/lib/player/timeline";
import type { TrickplayIndex } from "@/lib/player/trickplay";

import { usePlayerScrub } from "./use-player-scrub";

export function PlayerScrubBar({
  positionMs,
  video,
  startMs,
  overrideMs,
  durationMs,
  bufferedEndMs,
  chromeVisible,
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
  chromeVisible: boolean;
  onSeek: (fileMs: number) => void;
  onScrub: (fileMs: number) => void;
  onScrubCancel: () => void;
  onScrubbingChange: (active: boolean) => void;
  trickplay: TrickplayIndex | null;
  chapters: PlaybackChapterMark[];
  fakeLandscape: boolean;
}) {
  const s = usePlayerScrub({
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
  });

  return (
    <>
      <div
        className={`player-inset-x pointer-events-none relative flex items-center pt-24 pb-2 transition-opacity duration-300 max-md:pt-16 ${
          chromeVisible ? "opacity-100" : "opacity-0"
        }`}
      >
        <span className="player-glass inline-flex h-7 items-center gap-1 rounded-full px-2.5 text-[12px] font-medium tabular-nums text-white">
          <span>{formatClock(s.shown)}</span>
          <span className="font-normal text-white/40">
            / {durationMs ? formatClock(durationMs) : "--:--"}
          </span>
        </span>
      </div>

      <div
        className={`player-scrub-row player-inset-x relative transition-opacity duration-300 ${
          chromeVisible ? "pointer-events-auto opacity-100" : "pointer-events-none opacity-0"
        }`}
      >
        <div
          className="player-scrub-shade relative h-5"
          onPointerMove={s.trackPointer}
          onPointerLeave={s.handleTrackPointerLeave}
        >
          {s.hover ? (
            <div
              ref={s.bubbleRef}
              className="pointer-events-none absolute bottom-8 -translate-x-1/2 pointer-coarse:bottom-16"
              style={{ left: s.hover.x }}
            >
              {s.previewTile ? (
                <div
                  style={{
                    width: s.previewTile.width,
                    height: s.previewTile.height,
                    backgroundImage: `url(${s.previewTile.url})`,
                    backgroundPosition: `${s.previewTile.offsetX}px ${s.previewTile.offsetY}px`,
                  }}
                  className="rounded-[10px] shadow-[0_10px_28px_rgba(0,0,0,0.55)] ring-1 ring-white/30"
                />
              ) : null}
              {s.hoverChapter ? (
                <p className="mt-1.5 max-w-[220px] truncate text-center text-[12px] text-white/75 drop-shadow">
                  {s.hoverChapter}
                </p>
              ) : null}
              <p className="mt-0.5 text-center text-[13px] font-medium tabular-nums text-white drop-shadow">
                {formatClock(s.hover.ms)}
              </p>
            </div>
          ) : null}

          <div className="player-scrub-track pointer-events-none absolute inset-x-0 top-1/2 h-[3px] -translate-y-1/2 overflow-hidden rounded-full bg-[var(--player-track)] transition-[height] duration-150 [.player-scrub-row:hover_&]:h-[5px]">
            <div className="h-full bg-[var(--player-buffered)]" style={{ width: `${s.buffered}%` }} />
            <div
              ref={s.playedRef}
              data-player-played=""
              className="absolute inset-y-0 left-0 w-0 bg-[var(--player-accent)]"
            />
            {s.chapterMarks.map((mark) => (
              <span
                key={mark.start_ms}
                className="absolute inset-y-0 w-[2px] -translate-x-1/2 bg-black/55"
                style={{ left: `${mark.ratio * 100}%` }}
              />
            ))}
          </div>
          <input
            type="range"
            min={0}
            max={durationMs ?? 0}
            step={SCRUB_INPUT_STEP_MS}
            value={scrubInputValue(s.shown, durationMs)}
            disabled={!durationMs}
            aria-label="播放进度"
            onChange={s.handleRangeChange}
            onPointerDown={s.handleRangePointerDown}
            onPointerUp={s.handleRangePointerUp}
            onPointerCancel={s.handleRangePointerCancel}
            onKeyUp={s.handleRangeKeyUp}
            onBlur={s.handleRangeBlur}
            className="player-scrub absolute inset-x-0 top-1/2 h-5 w-full -translate-y-1/2 cursor-pointer touch-none appearance-none bg-transparent disabled:cursor-default pointer-coarse:h-11"
          />

          <div
            ref={s.thumbRef}
            data-player-thumb=""
            className={`pointer-events-none absolute left-0 top-1/2 size-[14px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-[var(--player-thumb)] shadow-[0_0_0_4px_var(--accent-soft)] transition-transform duration-150 pointer-coarse:size-[18px] ${
              durationMs
                ? s.dragging !== null
                  ? "scale-100 pointer-coarse:scale-110"
                  : "scale-0 [.player-scrub-row:hover_&]:scale-110 pointer-coarse:scale-100"
                : "scale-0"
            }`}
          />
        </div>
      </div>
    </>
  );
}
