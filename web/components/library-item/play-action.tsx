"use client";

import type { ReactNode } from "react";

import { CheckIcon, HeartIcon, PlayIcon } from "@/components/icons";
import { Tooltip } from "@/components/tooltip";
import type { PlaybackWatchState } from "@/lib/api/playback";
import { formatRuntimeMinutes } from "@/lib/format";
import { formatClock } from "@/lib/player/timeline";

const MARK_BUTTON_CLASS =
  "inline-flex h-12 shrink-0 items-center justify-center gap-2 rounded-full border border-white/[0.12] bg-white/[0.08] text-white/85 backdrop-blur-md transition duration-200 hover:bg-white/[0.14] hover:text-white active:scale-[0.96] disabled:pointer-events-none disabled:opacity-60 md:size-12 max-md:h-11 max-md:px-4";

/**
 * 播放入口（主行动按钮 + 续播进度 + 收藏/已看标记）。
 */
export function PlayAction({
  watched,
  onPlay,
  favorite = null,
  favoriteLabel = "",
  marking = false,
  onToggleFavorite,
  onTogglePlayed,
  extraAction = null,
}: {
  watched: PlaybackWatchState | null;
  onPlay: () => void;
  favorite?: boolean | null;
  favoriteLabel?: string;
  marking?: boolean;
  onToggleFavorite?: () => void;
  onTogglePlayed?: () => void;
  extraAction?: ReactNode | null;
}) {
  const positionMs = watched?.start_ms ?? watched?.position_ms ?? 0;
  const durationMs = watched?.duration_ms ?? null;
  const finished = watched?.played ?? false;
  const resumable = !finished && positionMs > 0;
  const percent =
    resumable && durationMs && durationMs > 0
      ? Math.min(100, Math.max(2, Math.round((positionMs / durationMs) * 100)))
      : null;
  const remainingMinutes =
    resumable && durationMs && durationMs > positionMs
      ? Math.round((durationMs - positionMs) / 60000)
      : null;
  const label = finished ? "重新播放" : resumable ? "继续观看" : "播放";
  const progressText = resumable
    ? [
        `看到 ${formatClock(positionMs)}`,
        remainingMinutes && remainingMinutes >= 1
          ? `剩余 ${formatRuntimeMinutes(remainingMinutes)}`
          : durationMs
            ? "即将看完"
            : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : null;

  return (
    <div className="mt-5 flex max-w-4xl flex-wrap items-center gap-x-5 gap-y-3 max-md:mt-4">
      <button
        type="button"
        onClick={onPlay}
        aria-label={progressText ? `${label}，${progressText}` : label}
        className="inline-flex h-12 shrink-0 items-center justify-center gap-2.5 rounded-full bg-white px-8 text-body font-semibold text-black shadow-[0_10px_30px_rgba(0,0,0,0.35)] transition duration-200 hover:bg-white/90 active:scale-[0.985] max-md:h-11 max-md:w-full max-md:px-6"
      >
        <PlayIcon className="size-5" />
        {label}
      </button>

      {(onToggleFavorite || onTogglePlayed) && (
        <div className="flex items-center gap-2.5">
          {onToggleFavorite && (
            <Tooltip
              content={favorite ? `取消收藏${favoriteLabel}` : `收藏${favoriteLabel}`}
              dismissOnReferencePress
            >
              <button
                type="button"
                onClick={onToggleFavorite}
                disabled={marking}
                aria-pressed={Boolean(favorite)}
                aria-label={favorite ? "取消收藏" : "收藏"}
                className={MARK_BUTTON_CLASS}
              >
                <HeartIcon
                  className={favorite ? "size-5 text-[var(--danger)]" : "size-5"}
                  fill={favorite ? "currentColor" : "none"}
                />
                <span className="text-ui font-medium md:hidden">
                  {favorite ? "已收藏" : "收藏"}
                </span>
              </button>
            </Tooltip>
          )}
          {onTogglePlayed && (
            <Tooltip content={finished ? "标记为未看" : "标记为已看"} dismissOnReferencePress>
              <button
                type="button"
                onClick={onTogglePlayed}
                disabled={marking}
                aria-pressed={finished}
                aria-label={finished ? "标记为未看" : "标记为已看"}
                className={MARK_BUTTON_CLASS}
              >
                <CheckIcon
                  className={
                    finished ? "size-5 stroke-[2.4] text-[var(--ok)]" : "size-5 stroke-[2.4]"
                  }
                />
                <span className="text-ui font-medium md:hidden">
                  {finished ? "已看完" : "标为已看"}
                </span>
              </button>
            </Tooltip>
          )}
          {extraAction}
        </div>
      )}

      {progressText && (
        <div className="min-w-0 max-md:w-full">
          <div
            className="h-1 w-44 overflow-hidden rounded-full bg-white/[0.18] max-md:w-full"
            role="progressbar"
            aria-valuenow={percent ?? 0}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label="观看进度"
          >
            <div className="h-full rounded-full bg-white/85" style={{ width: `${percent ?? 0}%` }} />
          </div>
          <p className="tnum mt-1.5 text-caption text-white/60">{progressText}</p>
        </div>
      )}

      {finished && watched && watched.play_count > 1 && (
        <p className="tnum text-caption text-white/55">看过 {watched.play_count} 次</p>
      )}
    </div>
  );
}
