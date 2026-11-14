import type { BrowserSettingsPayload } from "@/lib/browser-settings";
import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

/**
 * 浏览器（CDP / 受管 Obscura）设置的 HTTP 客户端。
 *
 * 路径一律是 API 相对路径：前缀由 `request()` 按 `apiBaseUrl` 补，这里绝不
 * 手写 `/api/v1`（写一次就会被拼成 `/api/v1/api/v1/...`，请求直接打空）。
 */

/** GET/PUT `/settings/browser` 的响应：当前生效的配置与按站点统计的用量。 */
export interface BrowserSettingsData {
  cdp?: { enabled?: boolean; url?: string };
  obscura?: { install?: "download" | "preinstalled" };
  user_agent?: string;
  /** 全部站点数 */
  browser_sites?: number;
  /** 走受管 Obscura 的站点数（没有自带 CDP 端点的那些） */
  managed_browser_sites?: number;
  /** 站点名 → 不支持 CDP 维护的原因 */
  incompatible_endpoints?: Record<string, string>;
}

/** 受管 Obscura 进程的实时状态。 */
export interface ObscuraStatusData {
  installed: boolean;
  running: boolean;
  downloading: boolean;
  port: number;
  cdp_url: string | null;
  expected_version: string;
  reported_version: string | null;
  binary_path: string;
  install_policy: string;
  last_error: string | null;
  not_started_reason: string | null;
}

/** 从本机浏览器的 Cookie 库同步 CDP 凭据的结果。 */
export interface SyncCdpData {
  total_cookies_scanned: number;
  updated_sites: number;
  candidate_sites?: Array<Record<string, any>>;
}

/** 读取浏览器设置（GET `/settings/browser`）。 */
export async function fetchBrowserSettings(): Promise<BrowserSettingsData> {
  return (await request<ApiEnvelope<BrowserSettingsData>>("/settings/browser")).data;
}

/** 保存浏览器设置（PUT `/settings/browser`），返回落库后的取值。 */
export async function updateBrowserSettings(
  payload: BrowserSettingsPayload,
): Promise<BrowserSettingsData> {
  return (
    await request<ApiEnvelope<BrowserSettingsData>>("/settings/browser", {
      method: "PUT",
      body: JSON.stringify(payload),
    })
  ).data;
}

/** 受管 Obscura 的安装 / 运行状态（GET `/settings/browser/obscura`）。 */
export async function fetchObscuraStatus(): Promise<ObscuraStatusData> {
  return (await request<ApiEnvelope<ObscuraStatusData>>("/settings/browser/obscura")).data;
}

/** 拉起受管 Obscura（POST `/settings/browser/obscura/start`），返回最新状态。 */
export async function startObscura(): Promise<ObscuraStatusData> {
  return (
    await request<ApiEnvelope<ObscuraStatusData>>("/settings/browser/obscura/start", {
      method: "POST",
    })
  ).data;
}

/** 停止受管 Obscura（POST `/settings/browser/obscura/stop`），返回最新状态。 */
export async function stopObscura(): Promise<ObscuraStatusData> {
  return (
    await request<ApiEnvelope<ObscuraStatusData>>("/settings/browser/obscura/stop", {
      method: "POST",
    })
  ).data;
}

/** 把本机浏览器的 Cookie 同步成站点凭据（POST `/settings/browser/sync-cdp`）。 */
export async function syncCdp(): Promise<SyncCdpData> {
  return (
    await request<ApiEnvelope<SyncCdpData>>("/settings/browser/sync-cdp", { method: "POST" })
  ).data;
}
