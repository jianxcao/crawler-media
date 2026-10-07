/**
 * 媒体库管理页（/library/manage）的纯函数：状态归类、摘要、筛选、换位。
 *
 * 设计见 docs/design/library-manage.md §2.2。放在这里而不是组件里，是为了
 * 能用 node --test 直接跑单测（本目录的 .ts 纯模块由 node 原生剥类型执行），
 * 因此这里只允许 `import type` 与带扩展名的同目录纯模块，不能引入带 `@/`
 * 别名或浏览器依赖的模块。
 *
 * 扫描/整理/刷新的实时进度仍不在 GET /libraries 下行；状态列因此恒为空闲。
 * 可见范围、首页排除、实时监控读库对象上的真实字段（access_mode 等）。
 */

import type { ChapterJobProgress, MediaLibrary, ScanPhase } from "./api/libraries";
import { formatBytes } from "./format.ts";
import type { LibraryKind } from "./media-types";

/** 状态列的语气：决定圆点 / 胶囊颜色（灰 / 蓝 / 黄 / 红）。 */
export type LibraryStatusTone = "idle" | "busy" | "pending" | "missing";

/** 状态列的种类：决定文案模板与菜单里「扫描」「刷新」的当前形态。 */
export type LibraryStatusKind =
  | "scan"
  | "organize"
  | "refresh"
  | "chapters"
  | "importing"
  | "missing"
  | "unidentified"
  | "idle";

export interface LibraryStatus {
  tone: LibraryStatusTone;
  kind: LibraryStatusKind;
  /** 主文案（第一行） */
  title: string;
  /** 补充文案（第二行）；没有则为空串 */
  detail: string;
  /** 0-100 的进度百分比；分母未知或不是进度型状态时为 null */
  percent: number | null;
}

export interface LibraryStatusContext {
  /** 扫描阶段 → 文案（传 SCAN_PHASE_LABELS；纯模块不直接依赖 API 模块） */
  phaseLabels: Record<ScanPhase, string>;
  /** ISO 时间 → 「X 前」 */
  relativeTime: (iso: string) => string;
}

function percentOf(processed: number, total: number): number | null {
  if (total <= 0) return null;
  return Math.min(100, Math.round((processed / total) * 100));
}

/**
 * 一行库的状态归类。
 *
 * GET /libraries 不下发扫描/整理/刷新/章节作业的实时进度，因此恒为「空闲」。
 * 根路径缺失见 `root_missing`（`libraryNeedsAttention`）。
 */
export function libraryStatus(library: MediaLibrary, ctx: LibraryStatusContext): LibraryStatus {
  void library;
  void ctx;
  return {
    tone: "idle",
    kind: "idle",
    title: "空闲",
    detail: "",
    percent: null,
  };
}

function chapterJobRunning(job: ChapterJobProgress): boolean {
  return job.status === "running" || job.status === "cancelling";
}

/**
 * 「生成章节」菜单项与状态列共用的一句话：没作业时是动作名，有作业时如实
 * 说到哪了。列表接口没有章节作业进度，管理页传入 null 时显示动作名。
 */
export function chapterJobLabel(job: ChapterJobProgress | null | undefined): string {
  if (!job) return "生成章节";
  if (job.stopping) return "正在停止生成章节";
  if (!chapterJobRunning(job)) return "生成章节排队中";
  const percent = percentOf(job.processed, job.total);
  return percent === null ? "正在生成章节" : `正在生成章节 ${percent}%`;
}

/** 是否有长任务在跑。列表接口没有作业进度，恒 false。 */
export function libraryIsBusy(library: MediaLibrary): boolean {
  void library;
  return false;
}

/** 根路径在磁盘上不存在时需要处理。 */
export function libraryNeedsAttention(library: MediaLibrary): boolean {
  return library.root_missing;
}

/** 页头摘要：规模事实一句话，加上真正要你看的两个数。 */
export interface LibrarySummary {
  /** `18 个媒体库 · 4896 个条目 · 31.6 TB` */
  facts: string;
  /** 在跑任务的库数 */
  busy: number;
  /** 有待处理文件的库数 */
  attention: number;
  /** 待处理里是否含缺失文件（缺失比待识别更急，摘要胶囊随之用红） */
  missing: boolean;
}

export function summarizeLibraries(libraries: MediaLibrary[]): LibrarySummary {
  const items = libraries.reduce((sum, l) => sum + l.stats.item_count, 0);
  const bytes = libraries.reduce((sum, l) => sum + l.stats.total_size_bytes, 0);
  const attention = libraries.filter(libraryNeedsAttention).length;
  return {
    facts: `${libraries.length} 个媒体库 · ${items} 个条目 · ${formatBytes(bytes)}`,
    busy: 0,
    attention,
    missing: attention > 0,
  };
}

/** 摘要胶囊对应的筛选：只看在跑任务的库 / 只看有待处理的库。 */
export type LibraryFocus = "busy" | "attention";

export interface LibraryFilter {
  /** 搜索词：匹配库名或任一根目录，大小写不敏感；空串不过滤 */
  query: string;
  /** 类型：null 为全部 */
  kind: LibraryKind | null;
  /** 状态：null 为全部 */
  focus: LibraryFocus | null;
}

export const EMPTY_FILTER: LibraryFilter = { query: "", kind: null, focus: null };

export function filterIsActive(filter: LibraryFilter): boolean {
  return filter.query.trim() !== "" || filter.kind !== null || filter.focus !== null;
}

/** 客户端筛选：库列表规模在几十以内，一次全拉后本地过滤即可。 */
export function filterLibraries(libraries: MediaLibrary[], filter: LibraryFilter): MediaLibrary[] {
  const q = filter.query.trim().toLowerCase();
  return libraries.filter((library) => {
    if (filter.kind !== null && library.kind !== filter.kind) return false;
    if (filter.focus === "busy" && !libraryIsBusy(library)) return false;
    if (filter.focus === "attention" && !libraryNeedsAttention(library)) return false;
    if (q === "") return true;
    if (library.name.toLowerCase().includes(q)) return true;
    return library.root_paths.some((root) => root.toLowerCase().includes(q));
  });
}

/**
 * 把 from 位置的元素挪到 to 位置（其余元素相对顺序不变），返回新数组。
 * 拖拽松手与键盘 Alt+↑/↓ 都走这里；越界或原地不动时返回原数组引用，
 * 调用方据此跳过提交。
 */
export function moveInList<T>(list: readonly T[], from: number, to: number): readonly T[] {
  if (from === to) return list;
  if (from < 0 || to < 0 || from >= list.length || to >= list.length) return list;
  const next = [...list];
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

/** 可见范围的文案。 */
export function accessLabel(library: MediaLibrary): string {
  if (library.access_mode === "selected") {
    const n = library.member_ids.length;
    return n === 0 ? "仅指定成员（未选）" : `指定 ${n} 名成员`;
  }
  return "全部成员";
}

/** 可见范围是否偏离「全部成员」。 */
export function accessRestricted(library: MediaLibrary): boolean {
  return library.access_mode === "selected";
}

/** 库名下的配置备注：只说偏离默认的部分。 */
export function configNotes(library: MediaLibrary): string[] {
  const notes: string[] = [];
  if (library.exclude_from_home) notes.push("不在首页");
  if (library.realtime_watch === false) notes.push("未开实时监控");
  return notes;
}

/** 库存列的文案：影视库按「部」、图片库按「张」、其他库按「条目」。 */
export function inventoryLabel(library: MediaLibrary): { primary: string; secondary: string } {
  const unit = library.kind === "video" ? "个条目" : "部";
  return {
    primary: `${library.stats.item_count} ${unit}`,
    secondary: `${library.stats.file_count} 个文件`,
  };
}
