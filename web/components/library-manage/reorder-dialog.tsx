"use client";

import { useEffect, useState } from "react";

import { ChevronDownIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import type { MediaLibrary } from "@/lib/api/libraries";
import { moveInList } from "@/lib/library-manage";

export function FilterChip({
  active,
  onClick,
  className = "",
  children,
}: {
  active: boolean;
  onClick: () => void;
  className?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={`flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-caption font-medium transition ${
        active
          ? "border-white/[0.2] bg-white/[0.14] text-[var(--text)]"
          : "border-white/[0.1] text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-[var(--text)]"
      } ${className}`}
    >
      {children}
    </button>
  );
}

/**
 * 手机端的排序弹窗：没有拖拽，上下箭头换位，确认后一次提交整单。
 * 弹窗内部持有一份顺序草稿，取消不影响列表。
 */
export function ReorderDialog({
  open,
  libraries,
  onClose,
  onSubmit,
}: {
  open: boolean;
  libraries: MediaLibrary[];
  onClose: () => void;
  onSubmit: (next: readonly MediaLibrary[]) => void;
}) {
  const [draft, setDraft] = useState<readonly MediaLibrary[]>(libraries);
  useEffect(() => {
    if (open) setDraft(libraries);
  }, [open, libraries]);
  const changed = draft.some((l, i) => l.id !== libraries[i]?.id);
  return (
    <Modal open={open} onClose={onClose} label="调整媒体库顺序">
      <div className="p-5">
        <h2 className="text-title font-bold text-white">调整顺序</h2>
        <p className="mt-1 text-sub text-[var(--text-muted)]">这也是首页「我的媒体库」的展示顺序。</p>
        <ol className="mt-4 divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08]">
          {draft.map((library, index) => (
            <li key={library.id} className="flex items-center gap-3 px-3 py-2.5">
              <span className="w-5 text-caption tabular-nums text-[var(--text-faint)]">{index + 1}</span>
              <span className="min-w-0 flex-1 truncate text-ui font-medium">{library.name}</span>
              <button
                type="button"
                aria-label={`「${library.name}」上移`}
                disabled={index === 0}
                onClick={() => setDraft((d) => moveInList(d, index, index - 1))}
                className="grid size-8 place-items-center rounded-full border border-white/[0.09] text-white/75 disabled:opacity-30"
              >
                <ChevronDownIcon className="size-4 rotate-180" />
              </button>
              <button
                type="button"
                aria-label={`「${library.name}」下移`}
                disabled={index === draft.length - 1}
                onClick={() => setDraft((d) => moveInList(d, index, index + 1))}
                className="grid size-8 place-items-center rounded-full border border-white/[0.09] text-white/75 disabled:opacity-30"
              >
                <ChevronDownIcon className="size-4" />
              </button>
            </li>
          ))}
        </ol>
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" onClick={onClose} className="btn-glass px-4 py-2 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={!changed}
            onClick={() => onSubmit(draft)}
            className="btn-accent rounded-full px-4 py-2 text-ui font-semibold disabled:opacity-40"
          >
            保存顺序
          </button>
        </div>
      </div>
    </Modal>
  );
}
