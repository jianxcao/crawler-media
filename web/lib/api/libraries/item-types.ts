import type { LibraryFilter } from "@/lib/library-filter";
import type { LibraryKind, MediaType } from "@/lib/media-types";

/** 库内一个媒体条目的库存聚合（单库海报墙的一格）。 */
export interface LibraryItem {
  media_item_id: string;
  kind: LibraryKind;
  /** 这一格属于哪个库 */
  library_id: string;
  title: string;
  year: number | null;
  rating?: number | null;
  studio?: string | null;
  studio_logo?: string | null;
  poster_url: string | null;
  backdrop_url?: string | null;
  file_count: number;
  /** 在库的季号列表（电影为空） */
  seasons: number[];
  /** 去重的 (季,集) 单元数（电影为 0） */
  episode_count: number;
  /** 该条目的文件是否已从磁盘消失（核验打标） */
  missing: boolean;
}

/** One physical file, with cached probe data when available. */
export interface LibraryItemFile {
  file_id: string;
  path: string;
  /** 完整覆盖：一个物理文件可服务多集；`season`/`episode` 只是首个具体坐标。 */
  units: Array<{ season: number; episode: number }>;
  /** 在位状态；missing/unknown 仍可查看但不可播放。 */
  presence: "present" | "missing" | "unknown";
  publication: "published" | "pending";
  season: number | null;
  episode: number | null;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string | null;
  /** ffprobe 抽取的音轨 / 字幕轨（file_details 缓存；未探测为空数组） */
  audio_tracks?: AudioStream[];
  subtitle_tracks?: SubtitleStream[];
  /** ffprobe 抽取的主视频流信息（file_details 缓存；未探测为 null） */
  video_track?: VideoTrackInfo | null;
  /** 是否已探测完成（false = 后台探测中，前端显示「探测中」并轮询） */
  probed?: boolean;
  /** 后台仍在排队或探测，包括流信息完成后的声纹阶段。 */
  probe_queued?: boolean;
}

export interface LibraryItemFilePage {
  files: LibraryItemFile[];
  next_cursor: string | null;
}

/** 条目详情：基本信息 + 逐文件真实介质规格。 */
export interface LibraryItemCastMember {
  name: string;
  role: string | null;
  /** TMDB person id：非空时演职员可点进人物页 */
  tmdb_person_id: number | null;
  /** 人物头像（TMDB profile path 经图片代理） */
  avatar_url: string | null;
}

export interface LibraryItemBrand {
  name: string;
  logo_url: string | null;
  kind?: "network" | "studio";
}

export type EpisodeAvailabilityStatus =
  | "in_library"
  | "downloading"
  | "file_missing"
  | "missing"
  | "unaired";

export interface LibrarySeasonSummary {
  season_number: number;
  owned_count: number;
  total_count: number;
  downloading_count: number;
}

export interface LibraryItemDetail {
  media_item_id: string;
  kind: LibraryKind;
  title: string;
  year: number | null;
  original_title: string | null;
  tmdb_id: string | null;
  poster_url: string | null;
  /** 条目目录 NFO 刮削产物（overview/评分/片长/类型/演职员），缺失为 null/空 */
  overview: string | null;
  rating: string | null;
  content_rating?: string | null;
  runtime_minutes: string | null;
  genres: string[];
  brands?: LibraryItemBrand[];
  cast: LibraryItemCastMember[];
  /** 本地 fanart.jpg 背景图（/fanart/{ledgerId}） */
  backdrop_url: string | null;
  files: LibraryItemFile[];
  seasons: number[];
  season_summaries?: LibrarySeasonSummary[];
}

/** 剧集分集区的一集。 */
export interface LibraryEpisode {
  episode_number: number;
  name: string | null;
  overview: string | null;
  air_date: string | null;
  still_url: string | null;
  /** 该集有在位文件；false=缺集或文件缺失（置灰展示） */
  owned: boolean;
  /** 5 态可用性全景（in_library / downloading / file_missing / missing / unaired） */
  status?: EpisodeAvailabilityStatus;
  /** 首选在位文件的清晰度（如 2160p / 1080p） */
  resolution?: string | null;
  /** 该集的台账文件 id */
  file_ids: string[];
  /** 当前观看者上次看到的位置（毫秒）；0=没看过或已重置 */
  position_ms: number;
  /** 当前观看者已看完该集 */
  played: boolean;
  /** 观看进度 1~99；已看完由 played 表达，不给百分比 */
  progress_percent: number | null;
}

/** 一季的分集清单。 */
export interface SeasonEpisodes {
  season_number: number;
  episodes: LibraryEpisode[];
}

/** 海报墙 A-Z 索引条的一档。 */
export interface LibraryIndexEntry {
  /** 档名：按标题排序是首字母 A-Z；按内容时间排序是月份；按评分排序是评分档 */
  initial: string;
  count: number;
  /**
   * 直达这一档第一格的**不透明游标**（服务端签发，客户端不得解析/构造）。
   * 第一档为 `null`：它从第 0 条开始，用没有 cursor 的首页即可。
   *
   * 线上不带绝对下标（Task 26）：档位位置由 `count` 前缀和还原，
   * 见 `lib/library-wall-index.ts` 的 `wallBucketOffsets`。
   */
  cursor: string | null;
}

/** 筛选面板里的一个候选值（计数已排除本维自身的条件）。 */
export interface FacetValue {
  value: string;
  label: string;
  /** 在**其他维度**已选条件下勾上本值还剩几部；0 的照常返回，前端置灰不可点 */
  count: number;
}

/** 一次筛选下的全部候选值与计数。 */
export interface LibraryFacets {
  total: number;
  genres: FacetValue[];
  countries: FacetValue[];
  decades: FacetValue[];
  watch: FacetValue[];
  ratings: FacetValue[];
  runtimes: FacetValue[];
  languages: FacetValue[];
  resolutions: FacetValue[];
  hdr: FacetValue[];
  stock: FacetValue[];
}

/** 「更换图片」弹层里的一张候选图。 */
export interface ArtworkCandidate {
  file_path: string;
  preview_url: string;
  width: number | null;
  height: number | null;
  language: string | null;
  vote_average: number | null;
  vote_count: number | null;
}

/** 条目的候选图集合。 */
export interface ArtworkCandidates {
  posters: ArtworkCandidate[];
  backdrops: ArtworkCandidate[];
  /** 实际在用的图路径——标「当前」用它比对 */
  current_poster: string | null;
  current_backdrop: string | null;
  /** 已手动选定，刷新不会覆盖 */
  poster_locked: boolean;
  backdrop_locked: boolean;
}

/** 缺失清单里的一个文件。 */
export interface MissingFile {
  id: string;
  file_path: string;
  season_number: number;
  episode_number: number;
  size_bytes: number;
}

/** 缺失清单的一行：按媒体条目聚合。 */
export interface MissingItem {
  media_item_id: string;
  kind: LibraryKind;
  tmdb_id: string | null;
  title: string;
  year: number | null;
  poster_url: string | null;
  /** 该条目已有订阅时给出 */
  subscription_id: string | null;
  files: MissingFile[];
}

/** 收敛器判不了时留下的一个候选（点一下即可认领）。 */
export interface UnidentifiedCandidate {
  tmdb_id: number;
  title: string;
  year: number | null;
  episode_count: number | null;
  reasons: string[];
}

/** 识别失败的分类。 */
export type UnidentifiedCode =
  | "unparsable"
  | "tmdb_unreachable"
  | "ambiguous"
  | "no_match"
  | "kind_mismatch";

/** 待识别清单的一个文件。 */
export interface UnidentifiedFile {
  id: string;
  library_id: string;
  library_name: string;
  file_path: string;
  size_bytes: number;
  season_number: number;
  episode_number: number;
  reason: string | null;
  code: UnidentifiedCode | null;
  candidates: UnidentifiedCandidate[];
}

/** 待识别清单的一组：同一条目目录下的文件（一部剧几十集算一组）。 */
export interface UnidentifiedGroup {
  /** 分组键：条目目录绝对路径；裸文件用文件自身路径 */
  key: string;
  /** 展示名：条目目录名（裸文件为文件名） */
  label: string;
  library_id: string;
  library_name: string;
  file_count: number;
  total_size_bytes: number;
  reason: string | null;
  code: UnidentifiedCode | null;
  candidates: UnidentifiedCandidate[];
  files: UnidentifiedFile[];
}

/** 海报墙排序。 */
export type LibraryItemSort =
  | "title"
  | "added_at"
  | "release_date"
  | "probing"
  | "rating"
  | "runtime"
  | "size"
  | "last_played"
  | "random";

/** 排序方向。 */
export type LibraryItemOrder = "asc" | "desc";

/** 海报墙口径：confirmed=正式条目 / provisional=临时条目。 */
export type LibraryItemIdentity = "confirmed" | "provisional";

/** 图廊里的一张图。 */
export interface LibraryGalleryImage {
  kind: "poster" | "backdrop" | "still" | "chapter";
  url: string;
  aspect: number;
  label: string;
  season: number | null;
  episode: number | null;
  t_seconds: number | null;
}

/** 图廊按条目分的一组。 */
export interface LibraryGalleryGroup {
  media_item_id: string;
  library_id: string;
  kind: LibraryKind;
  title: string;
  year: number | null;
  is_favorite: boolean;
  images: LibraryGalleryImage[];
}

/** 媒体库搜索结果的一组。 */
export interface LibrarySearchGroup {
  library_id: string;
  library_name: string;
  kind: MediaType;
  items: LibraryItem[];
}

/** 筛空时的一条放宽建议。 */
export interface RelaxSuggestion {
  dim: string;
  dim_label: string;
  value: string;
  label: string;
  count: number;
}

/** 筛空之后的出路。 */
export interface LibraryRelax {
  total: number;
  suggestions: RelaxSuggestion[];
}

/** 一条音轨。 */
export interface AudioStream {
  codec: string | null;
  profile: string | null;
  channels: number | null;
  channel_layout: string | null;
  language: string | null;
  sample_rate?: string | null;
  title: string | null;
  default: boolean;
}

/** 主视频流信息（ffprobe 探测，file_meta 缓存；未探测为 null）。 */
export interface VideoTrackInfo {
  codec: string | null;
  width: number | null;
  height: number | null;
  frame_rate: number | null;
  bit_rate: number | null;
  duration_secs: number | null;
}

/** 一条字幕：内封轨或外挂文件。 */
export interface SubtitleStream {
  codec: string | null;
  language: string | null;
  title: string | null;
  forced: boolean;
  default: boolean;
  external: boolean;
  file_name: string | null;
}

/** 字幕预览中的一条时间轴对白。 */
export interface SubtitleCue {
  start_ms: number;
  end_ms: number;
  text: string;
}

/** 字幕标签点击后加载的结构化预览。 */
export interface SubtitlePreview {
  track: string;
  format: string | null;
  event_count: number;
  cues: SubtitleCue[];
}

/** 删除一个外挂字幕文件后的回执。 */
export interface SubtitleDeleteResult {
  path: string;
  freed_bytes: number;
}

/** 文件来源快照。 */
export interface FileOrigin {
  kind: "subscription" | "manual_download" | "watch_import" | "scan" | string;
  label: string;
  detail: string | null;
}

/** 一个章节。 */
export interface LibraryChapter {
  index: number;
  start_ms: number;
  end_ms: number | null;
  frame_ms: number | null;
  title: string | null;
  synthetic: boolean;
  image_url: string | null;
}

/** 「修正识别结果」预览里一组文件的识别结论。 */
export interface ReidentifyOutcome {
  media_item_id: string | null;
  tmdb_id: string | null;
  title: string | null;
  year: number | null;
  poster_url: string | null;
  source: string | null;
  same_as_current: boolean;
  reason: string | null;
  code: string | null;
  candidates: UnidentifiedCandidate[];
}

/** 预览的一组。 */
export interface ReidentifyGroup {
  key: string;
  outcome: ReidentifyOutcome;
  /** 预览时的文件版本；提交时原样回传，服务器不补当前版本 */
  files: ReidentifyFile[];
  file_count: number;
  total_size_bytes: number;
  sample_names: string[];
}

/** 复核/重新识别场景的条目信息快照。 */
export interface ReviewItemInfo {
  media_item_id: string;
  tmdb_id: string | null;
  title: string;
  year: number | null;
  poster_url: string | null;
}

/** 「修正识别结果」第一阶段的结论。 */
export interface ReidentifyPreview {
  current: ReviewItemInfo;
  movie: boolean;
  groups: ReidentifyGroup[];
  skipped_missing: number;
  pinned_identity: boolean;
  unreachable: boolean;
  search_seed: string;
  media_item_id: string;
  source_revision: number;
}

/** 预览时固定的一份文件版本。 */
export interface ReidentifyFile {
  file_id: string;
  revision: number;
  source_version: string;
}

/** 重新识别的唯一请求：文件与源条目都固定到预览版本。 */
export interface ReidentifyRequest {
  media_item_id: string;
  source_revision: number;
  title_ref: string;
  files: ReidentifyFile[];
}

/** 条目真实删除的结论。 */
export interface ItemDeleteResult {
  removed_paths: string[];
  rows_deleted: number;
  freed_bytes: number;
  errors: string[];
}

export interface DuplicateEntry {
  file_id: string;
  path: string;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string | null;
  size_bytes: number;
}

export interface DuplicateEntryGroup {
  media_item_id: string;
  title: string;
  season: number | null;
  episode: number | null;
  files: DuplicateEntry[];
}

/** 浏览列表的一页查询参数。 */
export interface BrowseQueryParams {
  sort?: LibraryItemSort;
  order?: LibraryItemOrder;
  /** 每页条数；省略 = 服务端默认上限 */
  limit?: number;
  /** 键集游标（上一页的 `next_cursor`）；首屏不传 */
  cursor?: string | null;
  identity?: LibraryItemIdentity;
  filter?: LibraryFilter;
  /** 未看优先：观看分级参与排序（只排不筛，口径见 library::latest） */
  unwatchedFirst?: boolean;
}

/** 一页浏览结果：`data` 是条目数组，游标与总数在信封上。 */
export interface LibraryItemPage {
  items: LibraryItem[];
  /** 下一页游标；null = 已经到末页 */
  next_cursor: string | null;
  /** 总条目数；只有第一页（没有 cursor 的那页）才有，续页为 null */
  total: number | null;
}

/** 一页图廊：`data` 是图组数组，游标在信封上（图廊不报总数）。 */
export interface LibraryGalleryPage {
  groups: LibraryGalleryGroup[];
  /** 下一页游标；null = 已经到末页 */
  next_cursor: string | null;
}
