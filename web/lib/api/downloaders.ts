import { request } from "@/lib/http";

/** 后端统一响应信封 */
interface ApiEnvelope<T> {
  success: boolean;
  code: string;
  message: string;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/** 已适配的下载器类型（与后端 domain DownloaderId/kind 对应）。 */
export type DownloaderClientType = "qbittorrent" | "transmission";

/** pending/unverified 表示尚未测试；verifying 仅用于本地手动测试。enabled 独立于验证结果。 */
export type DownloaderStatus = "pending" | "verifying" | "active" | "failed" | "disabled" | "unverified";

/** 一条路径映射（见 self.md Downloader.path_maps）：下载器视角前缀 → 宿主机视角前缀。 */
export interface PathMap {
  from: string;
  to: string;
}

/** 已配置下载器的对外视图（见 self.md Downloader，凭证不回显）。 */
export interface Downloader {
  id: string;
  name: string;
  kind: DownloaderClientType;
  url: string;
  username?: string | null;
  password?: string | null;
  category?: string | null;
  path_maps: PathMap[];
  is_default: boolean;
  /** 停用的下载器不参与自动投递（手动提交仍可用）。 */
  enabled?: boolean;
  status: DownloaderStatus;
  last_error?: string | null;
}

/** 兼容导出名：组件 import type { ConfiguredDownloader }。 */
export type ConfiguredDownloader = Downloader;

/**
 * 一条路径映射（表单编辑用）：本机视角的目录前缀 → 下载器视角的对应前缀。
 * 提交时由 API 层反向转成 PathMap（{local, remote} → {from: remote, to: local}）。
 */
export interface PathMapping {
  /** 本机上的路径前缀（目录弹窗选择） */
  local: string;
  /** 下载器上的对应路径前缀（手动填写） */
  remote: string;
}

/** 一条路径映射在本机侧的体检结论（见 services.downloader_paths） */
export interface PathProbe {
  local: string;
  remote: string;
  state: "ok" | "empty" | "not_dir" | "missing" | "unmapped";
  /** 结论 + 该做什么，直接可展示 */
  detail: string;
}

export type DownloadTaskState =
  | "downloading"
  | "stalled"
  | "queued"
  | "paused"
  | "checking"
  | "completed"
  | "error"
  | "missing"
  | "unknown";

/** 追踪单元的工单状态；分批入库时同一个种子里各集会停在不同状态。 */
export type DownloadTaskUnitStatus = "wanted" | "grabbed" | "downloaded" | "imported";

/** 任务关联的订阅引用（新契约恒为数组，可为空；元素形状由组件适配阶段收敛）。 */
export interface DownloadTaskSubscription {
  id: string;
  media_item_id: number;
  media_title: string;
  media_kind: string;
  poster_url: string | null;
  /**
   * 投递目的。洗版（upgrade）任务要洗的集在投递前就已入库，"已入库"是前提
   * 而不是成果——进度必须改用 `units[].replaced` 的替换口径讲，否则会出现
   * 「有 16 集已入库」配 0% 进度的自相矛盾。
   */
  purpose: "download" | "upgrade";
  /**
   * 洗版成功后是否保留旧版本共存（规则组的收藏家模式）。
   * false=旧版本移入回收站、保留期满自动清理——替换发生前就要说清楚。
   */
  upgrade_keep_old: boolean;
  /** 种子声明覆盖的**全集**（含已入库的集），不是"还欠哪些集"。 */
  units: {
    season_number: number;
    episode_number: number;
    status: DownloadTaskUnitStatus;
    /** 洗版任务专用：该集是否已被本种子替换完成；补缺下载恒为 false。 */
    replaced: boolean;
    /**
     * 内容核验证明种子里没有这一集（声明的覆盖范围与实际文件不符）。这一集
     * 已退回重新寻找资源，等这个种子永远等不到——卡片要显示为需关注。
     */
    content_missing: boolean;
  }[];
}

/**
 * 下载完成后**还没发生**的那段路，由后端按当前媒体库与监听导入配置推导。
 * 任务中心据此把「等待入库 · 下一步」换成真实步骤链；推不出时整体为 null。
 */
export interface DownloadTaskPlan {
  /** watch=监听导入搬运；inplace=已在库内目录，扫描即入账；downloader_default=不会自动入库 */
  mode: "watch" | "inplace" | "downloader_default";
  /** 搬运策略；mode=watch 才有 */
  strategy: "hardlink" | "copy" | null;
  /** 目标库名；库未定或不进库时为 null */
  library_name: string | null;
  /** 落点目录 */
  dest_path: string | null;
  /** 整理后是否进入媒体库（自定义目录规则为 false） */
  enters_library: boolean;
}

/** 下载器实时任务（见 self.md Task）。subscriptions 恒为数组（可为空）。 */
export interface DownloadTask {
  id: string;
  info_hash?: string | null;
  name: string | null;
  downloader_id: string | null;
  downloader_name: string | null;
  progress: number | null;
  size_bytes: number | null;
  dlspeed_bytes: number | null;
  /** 当前上传速度（字节/秒）；未知为 null */
  upspeed_bytes: number | null;
  /** 累计上传量（字节）；刷流分组汇总用，未知为 null */
  uploaded_bytes: number | null;
  /** 已完成字节；刷流分组汇总用，未知为 null */
  completed_bytes: number | null;
  state: DownloadTaskState;
  /** state 为 error 时下载器给出的可读原因（文件缺失 / 出错详情）；其余为 null */
  error_message?: string | null;
  /** 来源站点 ID；投递的任务才有，外部任务为 null */
  site_id?: string | null;
  site_name?: string | null;
  /** 投递时快照的画面规格（如 2160p）；未知为 null */
  resolution?: string | null;
  media_title?: string | null;
  media_kind?: string | null;
  subscriptions: DownloadTaskSubscription[];
}

export interface DownloadTaskSource {
  id: string;
  name: string;
  client_type: DownloaderClientType;
  status: "active" | "disabled" | "unavailable" | "error";
  message: string | null;
  task_count: number;
}

export interface DownloadTaskSnapshot {
  items: DownloadTask[];
  sources: DownloadTaskSource[];
}

/** 新增/更新下载器的请求体（表单形状；API 层转成 self.md DownloaderInput）。 */
export interface DownloaderPayload {
  name: string;
  client_type: DownloaderClientType;
  url: string;
  username?: string | null;
  password?: string | null;
  save_path?: string | null;
  path_mappings?: PathMapping[] | null;
  enabled?: boolean;
}

/** 把表单 payload 转成新契约 DownloaderInput（save_path 无对应字段，丢弃）。 */
function toDownloaderInput(payload: DownloaderPayload): Record<string, unknown> {
  const input: Record<string, unknown> = {
    name: payload.name,
    kind: payload.client_type,
    url: payload.url,
    ...(payload.username !== undefined ? { username: payload.username ?? "" } : {}),
    ...(payload.password !== undefined ? { password: payload.password ?? "" } : {}),
    path_maps: (payload.path_mappings ?? []).map((m) => ({ from: m.remote, to: m.local })),
    ...(payload.enabled !== undefined ? { enabled: payload.enabled } : {}),
  };
  return input;
}

/** 列出所有已配置的下载器及连接状态。 */
export function listDownloaders(init?: RequestInit): Promise<ConfiguredDownloader[]> {
  return unwrap(request<ApiEnvelope<Downloader[]>>("/downloaders", init));
}

/** 任务中心快照：所有下载器的活跃任务、仍待入库任务与来源健康状态。 */
export function listDownloadTasks(init?: RequestInit): Promise<DownloadTaskSnapshot> {
  return unwrap(request<ApiEnvelope<DownloadTaskSnapshot>>("/downloaders/tasks", init));
}

/**
 * 从下载器移除一个订阅投递任务。
 *
 * 用任务中心的 `task.id`（后端编码为 `{subscribe_id}:{enclosure}`）定位，而不是
 * info hash —— 订阅投递的任务在真正进入客户端前拿不到 hash，任务列表里
 * `info_hash` 恒为 null。默认保留数据文件；只有确认弹窗中显式选择后才传
 * `deleteFiles=true`，避免误删正在下载或做种的数据。
 */
export function deleteDownloadTask(
  taskId: string,
  deleteFiles = false,
): Promise<{ subscribe_id: string; delete_files: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; subscribe_id: string; delete_files: boolean }>>(
      "/downloaders/tasks/remove",
      {
        method: "POST",
        body: JSON.stringify({ task_id: taskId, delete_files: deleteFiles }),
      },
    ),
  ).then((data) => ({
    subscribe_id: data.subscribe_id,
    delete_files: data.delete_files,
  }));
}

/**
 * 立即为长期无进度的订阅任务执行一次换源搜索。
 *
 * 后端会立刻排一个一次性的订阅搜索 Job，并把当前卡住的这个源排除在候选之外，
 * 因此本轮只可能选出**别的**源。旧任务会保留（继续做种/下载），直到新源真的
 * 产生进度为止。
 */
export function replaceDownloadTask(
  taskId: string,
): Promise<{ subscribe_id: string; job_id: string }> {
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; subscribe_id: string; job_id: string }>>(
      "/downloaders/tasks/replace",
      {
        method: "POST",
        body: JSON.stringify({ task_id: taskId }),
      },
    ),
  ).then((data) => ({
    subscribe_id: data.subscribe_id,
    job_id: data.job_id,
  }));
}

/** 获取保存的下载器配置；GET 不执行测试，也不持久化手动测试结果。 */
export function getDownloader(id: string, init?: RequestInit): Promise<ConfiguredDownloader> {
  return unwrap(request<ApiEnvelope<Downloader>>(`/downloaders/${id}`, init));
}

/** 添加下载器配置，不自动测试连接。 */
export function createDownloader(payload: DownloaderPayload): Promise<ConfiguredDownloader> {
  return unwrap(
    request<ApiEnvelope<Downloader>>("/downloaders", {
      method: "POST",
      body: JSON.stringify(toDownloaderInput(payload)),
    }),
  );
}

/** 更新配置后返回未测试状态；旧配置的本地测试结果不再适用。 */
export function updateDownloader(
  id: string,
  payload: DownloaderPayload,
): Promise<ConfiguredDownloader> {
  return unwrap(
    request<ApiEnvelope<Downloader>>(`/downloaders/${id}`, {
      method: "PATCH",
      body: JSON.stringify(toDownloaderInput(payload)),
    }),
  );
}

/** 下载器全局限制（见 schemas.downloader.DownloaderLimitsView）。
 *  限速单位字节/秒，null=不限；max_active_torrents 为 qBittorrent 独有。 */
export interface DownloaderLimits {
  download_limit_bytes: number | null;
  upload_limit_bytes: number | null;
  alt_speed_enabled: boolean | null;
  queue_enabled: boolean | null;
  max_active_downloads: number | null;
  max_active_uploads: number | null;
  max_active_torrents: number | null;
}

const UNLIMITED_LIMITS: DownloaderLimits = {
  download_limit_bytes: null,
  upload_limit_bytes: null,
  alt_speed_enabled: null,
  queue_enabled: null,
  max_active_downloads: null,
  max_active_uploads: null,
  max_active_torrents: null,
};

/**
 * 实时读取下载器的全局限速与任务队列上限。
 */
export function getDownloaderLimits(id: string): Promise<DownloaderLimits> {
  const query = id ? `?id=${encodeURIComponent(id)}` : "";
  return unwrap(
    request<ApiEnvelope<{ download_limit_bytes: number | null; upload_limit_bytes: number | null }>>(
      `/downloaders/limits${query}`,
    ),
  ).then((data) => ({
    ...UNLIMITED_LIMITS,
    download_limit_bytes: data.download_limit_bytes,
    upload_limit_bytes: data.upload_limit_bytes,
  }));
}

/**
 * 写入下载器全局限制，返回回读的生效值。限速 null=取消；其余 null=不改。
 */
export function setDownloaderLimits(
  id: string,
  limits: DownloaderLimits,
): Promise<DownloaderLimits> {
  const query = id ? `?id=${encodeURIComponent(id)}` : "";
  return unwrap(
    request<ApiEnvelope<{ download_limit_bytes: number | null; upload_limit_bytes: number | null }>>(
      `/downloaders/limits${query}`,
      {
        method: "PUT",
        body: JSON.stringify({
          download_limit_bytes: limits.download_limit_bytes ?? 0,
          upload_limit_bytes: limits.upload_limit_bytes ?? 0,
        }),
      },
    ),
  ).then((data) => ({
    ...UNLIMITED_LIMITS,
    download_limit_bytes: data.download_limit_bytes,
    upload_limit_bytes: data.upload_limit_bytes,
  }));
}

/**
 * 启用 / 停用下载器。停用后该下载器不再参与自动投递（手动提交仍可用）。
 */
export function setDownloaderEnabled(id: string, enabled: boolean): Promise<ConfiguredDownloader> {
  return getDownloader(id).then((downloader) =>
    unwrap(
      request<ApiEnvelope<Downloader>>(`/downloaders/${id}`, {
        method: "PATCH",
        body: JSON.stringify({
          name: downloader.name,
          kind: downloader.kind,
          url: downloader.url,
          ...(downloader.username ? { username: downloader.username } : {}),
          ...(downloader.category ? { category: downloader.category } : {}),
          path_maps: downloader.path_maps,
          enabled,
        }),
      }),
    ),
  );
}

/** 设为默认下载器。注意：其他条目的 is_default 会随之变化，调用后应整体刷新列表。 */
export function setDefaultDownloader(id: string): Promise<ConfiguredDownloader> {
  return getDownloader(id).then((downloader) =>
    unwrap(
      request<ApiEnvelope<Downloader>>(`/downloaders/${id}`, {
        method: "PATCH",
        body: JSON.stringify({
          name: downloader.name,
          kind: downloader.kind,
          url: downloader.url,
          ...(downloader.username ? { username: downloader.username } : {}),
          ...(downloader.category ? { category: downloader.category } : {}),
          path_maps: downloader.path_maps,
          is_default: true,
        }),
      }),
    ),
  );
}

import { verificationState } from "../downloader-verification";

/** 手动测试已保存配置。结果仅用于当前页面，不能用 GET 的未测试状态回读覆盖。 */
export async function reverifyDownloader(
  id: string,
  downloader: ConfiguredDownloader,
): Promise<ConfiguredDownloader> {
  try {
    const result = await unwrap(
      request<ApiEnvelope<{ ok: boolean; error?: string | null }>>(`/downloaders/${id}/verify`, {
        method: "POST",
      }),
    );
    return { ...downloader, ...verificationState(result) };
  } catch (error) {
    // 网络/HTTP 拒绝同样是本次测试失败，不能留着上次的「已连接」。
    const detail = error && typeof error === "object" && "message" in error
      ? String(error.message)
      : typeof error === "string" ? error : "";
    return {
      ...downloader,
      ...verificationState({ ok: false, error: detail.trim() || "连接验证失败，请检查网络后重试" }),
    };
  }
}

/** 删除下载器配置。 */
export function deleteDownloader(id: string): Promise<Record<string, never>> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/downloaders/${id}`, { method: "DELETE" }),
  ).then(() => ({} as Record<string, never>));
}

/** 手动提交下载的请求体（表单形状；API 层转成 self.md submit 输入）。 */
export interface DownloadSubmitPayload {
  /** 种子所属站点 ID（TorrentHit.site_id） */
  site_id: string;
  /** 种子下载入口（TorrentHit.download_url） */
  download_url: string;
  /** 站点内种子 ID（TorrentHit.torrent_id）：任务中心据此提供「打开种子页」 */
  torrent_id?: string | null;
  /** 入库目标库（可选）：带上后保存目录由库推导（主根/标题 (年份)） */
  library_id?: number | null;
  /** 种子体积（字节）；新契约 submit 接受，用于投递端展示/分流 */
  size_bytes?: number | null;
  /** 条目标题（推导条目子目录用；身份未确认时不要带） */
  title?: string | null;
  year?: number | null;
  /** 种子副标题（识别线索：中文片名/「全N集」帮扫描器收敛拼音命名种子） */
  subtitle?: string | null;
  /** 下载弹窗手选的保存目录（本机视角，优先于库推导） */
  save_path?: string | null;
  /** 指定投递到哪台下载器（配了多台按需分流）；缺省用默认下载器 */
  downloader_id?: string | null;
  /** 服务端按已确认的 TMDB 身份重新匹配媒体库并选择监听导入目录 */
  auto_route?: boolean;
  /** 智能入库的媒体类型，与 tmdb_id 一起构成后端路由输入 */
  media_kind?: "movie" | "tv";
  /**
   * 种子分类（TorrentHit.category）：提交成功后后端按它记住本次的保存位置选择。
   * 分类只有前端拿得到——提交接口的入参是 site_id/download_url/torrent_id，
   * 后端没有搜索结果上下文。**不传就不记**——这既是订阅投递等非搜索入口不产生
   * 记忆的方式，也是保存位置弹窗里「记住本次选择」没勾时的表达方式。
   */
  category?: string | null;
  /** 智能入库已确认的 TMDB 条目 ID */
  tmdb_id?: number;
}

/** 手动下载识别未收敛时返回的 TMDB 候选（供界面解释为何不自动投递）。 */
export interface ManualDownloadTargetCandidate {
  tmdb_id: number;
  title: string;
  year: number | null;
  episode_count: number | null;
}

/** 手动搜索种子的「识别 → 库路由 → 监听投递目录」预检结果。 */
export interface ManualDownloadTarget {
  status: "ready" | "ambiguous" | "not_found";
  tmdb_id: number | null;
  candidates: ManualDownloadTargetCandidate[];
  library_id: number | null;
  library_name: string | null;
  mode: "watch" | "inplace" | "downloader_default" | null;
  path: string | null;
  /** 条目目录的完整路径预览（后端按生效的命名模板渲染），展示落点时用它 */
  entry_dir: string | null;
  staging_path: string | null;
  route_matched: boolean | null;
  route_reason: string | null;
  ok: boolean;
  warning: string | null;
}

/**
 * 预演一条搜索结果能否被可靠识别并投递到匹配库的监听目录。
 *
 * 复用后端的路由预演端点（`POST /subscriptions/download-routing-preview`），
 * 它与真实投递同源：同一套目录 / 命名模板 / 默认下载器配置。
 *
 * 说明：身份识别（TMDB 匹配与候选消歧）不在本函数的职责内——调用方若已
 * 确认身份就传 `selected_tmdb_id`，此时 `status` 为 `ready`/`not_found`；
 * `candidates` 始终为空，因为消歧候选由搜索/识别接口给出。
 */
export function resolveManualDownloadTarget(payload: {
  kind: "movie" | "tv";
  title: string;
  year: number;
  subtitle?: string | null;
  /** 用这台下载器的路径映射预检；缺省沿用后端默认下载器语义 */
  downloader_id?: string | null;
  /** 歧义时由用户确认的、且必须属于本次候选的 TMDB ID */
  selected_tmdb_id?: number | null;
}): Promise<ManualDownloadTarget> {
  return unwrap(
    request<
      ApiEnvelope<{
        mode: "watch" | "inplace" | "downloader_default" | null;
        path: string | null;
        entry_dir: string | null;
        staging_path: string | null;
        library_id: string | null;
        library_name: string | null;
        route_matched: boolean | null;
        route_reason: string | null;
        ok: boolean;
        warning: string | null;
      }>
    >("/subscriptions/download-routing-preview", {
      method: "POST",
      body: JSON.stringify({
        kind: payload.kind,
        title: payload.title,
        year: payload.year,
        ...(payload.downloader_id ? { downloader_id: payload.downloader_id } : {}),
        ...(payload.selected_tmdb_id != null
          ? { tmdb_id: String(payload.selected_tmdb_id) }
          : {}),
      }),
    }),
  ).then((data) => ({
    status: data.ok ? ("ready" as const) : ("not_found" as const),
    tmdb_id: payload.selected_tmdb_id ?? null,
    candidates: [],
    library_id: data.library_id != null ? Number(data.library_id) : null,
    library_name: data.library_name,
    mode: data.mode,
    path: data.path,
    entry_dir: data.entry_dir,
    staging_path: data.staging_path,
    route_matched: data.route_matched,
    route_reason: data.route_reason,
    ok: data.ok,
    warning: data.warning,
  }));
}

/** 手动提交下载的结果（新契约只回 `{ok, save_path?}`，其余字段补默认）。 */
export interface DownloadSubmitResult {
  info_hash: string | null;
  name: string;
  /** 种子提交前已存在于下载器（幂等，未重复添加） */
  already_exists: boolean;
  downloader_id: string;
  downloader_name: string;
  /** 实际使用的保存目录（null = 下载器自身默认目录） */
  save_path: string | null;
}

/**
 * 把一条搜索结果种子提交到下载器：后端带站点登录态取回 .torrent 再递交。
 * 手动提交体保留下载目录、路由身份与搜索分类，后端据此配置下载器目标。
 * 失败抛 HttpError，message 为可读中文。
 */
export function submitTorrentDownload(
  payload: DownloadSubmitPayload,
): Promise<DownloadSubmitResult> {
  const body: Record<string, unknown> = {
    download_url: payload.download_url,
    ...(payload.torrent_id ? { torrent_id: payload.torrent_id } : {}),
    title: payload.title ?? payload.subtitle ?? "",
    ...(payload.site_id ? { site_id: payload.site_id } : {}),
    ...(payload.size_bytes != null ? { size_bytes: payload.size_bytes } : {}),
    ...(payload.downloader_id != null ? { downloader_id: payload.downloader_id } : {}),
    ...(payload.save_path != null ? { save_path: payload.save_path } : {}),
    ...(payload.auto_route ? { auto_route: true } : {}),
    ...(payload.media_kind ? { media_kind: payload.media_kind } : {}),
    ...(payload.tmdb_id != null ? { tmdb_id: payload.tmdb_id } : {}),
    ...(payload.year != null ? { year: payload.year } : {}),
    ...(payload.category ? { category: payload.category } : {}),
  };
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; save_path?: string | null }>>("/downloaders/submit", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  ).then((result) => ({
    info_hash: null,
    name: payload.title ?? payload.subtitle ?? "",
    already_exists: false,
    downloader_id: payload.downloader_id ?? "",
    downloader_name: "",
    save_path: result.save_path ?? null,
  }));
}

/** 一条保存位置记忆（见后端 DownloadTargetPrefView）。 */
export interface DownloadTargetPref {
  /** 种子分类（TorrentCategory 值） */
  category: string;
  kind: "smart" | "dir" | "default";
  /** 固定目录；仅 kind=dir 有值 */
  save_path: string | null;
  downloader_id: string | null;
  /** 下载器名称；null = 用默认下载器，或指定的下载器已被删除（记忆失效） */
  downloader_name: string | null;
  /** 这条记忆最后一次被改变的时间 */
  updated_at: string;
}

/**
 * 当前登录者的全部保存位置记忆（KV 存储，按分类索引）。
 * 搜索结果页挂载时拉一次：命中记忆的分类点「下载」走确认条，否则走完整弹窗。
 */
export function listDownloadTargetPrefs(): Promise<DownloadTargetPref[]> {
  return unwrap(request<ApiEnvelope<Record<string, unknown>>>("/downloaders/target-prefs")).then(
    (prefs) =>
      Object.entries(prefs ?? {}).map(([category, value]) => {
        const stored = (
          typeof value === "object" && value !== null ? value : {}
        ) as Partial<DownloadTargetPref>;
        return {
          category,
          kind: stored.kind ?? "default",
          save_path: stored.save_path ?? null,
          downloader_id:
            stored.downloader_id != null ? String(stored.downloader_id) : null,
          downloader_name: stored.downloader_name ?? null,
          updated_at: stored.updated_at ?? "",
        };
      }),
  );
}

/**
 * 清除某分类的记忆（确认条上的「不再记住」）。幂等：本就没有也返回成功。
 * 旧 DELETE 端点已下线，改为 PUT null（新契约的 KV 语义）。
 */
export function forgetDownloadTargetPref(category: string): Promise<null> {
  return unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/downloaders/target-prefs/${encodeURIComponent(category)}`,
      { method: "PUT", body: JSON.stringify(null) },
    ),
  ).then(() => null);
}
