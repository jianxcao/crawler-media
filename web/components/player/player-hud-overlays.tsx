"use client";

import { LockIcon } from "@/components/icons";
import { HOLD_SPEED_RATE } from "@/lib/player/hold-speed";
import type { PlayerPhase } from "@/lib/player/machine";
import { busyLabel } from "@/lib/player/player-ui";
import { formatClock } from "@/lib/player/timeline";

import { BrightnessGlyph, SeekChevrons, VolumeGlyph } from "./player-icons";
import type {
  AdjustFeedback,
  NoticeFeedback,
  SeekFlashFeedback,
  SeekPreviewFeedback,
} from "./player-types";

export function PlayerHudOverlays({
  locked,
  lockHint,
  onUnlock,
  holdSpeed,
  adjust,
  seekPreview,
  durationMs,
  notice,
  seekFlash,
  showPauseOverlay,
  busy,
  phase,
  speedLabel,
  error,
  onRetry,
  onExit,
  showNextCard,
  nextLabel,
  onDismissNext,
  onPlayNext,
}: {
  locked: boolean;
  lockHint: boolean;
  onUnlock: () => void;
  holdSpeed: boolean;
  adjust: AdjustFeedback | null;
  seekPreview: SeekPreviewFeedback | null;
  durationMs: number | null;
  notice: NoticeFeedback | null;
  seekFlash: SeekFlashFeedback | null;
  showPauseOverlay: boolean;
  busy: boolean;
  phase: PlayerPhase;
  speedLabel: string | null;
  error: { message: string; suggestion: string | null } | null;
  onRetry: () => void;
  onExit: () => void;
  showNextCard: boolean;
  nextLabel: string | null;
  onDismissNext: () => void;
  onPlayNext: () => void;
}) {
  return (
    <>
      {locked ? (
        <div
          {...{ noautohide: "" }}
          className={`absolute left-[6%] top-1/2 z-30 -translate-y-1/2 transition-opacity duration-300 ${
            lockHint ? "pointer-events-auto opacity-100" : "pointer-events-none opacity-0"
          }`}
        >
          <button
            type="button"
            onClick={onUnlock}
            className="grid size-11 place-items-center rounded-full border border-white/[0.09] bg-black/45 text-white backdrop-blur-md transition active:scale-[0.94]"
            aria-label="解锁"
            title="解锁"
          >
            <LockIcon className="size-[22px]" />
          </button>
        </div>
      ) : null}

      {holdSpeed ? (
        <div
          {...{ noautohide: "" }}
          className="pointer-events-none absolute left-1/2 top-[calc(1rem_+_var(--safe-top))] z-30 -translate-x-1/2"
        >
          <div className="player-flash-in flex items-center gap-2 rounded-full bg-black/70 px-4 py-2 text-[13px] font-medium text-white shadow-[0_10px_28px_rgba(0,0,0,0.45)]">
            <span className="tnum">{HOLD_SPEED_RATE}× 快进中</span>
            <SeekChevrons back={false} />
          </div>
        </div>
      ) : null}

      {adjust ? (
        <div
          {...{ noautohide: "" }}
          className="pointer-events-none absolute left-1/2 top-[calc(1rem_+_var(--safe-top))] z-30 -translate-x-1/2"
        >
          <div
            className={`flex items-center gap-2.5 rounded-full bg-black/70 px-4 py-2 text-[13px] font-medium text-white shadow-[0_10px_28px_rgba(0,0,0,0.45)] ${
              adjust.leaving ? "player-flash-out" : "player-flash-in"
            }`}
          >
            {adjust.kind === "brightness" ? (
              <BrightnessGlyph />
            ) : (
              <VolumeGlyph muted={Boolean(adjust.muted) || adjust.value <= 0.001} />
            )}
            {adjust.unsupported ? (
              <span>音量由系统侧键控制</span>
            ) : adjust.muted ? (
              <span>静音</span>
            ) : (
              <>
                <div className="h-1 w-24 overflow-hidden rounded-full bg-white/25">
                  <div
                    className="h-full rounded-full bg-white"
                    style={{ width: `${Math.round(adjust.value * 100)}%` }}
                  />
                </div>
                <span className="tnum w-9 text-right">{Math.round(adjust.value * 100)}%</span>
              </>
            )}
          </div>
        </div>
      ) : null}

      {seekPreview ? (
        <div
          {...{ noautohide: "" }}
          className="pointer-events-none absolute left-1/2 top-1/2 z-30 -translate-x-1/2 -translate-y-1/2"
        >
          <div
            className={`flex flex-col items-center gap-1.5 rounded-2xl bg-black/70 px-6 py-3.5 text-white shadow-[0_10px_28px_rgba(0,0,0,0.45)] ${
              seekPreview.leaving ? "player-flash-out" : "player-flash-in"
            }`}
          >
            <p className="tnum text-[24px] font-semibold leading-none max-md:text-[22px]">
              {formatClock(seekPreview.targetMs)}
              <span className="text-[15px] font-normal text-white/45">
                {" / "}
                {durationMs ? formatClock(durationMs) : "--:--"}
              </span>
            </p>
            <p className="tnum text-[13px] leading-none text-white/70">
              {seekPreview.deltaMs < 0 ? "−" : "+"}
              {formatClock(Math.abs(seekPreview.deltaMs))}
            </p>
          </div>
        </div>
      ) : null}

      {notice ? (
        <div
          {...{ noautohide: "" }}
          className="pointer-events-none absolute left-1/2 top-[calc(3.75rem_+_var(--safe-top))] z-30 -translate-x-1/2"
        >
          <div
            className={`max-w-[85vw] rounded-full bg-black/70 px-4 py-2 text-center text-[13px] font-medium text-white shadow-[0_10px_28px_rgba(0,0,0,0.45)] ${
              notice.leaving ? "player-flash-out" : "player-flash-in"
            }`}
          >
            {notice.text}
          </div>
        </div>
      ) : null}

      {seekFlash ? (
        <div
          {...{ noautohide: "" }}
          className={`pointer-events-none absolute top-1/2 z-10 -translate-y-1/2 ${
            seekFlash.seconds < 0 ? "left-[10%]" : "right-[10%]"
          }`}
        >
          <div
            className={`flex items-center gap-1.5 rounded-full bg-black/65 px-3.5 py-2 text-[13px] font-medium text-white shadow-[0_10px_28px_rgba(0,0,0,0.45)] ${
              seekFlash.leaving ? "player-flash-out" : "player-flash-in"
            }`}
          >
            {seekFlash.seconds < 0 ? <SeekChevrons back /> : null}
            <span className="tnum">{Math.abs(seekFlash.seconds)} 秒</span>
            {seekFlash.seconds > 0 ? <SeekChevrons back={false} /> : null}
          </div>
        </div>
      ) : null}

      <div
        className={`pointer-events-none absolute inset-0 z-10 bg-black/45 transition-opacity ${
          showPauseOverlay ? "opacity-100 duration-300 delay-200" : "opacity-0 duration-200 delay-0"
        }`}
      />

      {busy ? (
        <div
          {...{ noautohide: "" }}
          className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center"
        >
          <div className="player-busy-appear flex flex-col items-center gap-4">
            <span className="size-12 animate-spin rounded-full border-[3px] border-white/15 border-t-[var(--player-accent)]" />
            <div className="flex flex-col items-center gap-1.5">
              <span className="text-[14px] text-white/75">{busyLabel(phase)}</span>
              {speedLabel ? (
                <span className="tnum text-[12px] text-white/45">↓ {speedLabel}</span>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}

      {phase === "error" && error ? (
        <div
          {...{ noautohide: "" }}
          className="absolute inset-0 z-30 flex items-center justify-center bg-black/85 px-6"
        >
          <div className="max-w-[480px] text-center">
            <p className="text-[17px] font-semibold text-white">{error.message}</p>
            {error.suggestion ? (
              <p className="mt-3 text-[14px] leading-relaxed text-white/60">{error.suggestion}</p>
            ) : null}
            <div className="mt-6 flex justify-center gap-3">
              <button
                type="button"
                onClick={onRetry}
                className="rounded-full bg-[var(--player-accent)] px-6 py-2.5 text-[14px] font-semibold text-black transition-colors hover:bg-[var(--player-accent-hover)]"
              >
                重试
              </button>
              <button
                type="button"
                onClick={onExit}
                className="rounded-full bg-white/15 px-6 py-2.5 text-[14px] font-medium text-white transition-colors hover:bg-white/25"
              >
                返回
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {showNextCard && nextLabel ? (
        <div
          style={{ position: "absolute" }}
          className="menu-surface bottom-32 right-6 z-30 w-[300px] p-4 max-md:bottom-28 max-md:right-3 max-md:w-[240px]"
        >
          <p className="text-[12px] uppercase tracking-wide text-white/50">即将播放</p>
          <p className="mt-1.5 truncate text-[15px] font-semibold text-white">{nextLabel}</p>
          <div className="mt-4 flex gap-2">
            <button
              type="button"
              onClick={onDismissNext}
              className="rounded-full bg-white/15 px-4 py-2 text-[13px] text-white/85 transition-colors hover:bg-white/25"
            >
              关闭
            </button>
            <button
              type="button"
              onClick={onPlayNext}
              className="flex-1 rounded-full bg-white px-4 py-2 text-[13px] font-semibold text-black transition-colors hover:bg-white/85"
            >
              立即播放
            </button>
          </div>
        </div>
      ) : null}
    </>
  );
}
