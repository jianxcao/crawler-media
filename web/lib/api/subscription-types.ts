import type { MediaType } from "@/lib/media-types";

/** 订阅状态（见 self.md tracking_state；completed 是前端派生值） */
export type SubscriptionStatus = "active" | "paused" | "completed";

/** 工单状态机（见后端 WantedStatus） */
export type WantedStatus = "wanted" | "grabbed" | "downloaded" | "imported";

/** 订阅拉取模式（见 self.md subscribe.fetch_mode） */
export type FetchMode = "search" | "rss" | "both";

/** 订阅覆盖范围（见 self.md coverage）。 */
export type SubscriptionCoverage =
  | { kind: "movie" }
  | { kind: "tv"; season: number; episode_from: number; episode_to?: number | null };

/** 媒体条目视图（见 self.md MediaView；id 与外部 id 均为字符串直通）。 */
export interface MediaView {
  id: string;
  kind: MediaType;
  title: string;
  year?: number | null;
  original_title?: string | null;
  tmdb_id?: string | null;
  douban_id?: string | null;
  tvdb_id?: string | null;
  bangumi_id?: string | null;
  anilist_id?: string | null;
  poster_url?: string | null;
}

/** 条目摘要（新契约 MediaView 形状；media_item_id 为 UUID 字符串）。 */
export interface SubscriptionMedia {
  media_item_id: string;
  kind: MediaType;
  tmdb_id: string | null;
  douban_id: string | null;
  title: string;
  original_title: string | null;
  year: number | null;
  poster_url: string | null;
  status: string | null;
}

/** 弹层季选择器的一行 */
export interface SeasonOverview {
  season_number: number;
  name: string;
  air_date: string | null;
  episode_count: number | null;
  /** 已播集数（air_date<=今天） */
  aired_count: number;
  /** 媒体库已有的集数（库存 H） */
  owned_count: number;
}

/** 豆瓣收敛歧义时的确认候选 */
export interface ResolveCandidate {
  tmdb_id: string;
  /** 选定该候选后用于创建订阅的稳定引用 */
  title_ref: string;
  title: string;
  original_title: string;
  year: number | null;
  poster_url: string | null;
}

/** 订阅预检结果：ready 可直接渲染弹层 / ambiguous 先选候选 / not_found 无法订阅 */
export interface PrepareResult {
  status: "ready" | "ambiguous" | "not_found";
  media: SubscriptionMedia | null;
  seasons: SeasonOverview[];
  existing_subscription_id: string | null;
  /** 电影：媒体库里已有本片（弹层提示，不拦订阅） */
  movie_owned: boolean;
  /**
   * 建议预勾选的季号：豆瓣把剧集按季拆条目，点进「中餐厅 第十季」要订的就是那一季。
   * 服务端只在条目确实季专属时给出（且季号是 TMDB 的，与豆瓣季号未必相同）；
   * 为空表示无结论，按「全部已播正季」的原默认规则勾选。
   */
  suggested_seasons: number[];
  candidates: ResolveCandidate[];
}

export interface SubscriptionProgress {
  total: number;
  imported: number;
  missing: number;
  grabbing: number;
  downloaded: number;
}

/** 单元的洗版派生状态（见后端 WantedUpgradeView）；标签由后端生成。 */
export interface WantedUpgrade {
  /** 是否洗版中（可证明低于目标且未熔断） */
  active: boolean;
  /** 当前版本档位标签（如「1080p WEB-DL」） */
  current_label: string;
  /** 洗版目标档位标签（如「1080p Remux」） */
  target_label: string;
  /** 已洗版搜索次数 */
  search_attempts: number;
  /** 无法确认档位（证明不了低于目标也证明不了达标）：不参与自动洗版，可手动选种替换 */
  indeterminate: boolean;
}

/** 订阅（见 self.md Subscription）。 */
export interface Subscription {
  id: string;
  media: MediaView;
  user_id: string;
  coverage: SubscriptionCoverage;
  fetch_mode: FetchMode;
  filter_id: string | null;
  wash_cut: boolean;
  wash_cut_filter_id?: string | null;
  /** 洗版时保留被替换的旧版本（同时留 1080p 与 4K）。 */
  keep_old_versions: boolean;
  full_season_pack: boolean;
  downloader_id?: string | null;
  library_id?: string | null;
  /** Explicit target availability; never silently resets library_id. */
  target_library_status: "ready" | "deleting" | "missing";
  target_library_error: string | null;
  /** completed：后端 M4a 判定已全部入库（非洗版有界订阅），搜索跳过索引站。 */
  tracking_state: "active" | "paused" | "completed";
  follow_future: boolean;
  /** Seconds between automatic Site searches. Default 1800. */
  search_interval_secs: number;
  progress: SubscriptionProgress;
  created_at: string;
  updated_at: string;
}

/** 订阅首页的一行待入库摘要（今天，或今天没有安排时窗口内最近的一天）。 */
export interface TodaySubscriptionArrival {
  subscription_id: string;
  wanted_id: string;
  media_title: string;
  media_kind: "movie" | "tv";
  season_number: number;
  episode_number: number;
  /** imported = 当日已入库；grabbed = 在途下载中 */
  status: Extract<WantedStatus, "wanted" | "grabbed" | "downloaded"> | "imported";
  air_date: string | null;
  /** 预计入库/播出的站点日历日（YYYY-MM-DD），用于展示日期。 */
  expected_day: string;
  /** expected_day 距今天几天（0=今天）。站点日历口径由后端算好，前端不复制时区规则。 */
  days_ahead: number;
  release_forecast: ReleaseForecast | null;
  /** 后端按站点游标和礼貌间隔计算的下一次有效预测探测时间。 */
  next_probe_at: string | null;
  info_hash: string | null;
  grabbed_at: string | null;
  downloaded_at: string | null;
  estimated_release_to_import_minutes: number;
  estimated_download_to_import_minutes: number;
}

export interface ReleaseForecastSite {
  site_id: string;
  predicted_at: string;
  window_start: string;
  window_end: string;
  lag_minutes: number;
  coverage_count: number;
  probe_times: string[];
}

/** 单集资源发布时间预测快照；它是可重算的调度结论，不是新的观测事实源。 */
export interface ReleaseForecast {
  version: number;
  generated_at: string;
  target_air_date: string;
  predicted_at: string;
  window_start: string;
  window_end: string;
  confidence: "bootstrap" | "growing" | "stable" | "volatile";
  sample_count: number;
  cadence_days: number;
  basis_units: [number, number][];
  basis_torrent_row_ids: number[];
  sites: ReleaseForecastSite[];
  first: {
    generated_at: string;
    predicted_at: string;
    window_start: string;
    window_end: string;
    confidence: string;
    sample_count: number;
  };
}

/** 一集最近一次成功投递所用资源的发布→索引→提交时间链。 */
export interface ResourceTiming {
  site_id: string;
  torrent_id: string;
  publish_time: string | null;
  first_seen_at: string | null;
  submitted_at: string;
  publish_to_seen_seconds: number | null;
  seen_to_submit_seconds: number | null;
  publish_to_submit_seconds: number | null;
  dry_run: boolean;
}

export interface WantedItem {
  id: string;
  season_number: number;
  episode_number: number;
  status: WantedStatus;
  air_date: string | null;
  priority: number;
  next_search_at: string | null;
  search_attempts: number;
  last_search_at: string | null;
  release_forecast: ReleaseForecast | null;
  resource_timing: ResourceTiming | null;
  grabbed_at: string | null;
  downloaded_at: string | null;
  imported_at: string | null;
  /** 在途工单锚定的种子 hash；据此与 listActiveSubscriptionDownloads 的进度组对上 */
  info_hash: string | null;
  /** 单集履历注解：最近一次候选被拒原因（含站点与种子名）；null=从未被拒 */
  last_reject_reason: string | null;
  /** 单集履历注解：最近一次投递的种子（站点 · 标题）；null=未投递 */
  grab_title: string | null;
  /** 洗版派生状态；规则组未配洗版目标或单元未入库时为 null */
  upgrade: WantedUpgrade | null;
}

/** 订阅详情 = 订阅 + 工单明细（GET /subscriptions/{id} 现算的 wanted）。 */
export interface SubscriptionDetail extends Subscription {
  wanted: WantedItem[];
}

/** 规则组原子（见 self.md RuleSet.atoms；exclude 黑名单原子命中即排除）。 */
interface RuleSetAtomFields {
  kind:
    | "resolution"
    | "source"
    | "video_codec"
    | "free"
    | "platform"
    | "team"
    | "title_match"
    | "hdr"
    | "size"
    | "min_seeders"
    | "subtitle_language"
    | "audio_language"
    | "site"
    | "wash_target"
    | "upgrade_ladder";
  value: string | null;
  priority: number;
  exclude?: boolean;
}

export type RuleSetAtom =
  | RuleSetAtomFields
  | {
      kind: "hr";
      value: null;
      priority: number;
      exclude?: boolean;
      unknown: "exclude" | "allow";
    };

/** 规则组（见 self.md RuleSet；旧 spec 对象已折叠为 atoms 原子列表）。 */
export interface RuleSet {
  id: string;
  name: string;
  is_default: boolean;
  keep_old_versions?: boolean;
  atoms: RuleSetAtom[];
}

export interface SubscriptionTargetPreviewPayload {
  /** Discover 返回的影视条目稳定引用 */
  title_ref: string;
}

export interface CreateSubscriptionPayload {
  /** Discover 返回或歧义候选确认后的稳定引用 */
  title_ref: string;
  /** 从豆瓣候选改选 TMDB 条目时保留原始豆瓣身份 */
  source_title_ref?: string | null;
  /** 剧集订阅范围；电影省略。一季一条订阅。 */
  coverage?: { kind: "tv"; season: number; episode_from: number };
  follow_future?: boolean;
  filter_id?: string | null;
  /** 入库目标库；缺省用该类型的默认库 */
  library_id?: string | null;
  wash_cut?: boolean;
  wash_cut_filter_id?: string | null;
  keep_old_versions?: boolean;
}

export interface CreateSubscriptionResult {
  subscription: SubscriptionDetail;
  /** 管理员可见的下载与入库路由预检；成员调用时为空 */
  download_routing: DispatchPreview | null;
}

/** 投递路由预检（见 schemas.subscription.DispatchPreviewView）。 */
export interface DispatchPreview {
  /** watch=投监听导入目录 / inplace=直接下载进库 / downloader_default=下载器默认目录 */
  mode: "watch" | "inplace" | "downloader_default";
  /** 本机视角的投递基底目录 */
  path: string | null;
  /**
   * 条目目录的完整路径预览（后端按生效的命名模板渲染）。
   * 展示"直接下载到 …"时用它，**不要**自己拼 `${title} (${year})`：
   * 命名模板可全局/按库自定义，自己拼会与真实落点不一致。
   */
  entry_dir: string | null;
  /** 命中自定义目录规则时的整理落点：完成后整理到该目录（不直接入库），外部流转回库根后入账 */
  staging_path: string | null;
  /** 解析出的目标库（未选库时=收藏范围路由的结论，弹窗据此预选） */
  library_id: string | null;
  library_name: string | null;
  downloader_name: string | null;
  /** 收藏范围路由结论：true=命中声明库 / false=默认库兜底；未走路由为 null */
  route_matched: boolean | null;
  /** 路由理由（中文整句，徽标直接展示） */
  route_reason: string | null;
  /** 被收藏范围路由命中的库所配置的默认规则组；未配置或未走路由时为空。 */
  default_filter_id?: string | null;
  /** 按当前配置投递能否顺利入库 */
  ok: boolean;
  /** 不 ok 时的中文指引 */
  warning: string | null;
}

/** 订阅链路体检里的一段检查结论。 */
export interface PipelineCheck {
  key: string;
  /** 段落名（「下载器」「路径映射」「硬链接同盘」…） */
  label: string;
  /** ok=正常 / warn=能转但降级 / error=会失败，必须修 */
  status: "ok" | "warn" | "error";
  /** 中文事实陈述，直接展示 */
  detail: string;
  /** 修复去处：设置分区 id（downloaders/import-watch）或 libraries（媒体库页） */
  fix_section: string | null;
}

/** 一个库的完整入库链路结论。 */
export interface LibraryPipeline {
  library_id: string;
  name: string;
  kind: "movie" | "tv";
  is_default: boolean;
  /** watch=投监听导入目录 / inplace=直下库根 / downloader_default=下载器默认目录 */
  mode: "watch" | "inplace" | "downloader_default";
  path: string | null;
  /** 库主根（入库节点的落点展示） */
  library_root: string | null;
  /** 命中自定义目录规则时的整理落点：非空时转移段不直接入库，外部流转后回库根入账 */
  staging_path: string | null;
  status: "ok" | "warn" | "error";
  /** 「订阅命中本库会发生什么」的一句话叙事（全绿时的正向可预期） */
  narrative: string;
  checks: PipelineCheck[];
}

/** 修复卡里的一个可选修法。 */
export interface FixOption {
  title: string;
  /** 为什么这么做 / 适合谁（帮用户在多个选项间取舍；可为空串） */
  why: string;
  /** 具体做什么，含建议值 */
  steps: string;
  /** 修复去处（与 PipelineCheck.fix_section 同词表，前端映射到路由） */
  fix_section: string;
  fix_label: string;
  /** 跳转预填参数：目标设置页读取后自动填表单，免去誊写路径 */
  fix_params: Record<string, string> | null;
}

/** 按根因聚合的问题卡：一个根因 = 一张卡，不随受影响的库数膨胀。 */
export interface PipelineIssue {
  /** 与被聚合检查项的 PipelineCheck.key 同词表（库卡片据此隐藏重复红项） */
  key: string;
  status: "warn" | "error";
  title: string;
  detail: string;
  affected_libraries: string[];
  options: FixOption[];
}

/** 订阅链路体检整体结论（订阅设定页与订阅列表警示横幅共用）。 */
export interface PipelineHealth {
  /** 整体状态：库链路 + 全局段（站点/下载器）的最坏值 */
  status: "ok" | "warn" | "error";
  /** 链路有 error 的总项数（含站点/下载器全局段） */
  error_count: number;
  /** 自身入库链路存在 error 的媒体库数 */
  library_error_count?: number;
  warn_count: number;
  /** 资源搜索段（全局，链路第一环） */
  site_check: PipelineCheck;
  /** 是否有可用的默认下载器 */
  downloader_ok: boolean;
  /** 是否配置过站点（无论当前可用与否）——开局清单只看它 */
  sites_configured: boolean;
  /** 是否配置过下载器（同上语义） */
  downloaders_configured: boolean;
  /** 按根因聚合的问题卡（error 在前）——置顶展示为「需要处理 N 件事」 */
  issues: PipelineIssue[];
  libraries: LibraryPipeline[];
}

/** 一轮洗版体检里单个单元的结论（见 schemas.subscription.UpgradeRunUnitView）。 */
export interface UpgradeRunUnit {
  season_number: number;
  episode_number: number;
  /** upgradable=已排入立即搜索 / at_cutoff=已达目标 / in_flight=已在洗 /
   *  not_comparable=无法识别当前版本 / missing=缺失（照常走缺口下载） */
  state: "upgradable" | "at_cutoff" | "in_flight" | "not_comparable" | "missing";
  current_label: string | null;
  target_label: string;
}

/** 一轮洗版的体检报告（见 schemas.subscription.UpgradeRunView）。 */
export interface UpgradeRunReport {
  target_label: string;
  filter_id: string | null;
  /** 中文整句摘要，直接展示 */
  summary: string;
  counts: Record<UpgradeRunUnit["state"], number>;
  units: UpgradeRunUnit[];
}

/** 手动选种的种子字段（搜索结果行原样回传，attrs 即搜索链路的服务端解析）。 */
export interface GrabPayload {
  site_id: string;
  torrent_id: string;
  title: string;
  subtitle?: string;
  category?: string | null;
  attrs?: Record<string, unknown> | null;
  download_url?: string | null;
  size_bytes?: number | null;
  seeders?: number | null;
  is_free?: boolean | null;
  hit_and_run?: boolean | null;
  publish_time?: string | null;
}

/** 整个 Media 的文件数量，以及该订阅的待下载或推断 Torrent 数量。 */
export interface SubscriptionRemovalPreview {
  library_file_count: number;
  library_bytes: number;
  torrent_count: number;
  torrent_count_inferred: boolean;
  torrent_titles: string[];
}

/** 联动清理选项；两项都不勾 = 什么都不删（默认）。 */
export interface SubscriptionRemovalOptions {
  deleteTorrents: boolean;
  deleteLibraryFiles: boolean;
}

/** 订阅在途种子的实时下载快照（详情页轮询展示进度/速度/ETA）。 */
export interface SubscriptionDownload {
  info_hash: string;
  /** 下载器中的任务名；missing 时为空 */
  name: string | null;
  /** 0.0 ~ 1.0；missing 时为空 */
  progress: number | null;
  size_bytes: number | null;
  dlspeed_bytes: number | null;
  /** missing = 种子已不在任何可用下载器中（救援巡检稍后会退回工单重找） */
  state: "downloading" | "stalled" | "paused" | "completed" | "error" | "missing" | "unknown";
  downloader_name: string | null;
}

/** 订阅活动记录：message 是完整中文句子，时间线直接展示。 */
export interface SubscriptionActivity {
  id: string;
  type:
    | "created"
    | "adjusted"
    | "paused"
    | "resumed"
    | "completed"
    | "reopened"
    | "searched"
    | "match_accepted"
    | "match_rejected"
    | "grabbed"
    | "dispatch_failed"
    | "wanted_added"
    | "downloaded"
    | "imported"
    | "import_failed"
    | "download_stalled"
    | "replacement_searched"
    | "replacement_trial"
    | "replacement_promoted"
    | "replacement_cleanup"
    | "upgrade_grabbed"
    | "upgraded"
    | "upgrade_verify_failed"
    | "spec_mismatch";
  message: string;
  payload: Record<string, unknown>;
  created_at: string;
  /** 管线类活动所属工单；生命周期变更、搜索轮次等订阅级活动为 null */
  wanted_item_id: string | null;
}

export interface ReleaseForecastDay {
  date: string;
  weekday: number;
  count: number;
}
