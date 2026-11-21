"use client";

import type { ReactNode } from "react";

import { PlayGlyph, SkipGlyph } from "./player-icons";

/** 中央播放簇：退十秒 / 播放暂停 / 进十秒。 */
export function PlayerCenterControls({
  paused,
  visible,
  onTogglePlay,
  onSeekBy,
}: {
  paused: boolean;
  visible: boolean;
  onTogglePlay: () => void;
  onSeekBy: (seconds: number) => void;
}) {
  return (
    <div
      className={`pointer-events-none absolute inset-0 z-20 flex items-center justify-center gap-14 transition-opacity duration-300 max-md:gap-10 ${
        visible ? "opacity-100" : "opacity-0"
      }`}
    >
      <CenterButton label="后退 10 秒" visible={visible} onClick={() => onSeekBy(-10)}>
        <SkipGlyph forward={false} />
      </CenterButton>
      <CenterButton label={paused ? "播放" : "暂停"} visible={visible} onClick={onTogglePlay} primary>
        <PlayGlyph paused={paused} />
      </CenterButton>
      <CenterButton label="前进 10 秒" visible={visible} onClick={() => onSeekBy(10)}>
        <SkipGlyph forward />
      </CenterButton>
    </div>
  );
}

function CenterButton({
  label,
  visible,
  primary,
  onClick,
  children,
}: {
  label: string;
  visible: boolean;
  primary?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      className={`player-btn drop-shadow-[0_2px_8px_rgba(0,0,0,0.65)] ${
        primary ? "size-[68px] pointer-coarse:size-14" : "size-12 pointer-coarse:size-11"
      } ${visible ? "pointer-events-auto" : "pointer-events-none"}`}
    >
      {children}
    </button>
  );
}
