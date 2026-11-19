import { request } from "@/lib/http";
import type { MediaType } from "@/lib/media-types";

import { createSubscriptionBody } from "../subscription-form";
import type { DownloadTask } from "./downloaders";
import type {
  CreateSubscriptionPayload,
  DispatchPreview,
  FetchMode,
  GrabPayload,
  PipelineHealth,
  PrepareResult,
  ReleaseForecastDay,
  RuleSet,
  RuleSetAtom,
  Subscription,
  SubscriptionActivity,
  SubscriptionCoverage,
  SubscriptionDetail,
  SubscriptionDownload,
  SubscriptionRemovalOptions,
  SubscriptionRemovalPreview,
  SubscriptionTargetPreviewPayload,
  TodaySubscriptionArrival,
  UpgradeRunReport,
} from "./subscription-types";

export type * from "./subscription-types";

/** 后端统一响应信封 */
interface ApiEnvelope<T> {
  ok: boolean;
  code: string;
  message: string;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/**
 * 投递路由预检：订阅弹窗选库时调用，预演下载会落到哪、能否自动入库。
 * `libraryId` 为 null 且带 `tmdbId` 时由后端按收藏范围路由选库并返回理由。
 *
 * POST /subscriptions/download-routing-preview。
 */
export function previewSubscriptionDownloadRouting(
  kind: string,
  libraryId: string | null,
  tmdbId?: string | null,
  identity?: { title?: string | null; year?: number | null },
): Promise<DispatchPreview> {
  return unwrap(
    request<ApiEnvelope<DispatchPreview>>("/subscriptions/download-routing-preview", {
      method: "POST",
      body: JSON.stringify({
        kind,
        library_id: libraryId != null ? String(libraryId) : null,
        tmdb_id: tmdbId ?? null,
        title: identity?.title ?? null,
        year: identity?.year ?? null,
      }),
    }),
  );
}

/**
 * 订阅链路体检：逐库预演「投递 → 转移 → 入库」，与真实投递同源判定。
 * 新契约返回 `{status, sites_configured, rule_sets_configured, downloaders_configured, issues}`，
 * 字段子集与旧 PipelineHealth 不一致处由组件适配阶段收敛。
 */
export function checkSubscriptionAutomationReadiness(
  init?: RequestInit,
): Promise<PipelineHealth> {
  return unwrap(request<ApiEnvelope<PipelineHealth>>("/subscriptions/automation-readiness", init));
}

/**
 * 订阅预检：按严格 title_ref（tmdb:movie|tv:<id> / douban:<id>）解析媒体、季集、已有订阅。
 * 后端真实实现（POST /subscriptions/title-preview）；其他形状返回 400 title_ref.invalid。
 */
export function previewSubscriptionTitle(
  payload: SubscriptionTargetPreviewPayload,
): Promise<PrepareResult> {
  return unwrap(
    request<ApiEnvelope<PrepareResult>>("/subscriptions/title-preview", {
      method: "POST",
      body: JSON.stringify({ title_ref: payload.title_ref }),
    }),
  );
}

/** 创建订阅（同条目重复订阅幂等返回已有）。后端接受 title_ref 直接解析。 */
export function createSubscription(
  payload: CreateSubscriptionPayload,
): Promise<Subscription> {
  const body = createSubscriptionBody(payload);
  return unwrap(
    request<ApiEnvelope<Subscription>>("/subscriptions", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  );
}

/** 订阅列表（含工单进度）。kind 缺省返回全部。 */
export function listSubscriptions(
  kind?: MediaType,
  init?: RequestInit,
): Promise<Subscription[]> {
  const query = kind ? `?kind=${kind}` : "";
  return unwrap(request<ApiEnvelope<Subscription[]>>(`/subscriptions${query}`, init));
}

/**
 * 今日可能入库（真实）：在途（grabbed）与当日入库（imported）的订阅。
 * 后端按 pending 与 ledger 修改时间计算；无预告/ETA 时相应字段为 null。
 */
export function listTodaySubscriptionArrivals(
  init?: RequestInit,
): Promise<TodaySubscriptionArrival[]> {
  return unwrap(
    request<ApiEnvelope<TodayArrivalItem[]>>("/subscriptions/today-arrivals", init),
  ).then((items) =>
    items.map((item) => ({
      subscription_id: item.subscription_id,
      wanted_id: "",
      media_title: item.title,
      media_kind: item.coverage?.kind === "tv" ? "tv" : "movie",
      season_number: item.coverage && "season" in item.coverage ? item.coverage.season : 0,
      episode_number: 0,
      status: item.status === "imported" ? "imported" : item.status === "grabbed" ? "grabbed" : "wanted",
      air_date: null,
      expected_day: "",
      days_ahead: 0,
      release_forecast: null,
      next_probe_at: null,
      info_hash: item.info_hash ?? null,
      grabbed_at: item.grabbed_at ?? null,
      downloaded_at: null,
      estimated_release_to_import_minutes: 0,
      estimated_download_to_import_minutes: 0,
    })),
  );
}

/** 新契约 today-arrivals 的单条条目。 */
interface TodayArrivalItem {
  subscription_id: string;
  title: string;
  coverage: SubscriptionCoverage | null;
  eta: string | null;
  status?: "grabbed" | "imported" | "wanted";
  info_hash?: string | null;
  grabbed_at?: string | null;
}

/** 订阅详情（含工单明细 wanted：单元状态 + 履历，后端现算）。 */
export function getSubscription(id: string): Promise<SubscriptionDetail> {
  return unwrap(request<ApiEnvelope<SubscriptionDetail>>(`/subscriptions/${id}`));
}

/** 修改订阅。PATCH 只接受当前合同字段；旧的 selected_seasons / rule_set_id 不再发送。 */
export function updateSubscription(
  id: string,
  payload: {
    coverage?: { kind: "tv"; season: number; episode_from: number };
    follow_future?: boolean;
    filter_id?: string | null;
    /** 换入库目标库；显式传 null=清除指定、改回按默认库路由；缺省不变 */
    library_id?: string | null;
  },
): Promise<Subscription> {
  const body: Record<string, unknown> = {};
  if (payload.coverage) body.coverage = payload.coverage;
  if (payload.filter_id != null) body.filter_id = String(payload.filter_id);
  if (payload.follow_future !== undefined) body.follow_future = payload.follow_future;
  if (payload.library_id !== undefined) {
    body.library_id = payload.library_id === null ? null : String(payload.library_id);
  }
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  );
}

/** 立即搜索：缺口工单跳过冷却重新排队（暂停中/无可搜缺口时后端报可读错误）。 */
export function searchMissingSubscriptionResources(
  id: string,
): Promise<{ reset_count: number }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean; reset_count: number }>>(
      `/subscriptions/${id}/missing-resource-searches`,
      { method: "POST" },
    ),
  ).then(({ reset_count }) => ({ reset_count }));
}

/**
 * 触发一轮洗版：可升级/缺失的单元排入立即搜索，并返回单元级体检报告。
 * 后端按 facts/pending/规则组目标真实计算 counts 与 units。
 */
export function runSubscriptionUpgradeRound(
  id: string,
  ruleSetId?: string | null,
): Promise<UpgradeRunReport> {
  return unwrap(
    request<ApiEnvelope<UpgradeRunReport>>(`/subscriptions/${id}/upgrade-runs`, {
      method: "POST",
      body: JSON.stringify(ruleSetId != null ? { filter_id: ruleSetId } : {}),
    }),
  );
}

/**
 * 手动选种：把一条搜索结果直接投给订阅（跳过规则组过滤；身份匹配照常）。
 * 旧端点 `/subscriptions/{id}/selected-torrent-downloads` 已下线，改投
 * `/downloaders/submit` 完成实际下载。
 */
export function downloadSelectedTorrentForSubscription(
  id: string,
  payload: GrabPayload,
): Promise<{ units: { season_number: number; episode_number: number }[] }> {
  const body: Record<string, unknown> = {
    download_url: payload.download_url ?? "",
    ...(payload.torrent_id ? { torrent_id: payload.torrent_id } : {}),
    title: payload.title,
    subscribe_id: id,
    ...(payload.site_id ? { site_id: payload.site_id } : {}),
    ...(payload.size_bytes != null ? { size_bytes: payload.size_bytes } : {}),
  };
  return unwrap(
    request<
      ApiEnvelope<{
        ok: boolean;
        save_path?: string | null;
        units?: { season_number: number; episode_number: number }[];
      }>
    >("/downloaders/submit", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  ).then((resp) => ({ units: resp.units ?? [] }));
}

/**
 * 暂停 / 恢复订阅：PATCH /subscriptions/{id} `{tracking_state}`。
 */
export function setSubscriptionTrackingState(
  id: string,
  state: "active" | "paused",
): Promise<Subscription> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ tracking_state: state }),
    }),
  );
}

export function setSubscriptionFetchMode(
  id: string,
  fetch_mode: FetchMode,
): Promise<Subscription> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ fetch_mode }),
    }),
  );
}

export function setSubscriptionSearchInterval(
  id: string,
  search_interval_secs: number,
): Promise<Subscription> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ search_interval_secs }),
    }),
  );
}

/**
 * 洗版保留旧版本：PATCH /subscriptions/{id} `{keep_old_versions}`。
 */
export function setSubscriptionKeepOldVersions(
  id: string,
  enabled: boolean,
): Promise<Subscription> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ keep_old_versions: enabled }),
    }),
  );
}

/** 剧集自动续订：PATCH /subscriptions/{id} `{follow_future}`。 */
export function setSubscriptionFollowFuture(
  id: string,
  enabled: boolean,
): Promise<Subscription> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ follow_future: enabled }),
    }),
  );
}

/**
 * 成员停止关注（退订）；服务端保证不影响仍在追踪的其他成员。
 * 调用 DELETE /subscriptions/{id} 移除订阅。
 */
export function unsubscribeFromSubscription(id: string): Promise<Record<string, never>> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/subscriptions/${id}`, {
      method: "DELETE",
    }),
  ).then(() => ({}));
}

/** 取消订阅时读取全作品文件与该订阅 Torrent 预览；不支持按季过滤。 */
export function getSubscriptionRemovalPreview(id: string): Promise<SubscriptionRemovalPreview> {
  return unwrap(
    request<ApiEnvelope<SubscriptionRemovalPreview>>(`/subscriptions/${id}/removal-preview`),
  );
}

/**
 * 清理已移出订阅范围的那几季的内容（减季后的可选收尾）。
 *
 * 按文件覆盖单元和 Torrent 的季号选择清理范围，订阅本身保留。
 * 请求返回时清理已执行；回执中的 queued 不表示存在后台 Job。
 */
export function cleanupSubscriptionSeasons(
  id: string,
  seasons: number[],
  options: SubscriptionRemovalOptions,
): Promise<{ queued: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean }>>(`/subscriptions/${id}/season-cleanup`, {
      method: "POST",
      body: JSON.stringify({
        seasons,
        delete_torrents: options.deleteTorrents,
        delete_library_files: options.deleteLibraryFiles,
      }),
    }),
  );
}

/**
 * 管理员永久删除订阅与追踪工单。
 *
 * 默认不动任何已有内容；勾了联动清理时由服务端同一请求完成。回执即 `{deleted}`。
 */
export function deleteSubscriptionPermanently(
  id: string,
  options?: SubscriptionRemovalOptions,
): Promise<{ deleted: boolean }> {
  const query = new URLSearchParams({
    delete_torrents: String(options?.deleteTorrents ?? false),
    delete_library_files: String(options?.deleteLibraryFiles ?? false),
  });
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/subscriptions/${id}?${query}`, {
      method: "DELETE",
    }),
  );
}

/**
 * 订阅在途种子的实时下载进度：读全局 `/downloaders/tasks`（DownloadTask DTO），
 * 只保留该订阅 wanted 单元锚定的 info_hash。没有在途 hash 时不发请求。
 */
export async function listActiveSubscriptionDownloads(
  wantedHashes: readonly string[],
  init?: RequestInit,
): Promise<SubscriptionDownload[]> {
  const wanted = new Set(wantedHashes.filter((hash) => hash.length > 0));
  if (wanted.size === 0) return [];
  const snapshot = await unwrap(
    request<ApiEnvelope<{ items: DownloadTask[] }>>("/downloaders/tasks", init),
  );
  return snapshot.items
    .filter((task) => task.info_hash != null && wanted.has(task.info_hash))
    .map((task) => ({
      info_hash: task.info_hash as string,
      name: task.name,
      progress: task.progress,
      size_bytes: task.size_bytes,
      dlspeed_bytes: task.dlspeed_bytes,
      state: task.state as SubscriptionDownload["state"],
      downloader_name: task.downloader_name,
    }));
}

/**
 * 订阅活动时间线（后端按 Job 记录 + facts 推导事件类型与中文消息）。
 */
export function listSubscriptionActivities(
  id: string,
  limit = 100,
): Promise<SubscriptionActivity[]> {
  return unwrap(
    request<ApiEnvelope<{ items: SubscriptionActivityItem[]; total: number }>>(
      `/subscriptions/${id}/activities?limit=${limit}`,
    ),
  ).then((body) =>
    (body.items ?? []).map((item) => ({
      id: item.id ?? "",
      type: (item.type ?? "searched") as SubscriptionActivity["type"],
      message: item.message ?? [item.kind, item.status, item.error].filter(Boolean).join(" · "),
      payload: { ...item },
      created_at: item.started_at ?? item.finished_at ?? "",
      wanted_item_id: null,
    })),
  );
}

/** 新契约 activities 的单条事件（type/message 由后端给出）。 */
interface SubscriptionActivityItem {
  id?: string;
  type?: string;
  message?: string;
  kind: string;
  status: string;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** 规则组列表（首次访问后端自动创建默认组）。 */
export function listRuleSets(init?: RequestInit): Promise<RuleSet[]> {
  return unwrap(request<ApiEnvelope<RuleSet[]>>("/rule-sets", init));
}

/** 创建规则组，原样保存原子条件。 */
export function createRuleSet(
  name: string,
  atoms: RuleSetAtom[],
  keepOldVersions = false,
): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<RuleSet>>("/rule-sets", {
      method: "POST",
      body: JSON.stringify({ name, atoms, keep_old_versions: keepOldVersions }),
    }),
  );
}

/** 更新规则组（只影响之后的匹配评估）。 */
export function updateRuleSet(
  id: string,
  name: string,
  atoms: RuleSetAtom[],
  keepOldVersions = false,
): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<RuleSet>>(`/rule-sets/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ name, atoms, keep_old_versions: keepOldVersions }),
    }),
  );
}

/** 设为默认规则组（新订阅未指定规则组时使用；不改已有订阅的挂靠）。 */
export function setDefaultRuleSet(id: string): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<{ default_rule_set_id: string }>>("/rule-sets/default", {
      method: "PUT",
      body: JSON.stringify({ id }),
    }),
  ).then((result) => ({
    id: result.default_rule_set_id,
    name: "",
    is_default: true,
    atoms: [],
  }));
}

/** 删除规则组（默认组与被订阅引用的组后端会拒绝，错误信息可直接展示）。 */
export function deleteRuleSet(id: string): Promise<void> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/rule-sets/${id}`, { method: "DELETE" }),
  ).then(() => undefined);
}

export async function fetchReleaseForecast(
  id: string,
): Promise<{ samples: number; days: ReleaseForecastDay[] }> {
  const envelope = await request<ApiEnvelope<{ samples: number; days: ReleaseForecastDay[] }>>(
    `/subscriptions/${id}/release-forecast`,
  );
  return envelope.data;
}
