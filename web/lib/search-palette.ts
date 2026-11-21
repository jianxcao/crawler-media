import type { SearchHistoryItem } from "./api/search.ts";
import type { SearchTab, SearchVertical } from "./categories.ts";
import { isMediaHistoryProvider } from "./search-history.ts";

/** 搜索面板上次停留位置的浏览器级记忆，不随账号同步。 */
export const SEARCH_PALETTE_STATE_KEY = "crawler-media.search-palette-state";

export interface SearchPaletteState {
  mode: SearchVertical;
  tabKey: string;
}

const DEFAULT_PALETTE_STATE: SearchPaletteState = { mode: "media", tabKey: "all" };

/** 校验并解析本地保存的面板状态；非法 JSON 或未知枚举值回退默认值。 */
export function parseSearchPaletteState(raw: string | null): SearchPaletteState {
  if (!raw) return DEFAULT_PALETTE_STATE;
  try {
    const value = JSON.parse(raw) as Partial<SearchPaletteState> | null;
    return {
      mode:
        value?.mode === "torrent" || value?.mode === "media" || value?.mode === "library"
          ? value.mode
          : DEFAULT_PALETTE_STATE.mode,
      tabKey:
        typeof value?.tabKey === "string" && value.tabKey.length > 0
          ? value.tabKey
          : DEFAULT_PALETTE_STATE.tabKey,
    };
  } catch {
    return DEFAULT_PALETTE_STATE;
  }
}

/**
 * 从 localStorage 恢复搜索模式和资源分类。
 */
export function readSearchPaletteState(): SearchPaletteState {
  try {
    return parseSearchPaletteState(localStorage.getItem(SEARCH_PALETTE_STATE_KEY));
  } catch {
    return DEFAULT_PALETTE_STATE;
  }
}

/** localStorage 不可用时仅失去记忆能力，不影响搜索本身。 */
export function writeSearchPaletteState(state: SearchPaletteState): void {
  try {
    localStorage.setItem(SEARCH_PALETTE_STATE_KEY, JSON.stringify(state));
  } catch {
    // 隐私模式或存储空间不足时静默降级
  }
}

/** 标签在分类 chips 里的选中态 key：类型加前缀，内置分类与预设的 id 不会互相撞。 */
export function tabKeyOf(tab: SearchTab): string {
  return `${tab.type}:${tab.id}`;
}

/** 同关键词下的媒体搜索与各资源范围记录，默认在历史列表里折叠成一行。 */
export interface HistoryGroup {
  /** 忽略首尾空格和英文大小写后的分组键。 */
  key: string;
  /** 使用组内最近一条记录的原文展示关键词。 */
  keyword: string;
  /** 组内按最近搜索时间倒序，首条即主行回车时打开的记录。 */
  items: SearchHistoryItem[];
}

export function isMediaHistory(item: SearchHistoryItem): boolean {
  return isMediaHistoryProvider(item.provider);
}

/** 把接口返回的有序记录折叠为关键词组，保持后端给出的组顺序与组内顺序。 */
export function groupHistory(items: SearchHistoryItem[]): HistoryGroup[] {
  const groups = new Map<string, HistoryGroup>();
  for (const item of items) {
    const key = item.query.trim().toLocaleLowerCase();
    const group = groups.get(key);
    if (group) group.items.push(item);
    else groups.set(key, { key, keyword: item.query, items: [item] });
  }
  return [...groups.values()];
}

/** 输入关键词时按小写子串过滤关键词组。 */
export function filterHistoryGroups(groups: HistoryGroup[], keyword: string): HistoryGroup[] {
  const needle = keyword.trim().toLowerCase();
  if (!needle) return groups;
  return groups.filter((group) => group.key.includes(needle));
}
