"use client";

import type { SearchHistoryItem } from "@/lib/api/search";
import type { SearchVertical } from "@/lib/categories";
import { type HistoryGroup, isMediaHistory } from "@/lib/search-palette";
import { formatRelativeTime } from "@/lib/time";

/** 「影视 | 资源 | 媒体库」分段：输入行右侧的紧凑三段开关（Raycast 的 scope 选择位）。 */
export function ModeSwitch({
  mode,
  modes,
  onChange,
}: {
  mode: SearchVertical;
  modes: SearchVertical[];
  onChange: (mode: SearchVertical) => void;
}) {
  const options: { id: SearchVertical; label: string; hint: string }[] = [
    { id: "media", label: "影视", hint: "影视条目（豆瓣）" },
    { id: "torrent", label: "资源", hint: "跨站点种子搜索" },
    { id: "library", label: "媒体库", hint: "已入库的本地影片" },
  ];
  const visibleOptions = options.filter((option) => modes.includes(option.id));
  if (visibleOptions.length <= 1) return null;
  return (
    <div
      role="radiogroup"
      aria-label="搜索范围"
      className="flex shrink-0 gap-0.5 rounded-lg bg-white/[0.06] p-0.5"
    >
      {visibleOptions.map((opt) => {
        const active = opt.id === mode;
        return (
          <button
            key={opt.id}
            type="button"
            role="radio"
            aria-checked={active}
            title={opt.hint}
            // mousedown 抢焦点会让输入框失焦，preventDefault 保持焦点常驻输入框
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onChange(opt.id)}
            className={`rounded-md px-2.5 py-1 text-sub font-medium transition-colors ${
              active
                ? "bg-white/[0.13] text-[var(--text)]"
                : "text-[var(--text-muted)] hover:text-[var(--text)]"
            }`}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}

/** 分类 chip（资源模式）：展示哪些标签、什么顺序由「设置 → 搜索」的偏好决定。 */
export function CategoryChip({
  label,
  active,
  onClick,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
      className={`rounded-full px-2.5 py-[3px] text-sub transition-colors ${
        active
          ? "bg-white/[0.14] font-medium text-[var(--text)]"
          : "text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-[var(--text)]"
      }`}
    >
      {label}
    </button>
  );
}

/**
 * 关键词组主行：默认只占一行，点击/回车打开最近一次搜索；右侧箭头按需展开
 * 媒体及各资源范围。
 */
export function HistoryGroupRow({
  group,
  active,
  onHover,
  onPick,
  expanded,
  onToggle,
  onPickItem,
  onRemoveItem,
  onRemoveGroup,
}: {
  group: HistoryGroup;
  active: boolean;
  onHover: () => void;
  onPick: () => void;
  expanded: boolean;
  onToggle: () => void;
  onPickItem: (item: SearchHistoryItem) => void;
  onRemoveItem: (id: string) => void;
  onRemoveGroup: () => void;
}) {
  const latest = group.items[0];
  const mediaCount = group.items.filter((item) => isMediaHistory(item)).length;
  const torrentCount = group.items.length - mediaCount;

  // 单条记录不制造「只有一个孩子的分组」：沿用旧版扁平行，点击即进入该记录。
  if (group.items.length === 1) {
    return (
      <HistorySingleRow
        item={latest}
        active={active}
        onHover={onHover}
        onPick={onPick}
        onRemove={onRemoveGroup}
      />
    );
  }

  return (
    <li className="group/history">
      <div
        className={`flex items-center rounded-[10px] transition-colors ${
          active ? "bg-white/[0.07]" : ""
        }`}
        onMouseEnter={onHover}
      >
        <button
          type="button"
          onClick={onPick}
          className="flex min-w-0 flex-1 items-center gap-2.5 py-2 pl-2.5 text-left"
        >
          <span className="min-w-0 flex-1 truncate text-ui leading-5 text-[var(--text)]/90">
            {group.keyword}
          </span>
          {group.items.length > 1 ? (
            <>
              <span className="shrink-0 rounded-md bg-white/[0.07] px-1.5 py-0.5 text-micro text-[var(--text-muted)]">
                {group.items.length} 种范围
              </span>
              <span className="hidden shrink-0 text-micro text-[var(--text-faint)] sm:inline">
                {mediaCount > 0 && `影视 ${mediaCount}`}
                {mediaCount > 0 && torrentCount > 0 && " · "}
                {torrentCount > 0 && `资源 ${torrentCount}`}
              </span>
            </>
          ) : (
            <HistoryTypeBadges item={latest} />
          )}
          <span className="shrink-0 text-caption text-[var(--text-faint)]">
            {formatRelativeTime(latest.created_at)}
          </span>
        </button>
        <button
          type="button"
          aria-label={expanded ? `收起 ${group.keyword} 的搜索范围` : `展开 ${group.keyword} 的搜索范围`}
          aria-expanded={expanded}
          onMouseDown={(event) => event.preventDefault()}
          onClick={onToggle}
          className="touch-target mx-0.5 rounded-md p-1.5 text-[var(--text-faint)] transition-colors hover:bg-white/[0.08] hover:text-[var(--text-muted)]"
        >
          <svg
            viewBox="0 0 20 20"
            className={`size-3.5 transition-transform ${expanded ? "rotate-90" : ""}`}
            fill="none"
            stroke="currentColor"
            strokeWidth={1.8}
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="m7.5 4.5 5 5-5 5" />
          </svg>
        </button>
        <DeleteHistoryButton
          label={`删除搜索历史组：${group.keyword}`}
          onClick={onRemoveGroup}
          className="touch-reveal mr-1 opacity-0 group-hover/history:opacity-100"
        />
      </div>

      {expanded && (
        <ul className="ml-[18px] border-l border-white/[0.07] py-0.5 pl-3">
          {group.items.map((item) => (
            <HistoryVariantRow
              key={item.id}
              item={item}
              onPick={() => onPickItem(item)}
              onRemove={() => onRemoveItem(item.id)}
            />
          ))}
        </ul>
      )}
    </li>
  );
}

/**
 * 只有一条记录的关键词保持旧版扁平展示：类型、资源分类、快照和时间同层呈现。
 */
export function HistorySingleRow({
  item,
  active,
  onHover,
  onPick,
  onRemove,
}: {
  item: SearchHistoryItem;
  active: boolean;
  onHover: () => void;
  onPick: () => void;
  onRemove: () => void;
}) {
  return (
    <li className="group/single relative">
      <button
        type="button"
        onMouseEnter={onHover}
        onClick={onPick}
        className={`flex w-full items-center gap-2.5 rounded-[10px] px-2.5 py-2 text-left transition-colors ${
          active ? "bg-white/[0.07]" : ""
        }`}
      >
        <span className="min-w-0 flex-1 truncate text-ui leading-5 text-[var(--text)]/90">
          {item.query}
        </span>
        <HistoryTypeBadges item={item} />
        <span className="shrink-0 text-caption text-[var(--text-faint)] transition-opacity group-hover/single:opacity-0">
          {formatRelativeTime(item.created_at)}
        </span>
      </button>
      <DeleteHistoryButton
        label={`删除搜索历史：${item.query}`}
        onClick={onRemove}
        // !absolute 是必需的：.touch-target 在移动端的无层 position:relative
        // 会顶掉普通 absolute（同 media-track-rows / sidebar 的既有处理）
        className="touch-reveal !absolute right-2 top-1/2 -translate-y-1/2 opacity-0 group-hover/single:opacity-100"
      />
    </li>
  );
}

/** 展开态中的具体搜索范围：不重复关键词，只显示垂直、分类、快照和时间。 */
export function HistoryVariantRow({
  item,
  onPick,
  onRemove,
}: {
  item: SearchHistoryItem;
  onPick: () => void;
  onRemove: () => void;
}) {
  return (
    <li className="group/variant flex items-center rounded-lg transition-colors hover:bg-white/[0.05]">
      <button
        type="button"
        onClick={onPick}
        className="flex min-w-0 flex-1 items-center gap-2 py-1.5 pl-2 text-left"
      >
        <span className="min-w-0 flex-1 truncate text-sub text-[var(--text-muted)]">
          {isMediaHistory(item) ? "影视" : "资源"}
        </span>
        <span className="shrink-0 text-micro text-[var(--text-faint)]">
          {formatRelativeTime(item.created_at)}
        </span>
      </button>
      <DeleteHistoryButton
        label={`删除搜索历史：${item.query}（${isMediaHistory(item) ? "影视" : "资源"}）`}
        onClick={onRemove}
        className="touch-reveal mr-1 opacity-0 group-hover/variant:opacity-100"
      />
    </li>
  );
}

/** 单记录组在主行直接显示垂直，不必展开才能辨认。 */
export function HistoryTypeBadges({ item }: { item: SearchHistoryItem }) {
  const isMedia = isMediaHistory(item);
  return (
    <span
      className={`shrink-0 rounded-md px-1.5 py-0.5 text-micro ${
        isMedia
          ? "bg-[var(--accent-soft)] text-[var(--accent-2)]"
          : "bg-white/[0.07] text-[var(--text-muted)]"
      }`}
    >
      {isMedia ? "影视" : "资源"}
    </span>
  );
}

/** 组与子记录共用的删除按钮；mousedown 不抢走搜索输入框焦点。 */
export function DeleteHistoryButton({
  label,
  onClick,
  className,
}: {
  label: string;
  onClick: () => void;
  className?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
      className={`touch-target rounded-md p-1 text-[var(--text-faint)] transition-opacity hover:bg-white/[0.1] hover:text-[var(--text-muted)] ${className ?? ""}`}
    >
      <svg
        viewBox="0 0 24 24"
        className="size-[13px]"
        fill="none"
        stroke="currentColor"
        strokeWidth={2}
        strokeLinecap="round"
        aria-hidden="true"
      >
        <path d="m6 6 12 12M18 6 6 18" />
      </svg>
    </button>
  );
}

export function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="rounded bg-white/[0.07] px-1 py-px font-sans text-micro text-[var(--text-muted)]">
      {children}
    </kbd>
  );
}
