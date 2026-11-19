"use client";

import type { ReactNode } from "react";

/** 图标尺寸与全站顶栏控件一致，按触控精度（pointer-coarse）分档。 */
export const PLAYER_ICON_CLASS = "size-[18px] pointer-coarse:size-[22px]";

export function StrokeIcon({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className ?? PLAYER_ICON_CLASS}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

/** 播放 / 暂停（仅用于中央播放簇）。 */
export function PlayGlyph({ paused }: { paused: boolean }) {
  const cls = "size-[52px] fill-current pointer-coarse:size-11";
  return paused ? (
    <svg viewBox="0 0 24 24" className={cls} aria-hidden>
      <path d="M6 4.3v15.4a.7.7 0 0 0 1.07.6l12.3-7.7a.7.7 0 0 0 0-1.2L7.07 3.7A.7.7 0 0 0 6 4.3Z" />
    </svg>
  ) : (
    <svg viewBox="0 0 24 24" className={cls} aria-hidden>
      <path d="M6.5 4h3.6v16H6.5zM13.9 4h3.6v16h-3.6z" />
    </svg>
  );
}

/** 退/进十秒。 */
export function SkipGlyph({ forward }: { forward: boolean }) {
  return (
    <svg viewBox="0 0 24 24" className="size-9 fill-current pointer-coarse:size-8" aria-hidden>
      <path
        d={forward ? "M12 4.5A8 8 0 1 0 18.93 8.5" : "M12 4.5A8 8 0 1 1 5.07 8.5"}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
      />
      {forward ? (
        <path d="M11.7 1.9v5.2l4.6-2.6Z" />
      ) : (
        <path d="M12.3 1.9v5.2L7.7 4.5Z" />
      )}
      <text
        x="12"
        y="15"
        textAnchor="middle"
        fontSize="7"
        fontWeight="600"
        fill="currentColor"
        stroke="none"
      >
        10
      </text>
    </svg>
  );
}

export function AudioGlyph() {
  return (
    <StrokeIcon>
      <path d="M9 18V5.4l10-2v12.2" />
      <circle cx="6.4" cy="18" r="2.6" />
      <circle cx="16.4" cy="15.6" r="2.6" />
    </StrokeIcon>
  );
}

export function SubtitleGlyph() {
  return (
    <StrokeIcon>
      <rect x="3" y="5" width="18" height="14" rx="2.5" />
      <path d="M7 10.6h4M13.5 10.6h3.5M7 14.4h6.5M16 14.4h1" />
    </StrokeIcon>
  );
}

export function RotateGlyph({ active }: { active: boolean }) {
  return (
    <StrokeIcon>
      {active ? (
        <>
          <path d="M20.5 11.5A8 8 0 0 0 12.5 3.5" />
          <path d="m15.2 1.9-2.7 1.6 1.6 2.7" />
          <rect x="3" y="8.5" width="9.5" height="12.5" rx="2" />
        </>
      ) : (
        <>
          <path d="M3.5 11.5A8 8 0 0 1 11.5 3.5" />
          <path d="m8.8 1.9 2.7 1.6-1.6 2.7" />
          <rect x="8.5" y="11" width="12.5" height="9.5" rx="2" />
        </>
      )}
    </StrokeIcon>
  );
}

export function PipGlyph({ exit }: { exit: boolean }) {
  return (
    <svg viewBox="0 0 24 24" className={PLAYER_ICON_CLASS} fill="currentColor" aria-hidden>
      <path d="M3 5.5A1.5 1.5 0 0 1 4.5 4h15A1.5 1.5 0 0 1 21 5.5v13a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5v-13Zm2 .5v12h14V6H5Z" />
      <path d={exit ? "M7 8h7v5H7V8Z" : "M12 12h6v5h-6v-5Z"} />
    </svg>
  );
}

export function BrightnessGlyph() {
  return (
    <svg
      viewBox="0 0 24 24"
      className="size-4 shrink-0"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      aria-hidden
    >
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2.5M12 19.5V22M2 12h2.5M19.5 12H22M4.9 4.9l1.8 1.8M17.3 17.3l1.8 1.8M19.1 4.9l-1.8 1.8M6.7 17.3l-1.8 1.8" />
    </svg>
  );
}

export function VolumeGlyph({ muted }: { muted: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className="size-4 shrink-0"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M11 5 6.5 9H3v6h3.5L11 19V5Z" />
      {muted ? (
        <path d="M15 9l6 6M21 9l-6 6" />
      ) : (
        <path d="M15.5 8.5a5 5 0 0 1 0 7M18.4 6a9 9 0 0 1 0 12" />
      )}
    </svg>
  );
}

export function SeekChevrons({ back }: { back: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={`size-4 shrink-0 ${back ? "-scale-x-100" : ""}`}
      fill="none"
      stroke="currentColor"
      strokeWidth={2.2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="m6 6 6 6-6 6M13 6l6 6-6 6" />
    </svg>
  );
}
