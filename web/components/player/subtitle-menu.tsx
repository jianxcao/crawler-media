"use client";

import {
  clampSubtitleOffset,
  SUBTITLE_OFFSET_STEP,
  type SubtitleStyle,
  type SubtitleTracks,
} from "@/lib/player/subtitles";

import { AiChip, MenuItem, MenuPanel, StepRow, Toggle } from "./player-menu-primitives";

/** 字幕菜单：选轨 + 时间轴微调 + 外观。 */
export function SubtitleMenu({
  tracks,
  selected,
  onSelect,
  style,
  onStyleChange,
  systemRendered,
  onClose,
}: {
  tracks: SubtitleTracks;
  selected: string | null;
  onSelect: (ref: string | null) => void;
  style: SubtitleStyle;
  onStyleChange: (style: SubtitleStyle) => void;
  systemRendered: boolean;
  onClose: () => void;
}) {
  return (
    <MenuPanel title="字幕" onClose={onClose}>
      <div className="scroll-thin max-h-[240px] overflow-y-auto">
        <MenuItem
          active={selected === null}
          onClick={() => {
            onSelect(null);
            onClose();
          }}
        >
          关闭
        </MenuItem>
        {tracks.options.map((option) => (
          <MenuItem
            key={option.ref}
            active={selected === option.ref}
            badge={option.isAi ? <AiChip /> : null}
            onClick={() => {
              onSelect(option.ref);
              onClose();
            }}
          >
            {option.label}
          </MenuItem>
        ))}
        {tracks.unavailable.map((item) => (
          <div key={item.ref} className="px-4 py-1.5 text-white/35">
            <div className="truncate">{item.label}</div>
            <div className="text-[12px] leading-snug">{item.reason}</div>
          </div>
        ))}
        {tracks.options.length === 0 && tracks.unavailable.length === 0 ? (
          <div className="px-4 py-2 text-white/45">这个文件没有可用字幕</div>
        ) : null}
      </div>

      {tracks.options.some((option) => option.kind === "pgs") ? (
        <p className="mt-2 border-t border-white/10 px-4 pt-2.5 text-[12px] leading-relaxed text-white/55">
          图形字幕会转码压制进画面（切换约一秒），画中画等场景也能看到
        </p>
      ) : null}

      {selected && systemRendered ? (
        <p className="mt-2 border-t border-white/10 px-4 pt-2.5 text-[12px] leading-relaxed text-white/55">
          字幕由 iOS 系统渲染，样式在系统设置 → 辅助功能 → 字幕与隐藏式字幕中调整
        </p>
      ) : null}
      {selected && !systemRendered ? (
        <div className="mt-2 space-y-2 border-t border-white/10 pt-3">
          <StepRow
            label="时间轴"
            value={`${style.offsetSeconds > 0 ? "+" : ""}${style.offsetSeconds.toFixed(1)} 秒`}
            onMinus={() =>
              onStyleChange({
                ...style,
                offsetSeconds: clampSubtitleOffset(style.offsetSeconds - SUBTITLE_OFFSET_STEP),
              })
            }
            onPlus={() =>
              onStyleChange({
                ...style,
                offsetSeconds: clampSubtitleOffset(style.offsetSeconds + SUBTITLE_OFFSET_STEP),
              })
            }
          />
          <StepRow
            label="字号"
            value={`${style.fontScale.toFixed(1)}`}
            onMinus={() =>
              onStyleChange({ ...style, fontScale: Math.max(2, style.fontScale - 0.4) })
            }
            onPlus={() =>
              onStyleChange({ ...style, fontScale: Math.min(10, style.fontScale + 0.4) })
            }
          />
          <StepRow
            label="位置"
            value={`${style.bottomPercent}%`}
            onMinus={() =>
              onStyleChange({ ...style, bottomPercent: Math.max(0, style.bottomPercent - 2) })
            }
            onPlus={() =>
              onStyleChange({ ...style, bottomPercent: Math.min(40, style.bottomPercent + 2) })
            }
          />
          <div className="flex gap-2 px-4 pb-1">
            <Toggle
              on={style.outline}
              onClick={() => onStyleChange({ ...style, outline: !style.outline })}
            >
              描边
            </Toggle>
            <Toggle
              on={style.background}
              onClick={() => onStyleChange({ ...style, background: !style.background })}
            >
              背景
            </Toggle>
          </div>
        </div>
      ) : null}
    </MenuPanel>
  );
}
