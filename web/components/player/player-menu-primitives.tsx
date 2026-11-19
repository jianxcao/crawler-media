"use client";

import { useEffect, useRef, type ReactNode } from "react";

import { CheckIcon } from "@/components/icons";

export function IconButton({
  tip,
  active,
  open,
  onClick,
  children,
}: {
  tip: string;
  active?: boolean;
  open?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={tip}
      aria-expanded={open === undefined ? undefined : open}
      data-tip={tip}
      data-active={active ? "true" : undefined}
      data-open={open ? "true" : undefined}
      className="player-btn player-tip size-9 shrink-0 pointer-coarse:size-11"
    >
      {children}
    </button>
  );
}

export function MenuPanel({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDocPointerDown = (event: PointerEvent) => {
      if (!box.current?.contains(event.target as Node)) onClose();
    };
    document.addEventListener("pointerdown", onDocPointerDown, true);
    return () => document.removeEventListener("pointerdown", onDocPointerDown, true);
  }, [onClose]);

  return (
    <div
      ref={box}
      className="player-flash-in absolute bottom-full left-0 mb-8 w-[300px] origin-bottom-left overflow-hidden rounded-[14px] bg-black/85 py-2 text-[13px] shadow-[0_18px_44px_rgba(0,0,0,0.5)] max-md:left-[-2rem] max-md:w-[240px]"
    >
      <p className="px-4 pb-1.5 pt-0.5 text-[12px] font-semibold text-white/55">{title}</p>
      {children}
    </div>
  );
}

export function MenuItem({
  active,
  icon,
  badge,
  onClick,
  children,
}: {
  active: boolean;
  icon?: ReactNode;
  badge?: ReactNode;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex w-full cursor-pointer items-center gap-2.5 px-4 py-2 text-left font-medium transition-colors hover:bg-white/10 ${
        active ? "text-white" : "text-white/80 hover:text-white"
      }`}
    >
      {icon}
      <span className="truncate">{children}</span>
      <span className="ml-auto flex shrink-0 items-center gap-2">
        {badge}
        {active ? <CheckIcon className="size-4 text-white" /> : null}
      </span>
    </button>
  );
}

export function AiChip() {
  return (
    <span className="ai-chip">
      <svg viewBox="0 0 24 24" className="size-[9px] fill-current" aria-hidden>
        <path d="M12 1.5c.9 4.6 2.9 6.7 8 7.6-5.1.9-7.1 3-8 7.6-.9-4.6-2.9-6.7-8-7.6 5.1-.9 7.1-3 8-7.6Z" />
        <path d="M18.6 15c.45 2.3 1.45 3.35 4 3.8-2.55.45-3.55 1.5-4 3.8-.45-2.3-1.45-3.35-4-3.8 2.55-.45 3.55-1.5 4-3.8Z" />
      </svg>
      AI 生成
    </span>
  );
}

export function StepRow({
  label,
  value,
  onMinus,
  onPlus,
}: {
  label: string;
  value: string;
  onMinus: () => void;
  onPlus: () => void;
}) {
  return (
    <div className="flex items-center justify-between px-4 text-[13px] text-white/65">
      <span>{label}</span>
      <span className="flex items-center gap-1.5">
        <StepButton onClick={onMinus}>−</StepButton>
        <span className="w-[68px] text-center tabular-nums text-white/90">{value}</span>
        <StepButton onClick={onPlus}>+</StepButton>
      </span>
    </div>
  );
}

export function StepButton({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="size-6 rounded-[7px] border border-white/10 bg-white/[0.06] leading-none text-white/85 transition-colors hover:bg-white/[0.14] hover:text-white pointer-coarse:size-8 pointer-coarse:text-[13px]"
    >
      {children}
    </button>
  );
}

export function Toggle({
  on,
  onClick,
  children,
}: {
  on: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-full border px-3 py-1 text-[12px] font-medium transition-colors pointer-coarse:py-2 ${
        on
          ? "border-transparent bg-[var(--player-accent)] text-black"
          : "border-white/10 bg-white/[0.06] text-white/65 hover:bg-white/[0.14] hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}
