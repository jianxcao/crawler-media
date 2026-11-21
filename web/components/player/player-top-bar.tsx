"use client";

import { ChevronLeftIcon, LockIcon } from "@/components/icons";

import { PipGlyph } from "./player-icons";

export function PlayerTopBar({
  chromeVisible,
  landscape,
  fakeLandscape,
  deviceLandscape,
  canRotate,
  canPip,
  pipActive,
  title,
  episodeLabel,
  speedLabel,
  onBack,
  onLock,
  onTogglePip,
}: {
  chromeVisible: boolean;
  landscape: boolean;
  fakeLandscape: boolean;
  deviceLandscape: boolean;
  canRotate: boolean;
  canPip: boolean;
  pipActive: boolean;
  title: string | null;
  episodeLabel: string | null;
  speedLabel: string | null;
  onBack: () => void;
  onLock: () => void;
  onTogglePip: () => void;
}) {
  return (
    <div
      className={`player-inset-x pointer-events-none absolute inset-x-0 top-0 z-20 flex items-center gap-4 bg-gradient-to-b from-black/80 via-black/30 to-transparent pb-16 pt-[calc(1.25rem_+_var(--safe-top))] transition-opacity duration-300 max-md:pt-[calc(0.75rem_+_var(--safe-top))] ${
        chromeVisible ? "opacity-100" : "opacity-0"
      }`}
    >
      <button
        type="button"
        onClick={onBack}
        className={`grid size-9 shrink-0 place-items-center rounded-full border border-white/[0.09] bg-black/30 text-white/85 backdrop-blur-md transition hover:bg-black/50 hover:text-white active:scale-[0.94] pointer-coarse:size-11 ${
          chromeVisible ? "pointer-events-auto" : "pointer-events-none"
        }`}
        aria-label={landscape ? "退出横屏" : "退出播放"}
      >
        <ChevronLeftIcon className="size-[18px] pointer-coarse:size-[22px]" />
      </button>
      <div className="min-w-0">
        <h1 className="truncate text-[17px] font-semibold text-white drop-shadow">{title}</h1>
        {episodeLabel ? (
          <p className="truncate text-[13px] text-white/65">{episodeLabel}</p>
        ) : null}
      </div>

      <div className="ml-auto flex shrink-0 items-center gap-4">
        {speedLabel ? (
          <span className="tnum text-[12px] text-white/45">↓ {speedLabel}</span>
        ) : null}

        {canRotate && (landscape || fakeLandscape || deviceLandscape) ? (
          <button
            type="button"
            onClick={onLock}
            className={`grid size-9 shrink-0 place-items-center rounded-full border border-white/[0.09] bg-black/30 text-white/85 backdrop-blur-md transition hover:bg-black/50 hover:text-white active:scale-[0.94] pointer-coarse:size-11 ${
              chromeVisible ? "pointer-events-auto" : "pointer-events-none"
            }`}
            aria-label="锁屏"
            title="锁屏（防误触）"
          >
            <LockIcon className="size-[18px] pointer-coarse:size-[22px]" />
          </button>
        ) : null}

        {canPip ? (
          <button
            type="button"
            onClick={onTogglePip}
            className={`grid size-9 shrink-0 place-items-center rounded-full border border-white/[0.09] bg-black/30 text-white/85 backdrop-blur-md transition hover:bg-black/50 hover:text-white active:scale-[0.94] pointer-coarse:size-11 ${
              chromeVisible ? "pointer-events-auto" : "pointer-events-none"
            }`}
            aria-label={pipActive ? "退出画中画" : "画中画"}
            title={pipActive ? "退出画中画" : "画中画"}
          >
            <PipGlyph exit={pipActive} />
          </button>
        ) : null}
      </div>
    </div>
  );
}
