"use client";

import { useState, type ReactNode } from "react";

// ---------------------------------------------------------------------------
// 表单积木
// ---------------------------------------------------------------------------

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <div>
      <p className="mb-1.5 text-ui font-semibold text-white/85">{label}</p>
      {children}
      {hint && <p className="mt-1.5 text-caption leading-relaxed text-[var(--text-faint)]">{hint}</p>}
    </div>
  );
}

export function ToggleChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-full border px-3 py-1.5 text-sub font-medium transition ${
        active
          ? "border-white/25 bg-white/[0.14] text-white"
          : "border-white/[0.08] bg-white/[0.03] text-[var(--text-muted)] hover:bg-white/[0.07]"
      }`}
    >
      {children}
    </button>
  );
}

/**
 * 可折叠分段：折叠态直接显示该段的**当前摘要**——不展开也知道里面配了什么，
 * 这是这个长表单能收起来的前提（渐进披露，quality-upgrade.md §14.9）。
 */
export function Section({
  title,
  summary,
  children,
}: {
  title: string;
  summary: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-xl border border-white/[0.07] bg-white/[0.02]">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-3 px-4 py-3 text-left"
      >
        <span className="shrink-0 text-ui font-semibold text-white/85">{title}</span>
        <span className="min-w-0 flex-1 truncate text-right text-caption text-[var(--text-faint)]">
          {summary}
        </span>
        <span
          className={`shrink-0 text-[var(--text-faint)] transition-transform ${
            open ? "rotate-90" : ""
          }`}
        >
          ›
        </span>
      </button>
      {open && (
        <div className="space-y-5 border-t border-white/[0.06] px-4 pb-4 pt-4">{children}</div>
      )}
    </div>
  );
}

/**
 * 三态芯片：不限 → 只要 → 排除 → 不限。
 *
 * 平台有十几个选项，白/黑名单铺成两组芯片就是三十多个点击目标；三态把它压回
 * 一组，且每个芯片自解释（选中=只要，红色带 ✕=排除）。
 */
export function TriChip({
  state,
  onClick,
  children,
}: {
  state: "off" | "include" | "exclude";
  onClick: () => void;
  children: ReactNode;
}) {
  const tone =
    state === "include"
      ? "border-white/25 bg-white/[0.14] text-white"
      : state === "exclude"
        ? "border-red-400/35 bg-red-500/12 text-red-200"
        : "border-white/[0.08] bg-white/[0.03] text-[var(--text-muted)] hover:bg-white/[0.07]";
  return (
    <button
      type="button"
      onClick={onClick}
      className={`rounded-full border px-3 py-1.5 text-sub font-medium transition ${tone}`}
    >
      {state === "exclude" && <span className="mr-1">✕</span>}
      {children}
    </button>
  );
}

export function CheckRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center justify-between rounded-xl border border-white/[0.08] bg-white/[0.04] px-4 py-3">
      <span>
        <span className="block text-ui font-medium text-white/90">{label}</span>
        <span className="mt-0.5 block text-caption text-[var(--text-faint)]">{hint}</span>
      </span>
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="size-4 accent-[var(--accent-2)]"
      />
    </label>
  );
}

export function NumberInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder: string;
}) {
  return (
    <input
      type="number"
      min={0}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
    />
  );
}
