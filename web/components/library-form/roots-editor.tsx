"use client";

import { DirectoryPicker } from "@/components/directory-picker";
import { FolderIcon, PlusIcon, XIcon } from "@/components/icons";
import { Tooltip } from "@/components/tooltip";

/** 根目录列表：第一项为主根；行内可原位更改、设为主根、移除。 */
export function RootsEditor({
  roots,
  onChange,
  onPick,
}: {
  roots: string[];
  onChange: (next: string[]) => void;
  /** 打开目录选择器："add"=追加，数字=原位更改该下标 */
  onPick: (target: "add" | number) => void;
}) {
  return (
    <div className="space-y-1.5">
      {roots.map((root, i) => (
        <div
          key={root}
          className="group flex items-center gap-2 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2"
        >
          <FolderIcon className="size-4 shrink-0 text-[var(--accent)]/80" />
          {/* 保尾截断（dir=rtl）：路径的区分信息在尾部，省略号出现在头部；
              LRM 标记防止首尾的 "/" 在 RTL 下跳位。点击可原位更改目录 */}
          <Tooltip
            content={
              <>
                <p className="mb-1 break-all font-mono text-caption text-[var(--text-muted)]">{root}</p>
                点击更改：从当前路径开始重新选择目录。
              </>
            }
          >
            <button
              type="button"
              dir="rtl"
              onClick={() => onPick(i)}
              className="min-w-0 flex-1 truncate rounded text-left font-mono text-ui text-[var(--text)] transition-colors hover:text-[var(--accent)]"
            >
              {"‎" + root + "‎"}
            </button>
          </Tooltip>
          {i === 0 ? (
            <Tooltip
              content={
                <>
                  <strong>主根 = 新内容的落盘位置。</strong>
                  订阅与手动下载完成后，按「主根/标题 (年份)」建目录入库；
                  一个库可挂多个根，但写入点只有主根这一个。
                </>
              }
            >
              <span className="shrink-0 cursor-default rounded-full bg-[var(--accent)]/15 px-2 py-0.5 text-micro font-semibold text-[var(--accent)]">
                主根
              </span>
            </Tooltip>
          ) : (
            <Tooltip content="把该路径设为新内容的落盘位置（移到列表第一位）。已有文件不会被移动。">
              <button
                type="button"
                onClick={() => onChange([root, ...roots.filter((r) => r !== root)])}
                className="touch-reveal shrink-0 rounded-full px-2 py-0.5 text-micro font-medium text-[var(--text-faint)] opacity-0 transition-opacity hover:bg-white/10 hover:text-white group-hover:opacity-100"
              >
                设为主根
              </button>
            </Tooltip>
          )}
          <button
            type="button"
            aria-label={`移除 ${root}`}
            onClick={() => onChange(roots.filter((r) => r !== root))}
            className="shrink-0 rounded-md p-1 text-[var(--text-faint)] transition-colors hover:bg-white/10 hover:text-white"
          >
            <XIcon className="size-3.5" />
          </button>
        </div>
      ))}
      <button
        type="button"
        onClick={() => onPick("add")}
        className="flex w-full items-center justify-center gap-2 rounded-xl border border-dashed border-white/15 px-3 py-2.5 text-ui font-medium text-[var(--text-muted)] transition-colors hover:border-[var(--accent)]/50 hover:text-white"
      >
        <PlusIcon className="size-4" />
        {roots.length === 0 ? "浏览服务器目录并添加" : "添加目录"}
      </button>
    </div>
  );
}

/**
 * 服务端目录选择器的接线：追加时从最近添加的根起步，更改时从被改的根起步；
 * 追加去重，更改为原位替换（改主根仍是主根），撞上已有路径时合并去重。
 */
export function RootsPicker({
  target,
  roots,
  onChange,
  onClose,
}: {
  target: "add" | number | null;
  roots: string[];
  onChange: (next: string[]) => void;
  onClose: () => void;
}) {
  return (
    <DirectoryPicker
      open={target !== null}
      initialPath={
        target === "add" || target === null
          ? roots.length > 0
            ? roots[roots.length - 1]
            : undefined
          : roots[target]
      }
      onClose={onClose}
      onSelect={(path) => {
        if (target === "add" || target === null) {
          onChange(roots.includes(path) ? roots : [...roots, path]);
        } else {
          const next = roots.map((r, idx) => (idx === target ? path : r));
          onChange(next.filter((r, idx) => r !== path || idx === target));
        }
        onClose();
      }}
    />
  );
}
