import type { LibraryKind } from "@/lib/media-types";

// ---------------------------------------------------------------------------
// 自有 API 契约 DTO（docs/api-contracts/self.md §library / media）
// id 一律 UUID 字符串。
// ---------------------------------------------------------------------------

/** 库统计：真实台账统计。 */
export interface LibraryStats {
  item_count: number;
  file_count: number;
  total_size_bytes: number;
  strm_file_count?: number;
  /** 投影最后一次真实发布时间（毫秒）；0 = 从未刷新过。 */
  refreshed_at_ms?: number;
  /** 体积不是当前 scope 的最新事实（正在重建/投影落后）时为 true。 */
  stale?: boolean;
}

/** 媒体库（自契约 Library 的投影；组件引用名保持旧名 MediaLibrary）。 */
export interface MediaLibrary {
  id: string;
  name: string;
  /** 每库单一形态（movie / tv / video），创建后不可改 */
  kind: LibraryKind;
  /** 根路径列表（绝对路径），第一个为主根 */
  root_paths: string[];
  /** 是否为该类型的默认库 */
  is_default: boolean;
  /** 成员可见范围：everyone / selected */
  access_mode: LibraryAccessMode;
  /** 任一根目录在磁盘上不存在（被删/未挂载）——不能当「空库」展示 */
  root_missing: boolean;
  /** 超管浏览范围是否包含此库 */
  admin_visible: boolean;
  /** access_mode=selected 时可见的成员 id */
  member_ids: string[];
  /** 是否开启片头片尾标记检测 */
  detect_intros?: boolean;
  /** 是否开启音频声纹比对（针对无章节剧集） */
  enable_fingerprint?: boolean;
  /** 媒体库封面 URL（若已生成或用户上传） */
  cover_url?: string | null;
  /** 收藏范围规则 */
  match_rules?: MatchRule[];
  /** 关联的默认规则组 ID（新剧入库继承该规则组） */
  default_filter_id?: string | null;
  /** 是否开启实时目录监控 */
  realtime_watch?: boolean;
  /** 缺图时是否从视频抓帧生成封面 */
  generate_thumbnails?: boolean;
  /** 是否生成章节场景图 */
  extract_chapter_images?: boolean;
  /** 是否在首页展示排除此库 */
  exclude_from_home?: boolean;
  /** 是否按系列自动生成合集 */
  auto_series_collections?: boolean;
  /** 持久化删除状态；刷新或重新进入页面后继续跟踪。 */
  removal?: LibraryRemoval | null;
  /** 每个根目录最近一次改指操作的状态（建库/换根后由驱动推进）。 */
  root_reassignments?: RootReassignment[];
  /** 库存统计快照 */
  stats: LibraryStats;
}

/** 一次根目录改指的持久状态（对齐后端 `store::RootReassignment` 的投影）。 */
export interface RootReassignment {
  operation_id: string;
  library_id: string;
  /** 根目录的规范化 key（不是展示用的路径） */
  root_key: string;
  config_revision: number;
  /** queued / running / succeeded / failed / cancelled */
  state: string;
  revision: number;
  error: string | null;
  updated_at_ms: number;
}

// ---------------------------------------------------------------------------
// Library 配置类型
// ---------------------------------------------------------------------------

/** 收藏范围条件：条件间 AND、条件内任一匹配。 */
export interface MatchRule {
  field: "genres" | "origin_countries";
  op: "any_of";
  values: (number | string)[];
}

/** 库的可见范围模式。 */
export type LibraryAccessMode = "everyone" | "selected";

/** 创建/更新库的请求体（POST/PATCH /libraries）。 */
export interface LibraryPayload {
  name: string;
  kind: LibraryKind;
  root_paths: string[];
  generate_thumbnails?: boolean;
  extract_chapter_images?: boolean;
  exclude_from_home?: boolean;
  auto_series_collections?: boolean;
  access_mode?: LibraryAccessMode;
  admin_visible?: boolean;
  member_ids?: string[];
  detect_intros?: boolean;
  enable_fingerprint?: boolean;
  match_rules?: MatchRule[];
  default_filter_id?: string | null;
  realtime_watch?: boolean;
}

/**
 * 更新库的请求体：`root_paths` 省略表示不改目录。
 *
 * 改目录会冻结整库并异步迁移文件归属（202 + operation），只改其它字段仍是同步
 * 200；因此更新体必须允许省略 `root_paths`，否则「改名」会被后端当成换根。
 */
export type LibraryUpdatePayload = Partial<Omit<LibraryPayload, "kind">>;

/** 换根/换语言的异步操作（GET /libraries/{id}/operations/{operation_id}）。 */
export interface LibraryOperation {
  operation_id: string;
  kind: "roots" | "locale" | string;
  /** 持久操作状态。 */
  state: "planned" | "frozen" | "applying" | "succeeded" | "failed" | string;
  cursor?: string | null;
  error: string | null;
}

/** PATCH 结果：同步字段直接返回最新库；换根返回 202 受理的操作。 */
export type LibraryUpdate =
  | { kind: "library"; library: MediaLibrary }
  | { kind: "operation"; operation: LibraryOperation };

/** 收藏范围的可选项（GET /libraries/routing-options）。 */
export interface RoutingOptions {
  movie_genres: { id: number; label: string }[];
  tv_genres: { id: number; label: string }[];
  region_presets: { key: string; label: string; countries: string[] }[];
  country_names: Record<string, string>;
}

/** 扫描类任务的阶段。 */
export type ScanPhase =
  | "walking"
  | "ingesting"
  | "probing"
  | "assets"
  | "reidentifying"
  | "organizing";

/** 各阶段的界面文案（唯一出处，卡片与详情页共用）。 */
export const SCAN_PHASE_LABELS: Record<ScanPhase, string> = {
  walking: "正在盘点文件",
  ingesting: "正在扫描",
  probing: "正在补探画质与音轨",
  assets: "正在补齐海报与剧照",
  reidentifying: "正在重新识别条目",
  organizing: "正在整理文件名",
};

/** 阶段对应的一句补充说明（详情页胶囊用）。 */
export const SCAN_PHASE_HINTS: Record<ScanPhase, string> = {
  walking: "正在统计待处理的文件数",
  ingesting: "识别到的内容会自动入库",
  probing: "文件已全部入库，正在读取文件本体的规格",
  assets: "文件已全部入库，正在下载图片",
  reidentifying: "完成后条目身份会更新",
  organizing: "完成后自动刷新",
};

/** 扫描/整理的实时进度。 */
export interface ScanProgress {
  phase: ScanPhase;
  processed: number;
  total: number;
}

export interface LibraryRemoval {
  operation_id: string;
  library_id: string;
  state: "frozen" | "draining" | "cleaning" | "succeeded" | "failed";
  error: string | null;
}

export interface ScanOperationResponse {
  operation_id: string;
  status: string;
  queued: boolean;
  transferred: boolean;
}

/** 扫描操作的进度与报告（前端只消费 202 受理的响应，这里保留契约形状）。 */
export interface ScanOperationDetails {
  operation_id: string;
  status: string;
  generation: number;
  report: Record<string, unknown>;
}

/** 整理预览里跟随主文件改名的附属文件（字幕等）。 */
export interface OrganizeSidecar {
  source_path: string;
  target_path: string;
}

/** 整理预览里的一条改名计划。 */
export interface OrganizeRename {
  file_id: string;
  media_item_id: string;
  title: string;
  year: number | null;
  source_path: string;
  target_path: string;
  source_rel: string;
  target_rel: string;
  size_bytes: number;
  sidecars: OrganizeSidecar[];
}

/** 整理预览里的一条跳过说明。 */
export interface OrganizeSkip {
  file_path: string;
  reason: string;
}

/** 整理预览。 */
export interface OrganizePreview {
  total: number;
  already_ok: number;
  renames: OrganizeRename[];
  skips: OrganizeSkip[];
  entry_assets: OrganizeSidecar[];
}

/** 最近一次整理的结论。 */
export interface LastOrganize {
  finished_at: string;
  renamed: number;
  sidecars_renamed: number;
  entry_assets_moved: number;
  already_ok: number;
  skipped: number;
  removed_dirs: number;
  errors: string[];
}

/** 自有 API 的整理预览形状（后端按所选目标库和目标根返回 from/to）。 */
export interface OrganizeRenameEntry {
  file_id?: string;
  from: string;
  to: string;
  title?: string;
  kind?: string;
}

export interface OrganizePreviewResult {
  library_id?: string;
  root?: string;
  total?: number;
  already_ok: number;
  skips?: number;
  skipped?: number;
  renames: OrganizeRenameEntry[];
}

export interface OrganizePreviewOptions {
  targetLibraryId?: string;
  targetRoot?: string;
  signal?: AbortSignal;
}

/** 自动/重新生成媒体库封面（支持预览或正式应用）。 */
export interface GenerateLibraryCoverOptions {
  title_zh?: string;
  title_en?: string;
  style?: "macaron_card_single" | "multi_poster_pile";
  background?: {
    mode: "auto_extract" | "solid_color" | "gradient" | "custom_image";
    blur_radius?: number;
    color_ratio?: number;
    hex_color?: string;
    from_hex?: string;
    to_hex?: string;
  };
  preview_only?: boolean;
}

export type MediaInfoRefreshMode = "missing" | "force";

export interface MediaInfoRefreshReceipt {
  mode: MediaInfoRefreshMode;
  total: number;
  queued: number;
  skipped: number;
}

/** 元数据刷新是同步完成的：回执只有本次实际刷新的条目数。 */
export interface MetadataRefreshReceipt {
  refreshed: number;
}

export interface PathMissingRow {
  file_id: string;
  media_id: string;
  path: string;
  file_name: string;
  candidates: string[];
}
