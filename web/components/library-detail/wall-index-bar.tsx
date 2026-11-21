"use client";

import { useCallback, useMemo, useRef, useState } from "react";

import type { LibraryIndexEntry } from "@/lib/api/libraries";
import { wallBucketOffsets } from "@/lib/library-wall-index";

/** 索引条的 27 档字母（A–Z + #）。 */
const WALL_INITIALS = [...Array.from({ length: 26 }, (_, i) => String.fromCharCode(65 + i)), "#"];

/** 索引条整高的上限（px）：27 档 × 18px。视口更矮时按视口等比压缩每格。 */
const WALL_INDEX_MAX_HEIGHT = WALL_INITIALS.length * 18;
/** 索引条最多占视口高度的比例：留出上下呼吸空间，也保证吸附居中有余量 */
const WALL_INDEX_HEIGHT = `min(${WALL_INDEX_MAX_HEIGHT}px, 72dvh)`;

export function WallIndexBar({
  index,
  active,
  onJump,
  reversed,
}: {
  index: LibraryIndexEntry[];
  /** 当前视口所在档位；由海报墙滚动锚点实时更新。 */
  active: string | null;
  /** 参数是该档第一格在整份排序里的绝对位置（档位位置由 count 前缀和还原） */
  onJump: (offset: number) => void;
  /** 墙是倒序（Z→A）时字母表跟着倒过来：往下滑墙与往下滑索引条是同一个方向 */
  reversed: boolean;
}) {
  const byInitial = useMemo(() => new Map(index.map((e) => [e.initial, e])), [index]);
  // 档位 → 它在整份排序里的绝对起点：游标只到「这一档第一格」，落点仍按位置算
  const startsByInitial = useMemo(() => {
    const offsets = wallBucketOffsets(index);
    return new Map(index.map((entry, bucket) => [entry.initial, offsets[bucket]]));
  }, [index]);
  const slots = useMemo(() => (reversed ? [...WALL_INITIALS].reverse() : WALL_INITIALS), [reversed]);
  const barRef = useRef<HTMLDivElement>(null);
  // 正在滑选的字母（气泡预览 + 高亮）；null = 没在按压
  const [preview, setPreview] = useState<string | null>(null);
  // 是否处于按压/滑动中：pointermove 在鼠标悬停时也会触发，得靠它区分
  const dragging = useRef(false);

  /** 指尖纵坐标 → 档位字母：格子是 flex 等分的，按比例换算即可 */
  const letterAtY = useCallback((clientY: number) => {
    const rect = barRef.current?.getBoundingClientRect();
    if (!rect || rect.height === 0) return null;
    const ratio = (clientY - rect.top) / rect.height;
    const idx = Math.min(slots.length - 1, Math.max(0, Math.floor(ratio * slots.length)));
    return slots[idx];
  }, [slots]);

  if (index.length === 0) return null;

  const previewEntry = preview ? byInitial.get(preview) : undefined;
  const previewIdx = preview ? slots.indexOf(preview) : -1;

  return (
    <div
      ref={barRef}
      role="navigation"
      aria-label="按首字母跳转"
      className="sticky flex shrink-0 touch-none select-none flex-col items-stretch text-micro font-semibold leading-none text-[var(--text-faint)]"
      style={{
        height: WALL_INDEX_HEIGHT,
        // 吸附点 = 视口垂直居中；视口过矮时保底离顶 16px
        top: `max(16px, calc((100dvh - ${WALL_INDEX_HEIGHT}) / 2))`,
      }}
      onPointerDown={(e) => {
        // 阻掉按下的默认行为（文字选择/图片拖拽），并捕获指针：
        // 手指滑出条外仍持续收到 move/up，选择不中断
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        dragging.current = true;
        setPreview(letterAtY(e.clientY));
      }}
      onPointerMove={(e) => {
        if (dragging.current) setPreview(letterAtY(e.clientY));
      }}
      onPointerUp={(e) => {
        if (!dragging.current) return;
        dragging.current = false;
        const initial = letterAtY(e.clientY) ?? "";
        const start = startsByInitial.get(initial);
        if (start !== undefined) onJump(start);
        setPreview(null);
      }}
      onPointerCancel={() => {
        dragging.current = false;
        setPreview(null);
      }}
    >
      {slots.map((initial) => {
        const entry = byInitial.get(initial);
        return (
          <span
            key={initial}
            title={entry ? `${initial} · ${entry.count} 部` : undefined}
            className={`flex min-h-0 w-[18px] flex-1 items-center justify-center rounded transition-colors max-md:w-5 ${
              !entry
                ? "text-white/15"
                : initial === preview
                  ? "bg-[var(--accent)]/40 text-white"
                  : initial === active
                    ? "bg-[var(--accent)]/25 text-white"
                    : "text-white/55 hover:bg-white/10 hover:text-white"
            }`}
          >
            {initial}
          </span>
        );
      })}
      {/* 滑选气泡：跟着指尖所在档位浮在条左侧，大字预览 + 该档作品数。
          指尖压着的字母本身被手指挡住，没有它滑选等于盲选 */}
      {preview && previewIdx >= 0 && (
        <div
          className="pointer-events-none absolute right-full mr-3 flex -translate-y-1/2 items-center gap-2 rounded-xl border border-white/[0.14] bg-[rgba(16,18,26,0.92)] px-3.5 py-2 shadow-[0_8px_28px_rgba(0,0,0,0.5)]"
          style={{ top: `${((previewIdx + 0.5) / slots.length) * 100}%` }}
        >
          <span className="text-title-lg font-bold text-white">{preview}</span>
          <span className="whitespace-nowrap text-caption text-[var(--text-muted)]">
            {previewEntry ? `${previewEntry.count} 部` : "无作品"}
          </span>
        </div>
      )}
    </div>
  );
}
