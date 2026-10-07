import { request } from "@/lib/http";

export interface HealthResponse {
  status: string;
  version?: string;
  commit?: string;
  service: string;
  environment: string;
}

/**
 * 读取健康状态（GET /health 保留）。
 * 兼容两种信封：自有 API 返回 { ok, data }，直接形态则原样返回。
 */
export async function getHealth(init?: RequestInit): Promise<HealthResponse> {
  const payload = await request<HealthResponse | { ok: boolean; data: HealthResponse }>(
    "/health",
    init,
  );
  if (payload && typeof payload === "object" && "data" in payload) {
    return (payload as { ok: boolean; data: HealthResponse }).data;
  }
  return payload as HealthResponse;
}
