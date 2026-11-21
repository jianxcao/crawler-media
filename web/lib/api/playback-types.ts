import type {
  LibraryGalleryGroup,
  LibraryItem,
  LibraryItemOrder,
} from "@/lib/api/libraries";
import type { LibraryKind, MediaType } from "@/lib/media-types";

/**
 * 播放接口的作用域。
 *
 * 播放器对后端地址原本写死 `/playback/…`；默认值等于今天登录态的行为，
 * 既有调用点零改动。
 */
export interface PlaybackApiScope {
  /** 接口前缀 */
  base: string;
  /** 进度落服务端成员表 */
  progress: "server" | "local";
  /** metrics / client-log 是否上报 */
  telemetry: boolean;
}

export interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

export interface UpNextItem {
  media_item_id: string;
  library_id: string;
  kind: MediaType;
  title: string;
  year: number | null;
  poster_url: string | null;
  /** 海报真实宽高比（同海报墙 primary_aspect）；其他库的本地封面多是 16:9。 */
  poster_aspect: number;
  /** 电影横向背景剧照；缺失时前端按 poster_aspect 用海报铺满或模糊铺底。 */
  backdrop_url: string | null;
  /** **卡片这一集**的 16:9 剧照；电影恒为 null。 */
  episode_still_url: string | null;
  /** 卡片指向的季集——可能是最近播放那一集，也可能是它之后的下一集 */
  season_number: number;
  episode_number: number;
  episode_title: string | null;
  /** 卡片这一集**之后**还有几个没看完、且文件在位的单元；电影恒为 0。 */
  unwatched_ahead_count: number;
  /** 续播点；0 = 这一集还没开过（多半是下一集） */
  position_ms: number;
  duration_ms: number | null;
  progress_percent: number | null;
  /**
   * 卡片已经**翻过篇**了：上次那一集看完了，这张卡指向它之后的下一集。
   * 前端推不出来——`position_ms === 0` 同时意味着"下一集"和"这一集还没开过"。
   */
  advanced: boolean;
  /** 最近一次播放这部作品的时间——排序依据与"什么时候看的"那行文案 */
  last_played_at: string;
}

/**
 * 首页「我的收藏」的一格：单库海报墙的条目视图 + 收藏上下文。
 * 收藏层级来自最近一次收藏：整剧两者皆 null，整季只有季号，单集季集都有；
 * 电影恒为 null。
 */
export interface FavoriteItem extends LibraryItem {
  /** 卡片的详情落点库（同一作品跨库时取首页顺序第一个可见库） */
  library_id: string;
  favorite_season_number: number | null;
  favorite_episode_number: number | null;
}

export interface FavoritesPage {
  items: FavoriteItem[];
  /** Exact on the head page; continuations do not count again. */
  total: number | null;
  next_cursor: string | null;
}

export type FavoriteSort = "favorited_at" | "title" | "year";

export interface FavoriteSortParams {
  sort?: FavoriteSort;
  order?: LibraryItemOrder;
}

export interface FavoritePageParams extends FavoriteSortParams {
  limit?: number;
  after?: string;
  unwatched_first?: boolean;
}

export interface FavoritesGalleryPage {
  items: LibraryGalleryGroup[];
  total: number | null;
  next_cursor: string | null;
}

export interface MediaActivityTarget {
  media_item_id: string;
  /** 详情页落点；作品没有在位文件时为 null（此时不渲染跳转）。 */
  library_id: string | null;
  /** 落点库是否在当前超管的可浏览范围内。「全部」口径下范围外记录照常出片名，
   *  但浏览类接口对范围外超管是 404，前端不渲染详情链接。 */
  browsable: boolean;
  /** movie / tv / video（「其他」库的单本视频） */
  kind: LibraryKind;
  title: string;
  year: number | null;
  poster_url: string | null;
  season_number: number;
  episode_number: number;
  episode_title: string | null;
}

export interface PlaybackFileSpec {
  resolution: string | null;
  video_codec: string | null;
  hdr: string | null;
  container: string | null;
  bit_rate: number | null;
  size_bytes: number | null;
}

export interface ActivePlaybackSession {
  user_id: string;
  device_id: string;
  /** 能否「注销此设备」：只有持 Jellyfin 设备凭据的会话可以；网页播放器不行。 */
  revocable: boolean;
  member_name: string;
  client: string;
  device_name: string;
  client_version: string;
  media: MediaActivityTarget;
  position_ms: number | null;
  duration_ms: number | null;
  progress_percent: number | null;
  paused: boolean;
  /** local = 本地文件直连（速率可测）；remote = 网盘直链等不经过服务器的播放。 */
  play_method: "local" | "remote";
  rate_bytes_per_second: number | null;
  bytes_sent: number | null;
  connections: number;
  file: PlaybackFileSpec | null;
  started_at: string;
  last_report_at: string;
}

export interface ActiveFileDownload {
  user_id: string;
  device_id: string;
  revocable: boolean;
  member_name: string;
  client: string;
  device_name: string;
  media: MediaActivityTarget | null;
  file_name: string;
  size_bytes: number;
  bytes_sent: number;
  rate_bytes_per_second: number;
  /** 同一设备同一文件的多条 Range 连接（断点续传）聚合后的连接数。 */
  connections: number;
  /** 已下载到文件的哪个字节位置（Range 起点 + 本次已传）。 */
  position_bytes: number;
  /** 由位置换算的完成百分比；文件大小未知时为 null。 */
  progress_percent: number | null;
  started_at: string;
}

/** 活动页「观看」视角的实时快照：正在播放与正在下载。历史走 fetchPlaybackHistory。 */
export interface MediaActivitySnapshot {
  sessions: ActivePlaybackSession[];
  downloads: ActiveFileDownload[];
  /** 「我的浏览范围」口径下落在超管不可浏览的库里的记录数（只报个数，不出片名）；
   *  「全部」口径恒为 0。 */
  hidden_session_count: number;
  hidden_download_count: number;
}

/**
 * 活动页的可见范围口径。`visible` = 按我的浏览范围折叠范围外记录（默认，尊重
 * 超管给自己设的隐藏意图）；`all` = 管控视角的全量口径，跨成员跨库都看。
 */
export type MediaActivityScope = "visible" | "all";

export interface PlaybackHistoryClearResult {
  deleted_states: number;
  deleted_metrics: number;
}

export interface PlaybackLogEntry {
  id: number;
  member_name: string;
  media: MediaActivityTarget;
  client: string;
  device_name: string;
  started_at: string;
  /** null = 仍在进行中 */
  ended_at: string | null;
  /** 实际观看时长（毫秒），暂停与 seek 跳过的区间不计 */
  watched_ms: number;
  start_position_ms: number;
  end_position_ms: number;
  /** 这一场结束时看到哪；条目已删时为 null */
  duration_ms: number | null;
  progress_percent: number | null;
  completed: boolean;
}

export interface PlaybackStatsMemberRow {
  member_id: string;
  member_name: string;
  plays: number;
  watched_ms: number;
  completed: number;
}

export interface PlaybackStatsClientRow {
  client: string;
  plays: number;
  watched_ms: number;
}

export interface PlaybackStatsDayRow {
  /** 按浏览器时区的日期 YYYY-MM-DD */
  date: string;
  plays: number;
  watched_ms: number;
  completed: number;
  /** 当天有播放的成员数 */
  members: number;
}

export interface PlaybackStatsTitleRow {
  media: MediaActivityTarget;
  plays: number;
  watched_ms: number;
  /** 看过这部作品的成员数 */
  members: number;
}

export interface PlaybackStatsTotals {
  plays: number;
  watched_ms: number;
  completed: number;
  active_members: number;
}

export interface PlaybackStatsTierRow {
  tier: number;
  label: string;
  plays: number;
}

/**
 * 观看统计：当前周期与上一周期成对返回（没有参照系的数字只是数据，不是洞察）。
 * by_day 与 previous_by_day 按天对齐，主图把两条线画在同一坐标系里；by_hour 是
 * 星期 × 小时的观看时长矩阵（0 行 = 周一），按浏览器时区分桶。
 */
export interface PlaybackWatchStats {
  days: number;
  current: PlaybackStatsTotals;
  previous: PlaybackStatsTotals;
  /** 上一周期有没有日志；日志刚开始记时没有，卡片上不该显示 0% */
  previous_available: boolean;
  by_day: PlaybackStatsDayRow[];
  previous_by_day: PlaybackStatsDayRow[];
  by_hour: number[][];
  by_member: PlaybackStatsMemberRow[];
  by_client: PlaybackStatsClientRow[];
  /** 网页播放按档位；Jellyfin 客户端恒为直连，不在内 */
  by_tier: PlaybackStatsTierRow[];
  top_titles: PlaybackStatsTitleRow[];
  hidden_title_count: number;
  /** 本期最受欢迎前三：看过的成员最多，并列取时长长的；与作品榜（按时长）口径不同 */
  favorites: PlaybackStatsTitleRow[];
  /** 上一周期的前三，用来标「蝉联 / 上期第 n / 新上榜」 */
  previous_favorites: PlaybackStatsTitleRow[];
}

/**
 * 客户端解码能力快照。**字段名与后端 ClientCapabilityIn 严格对应**，
 * codec 用的是归一化的编码家族名（h264 / hevc / aac …），不是浏览器探测时
 * 用的 RFC 6381 串（avc1.640028）——服务端要拿它和 ffprobe 落库的 codec_name
 * 直接比对。翻译在 lib/player/capability.ts 里做。
 */
export type MseKind = "full" | "managed" | "none";

export interface ClientCapability {
  video: { codec: string; max_height: number; smooth: boolean; power_efficient: boolean }[];
  audio: { codec: string; max_channels: number }[];
  /** "mp4" = 能 <video src> 直出；"hls-fmp4" = 有 MSE 或原生 HLS */
  containers: string[];
  hdr_passthrough: boolean;
  /** "full" | "managed"（iOS 的 ManagedMediaSource）| "none" */
  mse: MseKind;
  is_mobile: boolean;
  /** 能原生播 HLS（Safari/AVPlayer）。服务端不消费，前端选引擎用 */
  native_hls: boolean;
}

/** 播放单元：电影通过缺省季集表达规范 whole-work 请求；可选显式版本 file_id。 */
export interface PlaybackUnit {
  media_item_id: string;
  season_number?: number;
  episode_number?: number;
  file_id?: string;
}

export interface VideoPlan {
  /** "copy" | "transcode" */
  action: string;
  codec: string | null;
  height: number | null;
  tone_map: boolean;
  /** 非空 = 该字幕轨被烧录进画面（用户显式选中 PGS 触发，Emby 语义的
   * 「字幕压制」）。前端据此：不再旁挂渲染这条轨、菜单选中态指向它、
   * 画中画补丁轨跳过、诊断面板显示「字幕压制」。 */
  burn_subtitle: string | null;
  /** 按实测带宽收紧后的码率上限（bps）；null = 只按分辨率阶梯 */
  bitrate_cap_bps?: number | null;
}

export interface AudioPlan {
  action: string;
  track_ref: string | null;
  codec: string | null;
  channels: number | null;
  downmix: boolean;
}

/** 文件里的一条可选音轨。`AudioPlan` 说的是「这次放哪条」，这个是候选列表。 */
export interface AudioTrack {
  /** 中性轨引用：embedded:<下标> */
  ref: string;
  codec: string | null;
  channels: number | null;
  language: string | null;
  is_default: boolean;
}

export interface SubtitlePlan {
  /** 中性轨引用：external:<文件名> / embedded:<下标> */
  track_ref: string;
  /** "vtt"（服务端转好的文本轨）| "ass"（原样下发交 JASSUB）| "pgs"（P0 不渲染） */
  kind: string;
  language: string | null;
  is_default: boolean;
  /** 本机 AI 生成的字幕（翻译 / 双语），字幕菜单据此打标 */
  is_ai?: boolean;
}

/**
 * 决策结果的三态并集。`outcome` 决定其余字段哪些有值——分支处理别漏，
 * consent 与 rejected 都带面向用户的中文 `reason`。
 */
export interface PlaybackDecision {
  outcome: "plan" | "consent" | "rejected";
  tier: number | null;
  file_id: string | null;
  container: string | null;
  video: VideoPlan | null;
  audio: AudioPlan | null;
  /** 这个文件里全部可选音轨（含当前这条），前端据此渲染音轨菜单 */
  audio_tracks: AudioTrack[];
  subtitles: SubtitlePlan[];
  /** 本次是降档重来的结果，记录原本失败的档位（§6.3） */
  degraded_from: number | null;
  cost_hint: string | null;
  /** 当前账号能不能自己去开软件转码开关（只有超管可以） */
  can_self_enable: boolean | null;
  setting_namespace: string | null;
  setting_key: string | null;
  reason: string;
  suggestion: string | null;
}

/** 源文件的客观规格（台账真值），诊断面板「源 → 处理」层次的左半边 */
export interface PlaybackSource {
  container: string | null;
  resolution: string | null;
  video_codec: string | null;
  hdr: string | null;
  /** 总码率（bps） */
  bit_rate: number | null;
  frame_rate: number | null;
  size_bytes: number | null;
}

/** 进度条上的章节刻度。合成章节不下发 */
export interface PlaybackChapterMark {
  start_ms: number;
  title: string | null;
}

export interface PlaybackSession {
  decision: PlaybackDecision;
  /** 档 0 没有会话（原文件直出），此处为 null */
  session_id: string | null;
  play_session_id: string;
  /** 可直接喂给 <video src> 或 hls.js 的地址，已带签名 token */
  stream_url: string | null;
  /** 会话时间轴的零点在文件里的位置。**文件时间 = start_ms + currentTime**。 */
  start_ms: number;
  /** 续播状态：compatible | source_changed | no_progress | already_played */
  resume_status?: "compatible" | "source_changed" | "no_progress" | "already_played";
  /** master 播放列表（带 WEBVTT 字幕组），仅 VOD 会话有——iOS 原生 HLS 用 */
  master_url?: string | null;
  /** 旁挂字幕地址（已带 token），与 decision.subtitles 一一对应 */
  subtitle_urls: string[];
  /** 实际使用的硬件加速后端；null = 纯软件（直通档不经编码器，同样为 null） */
  hw_backend: string | null;
  /** 进度条上的章节刻度；冷缓存时暂为空，解析完成后可从章节缓存接口读取。 */
  chapters?: PlaybackChapterMark[];
  /** 冷缓存时章节在后台解析；播放器可按播放会话读取缓存。 */
  chapters_pending?: boolean;
  file_id?: string;
  /** 本单元的观看状态快照（§6.10）。续播点已由服务端并入 start_ms，这里
   * 整份带回给前端预填时间轴、恢复字幕记忆——起播不再单独问 /resume */
  watch: PlaybackWatchState | null;
  /** 选中文件的源规格（台账真值），诊断面板「源 → 处理」层次用（§6.5） */
  source: PlaybackSource | null;
}

export interface DecideBody extends PlaybackUnit {
  /** BFCache 自动恢复时，必须校验的原成员播放会话。 */
  resume_from_play_session_id?: string;
  file_id?: string;
  capability: ClientCapability;
  /** 运行期降档回路：带上已失败的档位，服务端跳过它们（§6.3） */
  failed_tiers?: number[];
  start_ms?: number;
  /**
   * 用户点选的音轨。给了服务端就认它、不再自动换轨。
   *
   * **换轨要换会话**：音轨在开会话时就被 ffmpeg `-map` 进命令里了，改不了；
   * 而且选了非默认轨会把档 0 顶成档 1（直出时浏览器只放默认轨）。所以前端
   * 换轨走的是和 seek 出界一样的「带着当前位置重开会话」那条路。
   */
  audio_track?: string;
  /** 用户选中的字幕轨。只在指向内封 PGS 轨时改变决策——视频转码并把字幕
   * 烧录进画面（Emby 语义）。缺省 = 服务端用观看状态里记住的轨；
   * "off" 显式不烧。文本轨传了也无妨（服务端 no-op），顺带刷新轨记忆。 */
  subtitle_track?: string;
  /** 画质上限（如 720）。上限而非目标：源不超就照常直通。省略 = 自动 */
  max_height?: number;
  /**
   * 实测下行速度（bps，bandwidth.ts 的传输期口径）。服务端只对转码视频
   * 用它压码率、必要时降高度；手动选了画质上限时服务端忽略。样本不够时省略。
   */
  downlink_bps?: number;
}

export interface PlaybackWatchState {
  position_ms: number;
  played: boolean;
  play_count: number;
  /** 服务端算出的片长；已看判定的分母始终以它为准 */
  duration_ms: number | null;
  audio_track: string | null;
  subtitle_track: string | null;
  /** 管理员已在活动页结束了这台浏览器的播放：播放器收到后退出，不再重开 */
  ended_by_admin?: boolean;
  /** 源感知续播（登录态）：与开播同一解析；本地分享进度没有这些字段 */
  file_id?: string;
  start_ms?: number;
  resume_status?: "compatible" | "source_changed" | "no_progress" | "already_played";
}

/** 播放页要的条目信息（§6.10）：路由只带 media_item_id，库归属服务端解析。 */
export interface PlaybackItemInfo {
  media_item_id: string;
  /** 该条目对当前成员可见的库 id；退出播放跳回条目页用 */
  library_id: string;
  kind: "movie" | "tv";
  title: string;
  year: number | null;
  poster_url: string | null;
}

/** 续播查询的入参：一个播放单元坐标。 */
export type ResumeParams = PlaybackUnit;

/**
 * 已看 / 收藏标记的目标。不带季集 = 整个条目（电影，或整剧级联到全部集）；
 * 只带季 = 整季；季集都带 = 单集——与 Jellyfin 客户端的 Series / Season /
 * Episode 三级一一对应，后端落的是同一张表、同一套哨兵约定。
 */
export interface PlaybackMarkTarget {
  media_item_id: string;
  season_number?: number;
  episode_number?: number;
}

export interface PlaybackMarks {
  played: boolean;
  is_favorite: boolean;
  /** 整剧 / 整季尚未看完的集数；电影与单集为 null */
  unplayed_count: number | null;
}

export interface PlaybackProgressBody extends PlaybackUnit {
  event: "start" | "progress" | "stop";
  /** 播到文件的哪个位置。**不报（undefined）视同播到结尾**，与报 0 不同。 */
  position_ms?: number;
  audio_track?: string;
  subtitle_track?: string;
  /** 暂停态；不报 = 实时会话保持原值 */
  paused?: boolean;
  play_session_id: string;
  event_sequence: number;
}

/** 播放策略。数字上限（并发/高度/缓存配额）由服务端按机器规格自动推导，
    不再是配置项；这里只剩两个意愿开关：软件转码（由播放页同意弹窗翻开）和
    进度条预览生成（设置页「播放」分区）。 */
export interface PlaybackPolicy {
  software_transcode_enabled: boolean;
  trickplay_enabled: boolean;
  /** 转码产物保留供续播、重看复用（§B）；关闭即会话结束即删 */
  transcode_cache_enabled: boolean;
  /** 实测结果而非配置项——用户改不了自己有没有显卡 */
  hardware_available: boolean;
  hw_backends: string[];
}

export interface PlaybackMetricPayload {
  /** Canonical Media identity; a UUID string, not a numeric UI index. */
  media_item_id: string;
  /** Visible Library ledger identity, including UUID-backed direct play; a UUID string, not a numeric UI index. */
  library_file_id: string | null;
  tier: number;
  degraded_from: number | null;
  engine: string;
  hw_backend: string;
  ttff_ms: number | null;
  rebuffer_ms: number;
  rebuffer_count: number;
  seek_count: number;
  dropped_frames: number | null;
  total_frames: number | null;
  watched_ms: number;
}
