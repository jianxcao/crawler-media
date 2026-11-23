"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { useConfirm } from "@/components/feedback";
import { SearchIcon } from "@/components/icons";
import {
  CategoryChip,
  HistoryGroupRow,
  Kbd,
  ModeSwitch,
} from "@/components/search/search-command-rows";
import {
  clearSearchHistory,
  deleteSearchHistoryEntry,
  listSearchHistory,
  type SearchHistoryItem,
} from "@/lib/api/search";
import {
  CATEGORY_LABEL,
  SCOPE_ALL,
  scopeOfTab,
  type SearchScope,
  type SearchVertical,
} from "@/lib/categories";
import { useSearchAccess } from "@/lib/search-access";
import { historyReplayOptions } from "@/lib/search-history";
import {
  filterHistoryGroups,
  groupHistory,
  type HistoryGroup,
  isMediaHistory,
  readSearchPaletteState,
  tabKeyOf,
  writeSearchPaletteState,
} from "@/lib/search-palette";
import { useSearchPrefs } from "@/lib/search-prefs";
import type { SearchSubmitOptions } from "@/lib/search-url";

/**
 * 侧栏搜索入口 + 命令面板（Command Palette）。
 *
 * 全站唯一搜索入口，三模式（媒体优先）：触发器是品牌行右侧的放大镜图标按钮，
 * 点击（或 ⌘K）弹出面板。视觉对齐 Raycast/Spotlight 的实心深色浮层，
 * 提交与回放都走 onSearch：关键词 + 范围 + { vertical, snapshotId }。
 */

export interface SearchCommandProps {
  /** 提交搜索的回调：关键词 + 搜索范围（标签换算而来）。由上层负责落地展示。 */
  onSearch: (keyword: string, scope: SearchScope, options?: SearchSubmitOptions) => void;
  /** 触发键的样式覆盖。缺省是侧栏/顶栏那副玻璃行图标键。 */
  triggerClassName?: string;
}

export function SearchCommand({
  onSearch,
  // 移动端 44px：iOS HIG 最小可点目标（触屏），桌面保持 32px 紧凑图标键
  triggerClassName = "glass-row !size-8 shrink-0 justify-center !p-0 max-md:!size-11",
}: SearchCommandProps) {
  const searchAccess = useSearchAccess();
  const [open, setOpen] = useState(false);
  // 面板是全屏浮层，Portal 到 body：避免被 sidebar 玻璃面板的 isolation:isolate 层叠上下文困住。
  const [portalReady, setPortalReady] = useState(false);
  useEffect(() => setPortalReady(true), []);

  // 面板打开时给 body 挂 cmdk-open 类，驱动 .app-shell 轻微缩放后推（Spotlight 式纵深）
  useEffect(() => {
    document.body.classList.toggle("cmdk-open", open);
    return () => document.body.classList.remove("cmdk-open");
  }, [open]);

  // ⌘K / Ctrl+K 全局唤起；已打开时再次按下则关闭
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  if (searchAccess.ready && searchAccess.available.length === 0) return null;

  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        aria-label="搜索（⌘K 或 Ctrl+K）"
        aria-haspopup="dialog"
        title="搜索（⌘K 或 Ctrl+K）"
        className={triggerClassName}
      >
        <SearchIcon className="size-[18px] max-md:size-[22px]" />
      </button>

      {portalReady &&
        open &&
        createPortal(
          <SearchPalette
            onClose={() => setOpen(false)}
            onSearch={(keyword, scope, options) => {
              onSearch(keyword, scope, options);
              setOpen(false);
            }}
          />,
          document.body,
        )}
    </>
  );
}

/* —— 面板本体 —— */

function SearchPalette({
  onClose,
  onSearch,
}: {
  onClose: () => void;
  onSearch: (keyword: string, scope: SearchScope, options?: SearchSubmitOptions) => void;
}) {
  const { visibleTabs, loading: tabsLoading } = useSearchPrefs();
  const searchAccess = useSearchAccess();
  const confirm = useConfirm();
  const [keyword, setKeyword] = useState("");
  const [rememberedState] = useState(readSearchPaletteState);
  const [mode, setMode] = useState<SearchVertical>(rememberedState.mode);
  const [tabKey, setTabKey] = useState(rememberedState.tabKey);
  // null = 加载中；[] = 无历史
  const [items, setItems] = useState<SearchHistoryItem[] | null>(null);
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(() => new Set());
  // 键盘高亮的关键词组下标；-1 = 未选中（此时回车提交输入的关键词）
  const [sel, setSel] = useState(-1);
  const inputRef = useRef<HTMLInputElement>(null);
  const availableModes = searchAccess.available;

  // 挂载即聚焦输入框
  useEffect(() => inputRef.current?.focus(), []);

  useEffect(() => {
    if (!searchAccess.ready) return;
    if (availableModes.length === 0) {
      onClose();
      return;
    }
    if (!availableModes.includes(mode)) {
      const nextMode = searchAccess.firstAvailable ?? availableModes[0];
      setMode(nextMode);
      writeSearchPaletteState({ mode: nextMode, tabKey });
    }
  }, [availableModes, mode, onClose, searchAccess.firstAvailable, searchAccess.ready, tabKey]);

  // 等服务端搜索偏好加载完成后再校验。若上次分类已隐藏或删除，回退到「全部」。
  useEffect(() => {
    if (
      !tabsLoading &&
      tabKey !== "all" &&
      !visibleTabs.some((tab) => tabKeyOf(tab) === tabKey)
    ) {
      setTabKey("all");
      writeSearchPaletteState({ mode, tabKey: "all" });
    }
  }, [mode, tabKey, tabsLoading, visibleTabs]);

  const changeMode = (nextMode: SearchVertical) => {
    if (!availableModes.includes(nextMode)) return;
    setMode(nextMode);
    writeSearchPaletteState({ mode: nextMode, tabKey });
  };

  const changeTab = (nextTabKey: string) => {
    setTabKey(nextTabKey);
    writeSearchPaletteState({ mode, tabKey: nextTabKey });
  };

  // 历史存在后端（search_history 表）；limit 按关键词组计算，组内范围会完整返回。
  useEffect(() => {
    let cancelled = false;
    listSearchHistory()
      .then((list) => !cancelled && setItems(list))
      .catch(() => !cancelled && setItems([]));
    return () => {
      cancelled = true;
    };
  }, []);

  const groups = useMemo(
    () =>
      groupHistory(
        (items ?? []).filter((item) =>
          isMediaHistory(item) ? searchAccess.canMedia : searchAccess.canTorrent,
        ),
      ),
    [items, searchAccess.canMedia, searchAccess.canTorrent],
  );

  // 输入即过滤关键词组（子串匹配）；匹配后 UI 自动展开该组的所有具体范围。
  const filteredGroups = useMemo(() => filterHistoryGroups(groups, keyword), [groups, keyword]);

  // 过滤结果变化后旧下标可能越界，收敛到组列表末尾；空列表回到 -1。
  useEffect(() => {
    setSel((prev) => Math.min(prev, filteredGroups.length - 1));
  }, [filteredGroups.length]);

  const submit = () => {
    const kw = keyword.trim();
    if (!availableModes.includes(mode)) return;
    if (mode === "torrent") {
      // 资源模式允许空关键词 = 浏览选中分类的站点种子列表页
      const tab = visibleTabs.find((t) => tabKeyOf(t) === tabKey);
      onSearch(kw, tab ? scopeOfTab(tab) : SCOPE_ALL, { vertical: "torrent" });
      return;
    }
    // 影视/媒体库没有「浏览」语义（无分类维度），空词不提交，范围恒为「全部」
    if (!kw) return;
    onSearch(kw, SCOPE_ALL, { vertical: mode });
  };

  /** 点开一条历史：按记录自身的垂直回放对应结果快照。 */
  const pick = (item: SearchHistoryItem) => {
    onSearch(item.query, SCOPE_ALL, historyReplayOptions(item));
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    switch (event.key) {
      case "Escape":
        event.preventDefault();
        onClose();
        break;
      case "Tab":
        event.preventDefault();
        if (availableModes.length > 1) {
          const index = availableModes.indexOf(mode);
          changeMode(availableModes[(index + 1) % availableModes.length]);
        }
        break;
      case "ArrowDown":
        event.preventDefault();
        setSel((prev) => Math.min(prev + 1, filteredGroups.length - 1));
        break;
      case "ArrowUp":
        event.preventDefault();
        setSel((prev) => Math.max(prev - 1, -1));
        break;
      case "ArrowRight": {
        const group = filteredGroups[sel];
        if (!group || group.items.length === 1) break;
        event.preventDefault();
        setCollapsedGroups((prev) => {
          const next = new Set(prev);
          next.delete(group.key);
          return next;
        });
        break;
      }
      case "ArrowLeft": {
        const group = filteredGroups[sel];
        if (!group || group.items.length === 1) break;
        event.preventDefault();
        setCollapsedGroups((prev) => new Set(prev).add(group.key));
        break;
      }
      case "Enter":
        event.preventDefault();
        if (sel >= 0 && filteredGroups[sel]) pick(filteredGroups[sel].items[0]);
        else submit();
        break;
    }
  };

  const removeOne = (id: string) => {
    setItems((prev) => (prev ? prev.filter((i) => i.id !== id) : prev));
    deleteSearchHistoryEntry(id).catch(() => undefined);
  };

  const removeGroup = async (group: HistoryGroup) => {
    if (
      group.items.length > 1 &&
      !(await confirm({
        title: `删除「${group.keyword}」的 ${group.items.length} 条搜索记录？`,
        confirmLabel: "删除",
        tone: "danger",
      }))
    ) {
      return;
    }
    const ids = new Set(group.items.map((item) => item.id));
    setItems((prev) => (prev ? prev.filter((item) => !ids.has(item.id)) : prev));
    Promise.all(group.items.map((item) => deleteSearchHistoryEntry(item.id))).catch(
      () => undefined,
    );
  };

  const toggleGroup = (groupKey: string) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(groupKey)) next.delete(groupKey);
      else next.add(groupKey);
      return next;
    });
  };

  const removeAll = () => {
    setItems([]);
    clearSearchHistory().catch(() => undefined);
  };

  return (
    <div
      className="search-palette-overlay fixed inset-0 z-[80] flex items-start justify-center px-4 pt-[13vh] [bottom:calc(-1*var(--vp-overshoot))] max-md:px-2 max-md:pt-[calc(var(--safe-top)+10px)]"
      onMouseDown={(event) => event.target === event.currentTarget && onClose()}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="搜索"
        className="search-palette-panel dialog-surface flex w-full max-w-[600px] flex-col overflow-hidden rounded-2xl max-md:max-h-[calc(100dvh-var(--safe-top)-var(--keyboard-inset)-20px)]"
        onKeyDown={handleKeyDown}
      >
        {/* —— 输入行 —— */}
        <div className="flex items-center gap-3 pl-4 pr-3">
          <SearchIcon className="size-[17px] shrink-0 text-[var(--text-faint)]" />
          <input
            ref={inputRef}
            value={keyword}
            onChange={(event) => {
              setKeyword(event.target.value);
              setSel(-1);
            }}
            placeholder={
              mode === "media"
                ? "搜索电影、剧集… · 下次按 ⌘K / Ctrl+K 唤醒"
                : mode === "torrent"
                  ? "搜索资源或 IMDb ID… · 下次按 ⌘K / Ctrl+K 唤醒"
                  : "搜索已入库的影片… · 下次按 ⌘K / Ctrl+K 唤醒"
            }
            aria-label={
              mode === "media" ? "搜索影视条目" : mode === "torrent" ? "搜索站点资源" : "搜索媒体库"
            }
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            className="h-[52px] min-w-0 flex-1 bg-transparent text-body-lg text-[var(--text)] outline-none placeholder:text-[var(--text-faint)]"
          />
          <ModeSwitch mode={mode} modes={availableModes} onChange={changeMode} />
        </div>

        {/* —— 分类 chips（仅资源模式；影视/媒体库搜索没有分类维度）—— */}
        {mode === "torrent" && (
          <div className="flex flex-wrap gap-1.5 px-4 pb-3">
            <CategoryChip label="全部" active={tabKey === "all"} onClick={() => changeTab("all")} />
            {visibleTabs.map((tab) => (
              <CategoryChip
                key={tabKeyOf(tab)}
                label={tab.type === "category" ? CATEGORY_LABEL[tab.id] : tab.name}
                active={tabKey === tabKeyOf(tab)}
                onClick={() => changeTab(tabKeyOf(tab))}
              />
            ))}
          </div>
        )}

        <div className="h-px bg-white/[0.06]" />

        {/* —— 主体：最近搜索（媒体/资源混排）—— */}
        <div className="scroll-thin max-h-[336px] min-h-[96px] overflow-y-auto p-2 max-md:max-h-none max-md:min-h-0 max-md:flex-1">
          {items !== null && items.length > 0 && (
            <div className="flex items-center justify-between px-2.5 pb-1 pt-1">
              <span className="text-caption font-medium tracking-wide text-[var(--text-faint)]">
                最近搜索
              </span>
              <button
                type="button"
                onClick={removeAll}
                className="touch-target rounded-md px-1.5 py-0.5 text-caption text-[var(--text-faint)] transition-colors hover:bg-white/[0.08] hover:text-[var(--text-muted)]"
              >
                清空
              </button>
            </div>
          )}
          {items !== null && items.length === 0 && (
            <p className="px-2.5 py-6 text-center text-sub text-[var(--text-faint)]">
              还没有搜索记录，输入关键词回车开始搜索
            </p>
          )}
          {items !== null && items.length > 0 && filteredGroups.length === 0 && (
            <p className="px-2.5 py-6 text-center text-sub text-[var(--text-faint)]">
              没有匹配「{keyword.trim()}」的搜索记录，回车直接搜索
            </p>
          )}
          {filteredGroups.length > 0 && (
            <ul aria-label="最近搜索">
              {filteredGroups.map((group, index) => (
                <HistoryGroupRow
                  key={group.key}
                  group={group}
                  active={index === sel}
                  onHover={() => setSel(index)}
                  onPick={() => pick(group.items[0])}
                  expanded={keyword.trim().length > 0 || !collapsedGroups.has(group.key)}
                  onToggle={() => toggleGroup(group.key)}
                  onPickItem={pick}
                  onRemoveItem={removeOne}
                  onRemoveGroup={() => void removeGroup(group)}
                />
              ))}
            </ul>
          )}
        </div>

        {/* —— 页脚：左侧模式说明，右侧快捷键 —— */}
        <div className="flex h-10 shrink-0 items-center justify-between border-t border-white/[0.06] px-4">
          <span className="text-caption text-[var(--text-faint)]">
            {mode === "media"
              ? "在豆瓣中搜索影视条目"
              : mode === "torrent"
                ? "跨全部已配置站点搜索种子，留空回车 = 浏览该分类最新资源"
                : "在媒体库中搜索已入库的影片"}
          </span>
          <span className="flex items-center gap-3 text-caption text-[var(--text-faint)] max-md:hidden">
            <span className="flex items-center gap-1">
              <Kbd>⏎</Kbd> {mode === "torrent" && !keyword.trim() ? "浏览" : "搜索"}
            </span>
            <span className="flex items-center gap-1">
              <Kbd>Tab</Kbd> 切换范围
            </span>
            <span className="flex items-center gap-1">
              <Kbd>↑↓</Kbd> 历史
            </span>
            <span className="flex items-center gap-1">
              <Kbd>esc</Kbd> 关闭
            </span>
          </span>
        </div>
      </div>
    </div>
  );
}
