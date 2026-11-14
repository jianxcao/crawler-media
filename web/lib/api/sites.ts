import { request } from "@/lib/http";

/** 后端统一响应信封（见 docs/api-contracts/self.md） */
interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

// ---------------------------------------------------------------------------
// DTO（docs/api-contracts/self.md：sites）
// ---------------------------------------------------------------------------

/** 站点授权方式（后端从 api_key/cookie 是否配置派生，可能为 none）。 */
export type SiteAuthType = "cookie" | "apikey" | "credential" | "none";

/** 站点配置视图。凭证字段（cookie/api_key）在列表/读取时被后端遮盖为 null，
 *  仅创建响应回显。 */
export interface Site {
  id: string;
  name: string;
  url: string;
  profile_id: string;
  cookie: string | null;
  api_key: string | null;
  rss_url: string | null;
  proxy: string | null;
  rate_limit_per_minute: number | null;
  cdp_url: string | null;
  downloader_id: string | null;
  enabled: boolean;
  auth_type: SiteAuthType;
  maintenance_via: "http" | "browser";
}

/** 刷流配置（PATCH /sites/{id}/boost 的可选字段，settings KV 持久化）。 */
export interface BoostConfig {
  enabled?: boolean;
  budget_bytes?: number | null;
  hold_days?: number | null;
}

/** 目录项：一个可用的 Indexer profile 及其支持授权方式。 */
export interface CatalogItem {
  profile_id: string;
  auth_types: SiteAuthType[];
}

/** 创建 / 更新站点时可提交的字段（凭证字段仅写入时回显）。 */
export interface SiteConfigPayload {
  name?: string;
  url?: string;
  cookie?: string | null;
  api_key?: string | null;
  rss_url?: string | null;
  proxy?: string | null;
  rate_limit_per_minute?: number | null;
  cdp_url?: string | null;
  downloader_id?: string | null;
  enabled?: boolean;
  maintenance_via?: "http" | "browser";
  /** 仅供前端展示提示；后端忽略（auth_type 由 api_key/cookie 派生）。 */
  auth_type?: SiteAuthType;
}

/** POST /sites/{id}/verify 的结果。 */
export interface VerifyResult {
  ok: boolean;
  error?: string | null;
  torrent_count?: number | null;
}

/** 刷流运行统计（GET /sites/boost-stats：每个已配置站点一行）。 */
export interface SiteBoostStats {
  site_id: string;
  site_name: string;
  boost: BoostConfig;
}

// ---------------------------------------------------------------------------
// 端点函数
// ---------------------------------------------------------------------------

/** 列出系统支持的 Indexer profile 清单（供渲染"可添加"列表）。 */
export function listSiteCatalog(init?: RequestInit): Promise<CatalogItem[]> {
  return unwrap(request<ApiEnvelope<CatalogItem[]>>("/sites/catalog", init));
}

/** 列出所有已配置站点（凭证被后端遮盖）。 */
export function listConfiguredSites(init?: RequestInit): Promise<Site[]> {
  return unwrap(request<ApiEnvelope<Site[]>>("/sites", init));
}

/** 获取单个已配置站点详情（回读用；对外只暴露带业务语义的写接口）。 */
function getSite(siteId: string, init?: RequestInit): Promise<Site> {
  return unwrap(request<ApiEnvelope<Site>>(`/sites/${siteId}`, init));
}

/** 新增配置一个站点（profile_id 必须存在于 Indexer profiles；创建响应回显凭证）。 */
export function configureSite(siteId: string, payload: SiteConfigPayload): Promise<Site> {
  return unwrap(
    request<ApiEnvelope<Site>>("/sites", {
      method: "POST",
      body: JSON.stringify({
        ...payload,
        profile_id: siteId,
        name: payload.name ?? siteId,
        ...(payload.url ? { url: payload.url } : {}),
      }),
    }),
  );
}

/** 更新已配置站点（PATCH 只改传入字段；含 enabled 与凭证）。 */
export function updateSite(siteId: string, payload: SiteConfigPayload): Promise<Site> {
  return unwrap(
    request<ApiEnvelope<Site>>(`/sites/${siteId}`, {
      method: "PATCH",
      body: JSON.stringify(payload),
    }),
  );
}

/** 启用 / 停用站点（并入 PATCH /sites/{id}）。 */
export function setSiteEnabled(siteId: string, enabled: boolean): Promise<Site> {
  return unwrap(
    request<ApiEnvelope<Site>>(`/sites/${siteId}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),
  );
}

/** 手动触发一次探活校验（POST /sites/{id}/verify），返回校验后的站点。 */
export async function reverifySite(siteId: string): Promise<Site> {
  const result = await unwrap(
    request<ApiEnvelope<VerifyResult>>(`/sites/${siteId}/verify`, { method: "POST" }),
  );
  if (!result.ok) {
    throw new Error(result.error ?? "站点连通性校验失败");
  }
  return getSite(siteId);
}

/** 设置自动刷分享率：开关 + 存储预算 + 汰换保留期（省略的字段不修改）。
 *  刷流配置走 PATCH /sites/{id}/boost，完成后回读站点视图。 */
export async function setSiteRatioBoost(
  siteId: string,
  enabled: boolean,
  budgetBytes?: number,
  holdDays?: number,
): Promise<Site> {
  await unwrap(
    request<ApiEnvelope<{ site_id: string; boost: BoostConfig }>>(`/sites/${siteId}/boost`, {
      method: "PATCH",
      body: JSON.stringify({
        enabled,
        budget_bytes: budgetBytes ?? null,
        hold_days: holdDays ?? null,
      }),
    }),
  );
  return getSite(siteId);
}

/** 各站点的刷流运行统计，按 site_id 索引（从未刷流且未开启的站点没有条目）。 */
export async function listSiteBoostStats(
  init?: RequestInit,
): Promise<Record<string, SiteBoostStats>> {
  const rows = await unwrap(request<ApiEnvelope<SiteBoostStats[]>>("/sites/boost-stats", init));
  return Object.fromEntries(rows.map((row) => [row.site_id, row]));
}

/** 删除站点配置。 */
export async function deleteSite(siteId: string): Promise<{ site_id: string }> {
  await unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/sites/${siteId}`, { method: "DELETE" }),
  );
  return { site_id: siteId };
}
