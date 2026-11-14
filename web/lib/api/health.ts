import { request } from "@/lib/http";

export interface HealthResponse {
  status: string;
  version?: string;
  commit?: string;
  service: string;
  environment: string;
}

/** 读取健康状态（GET /health）。只接受当前 `{ ok: true, data }` 信封。 */
export async function getHealth(init?: RequestInit): Promise<HealthResponse> {
  const payload = await request<{ ok: boolean; data: HealthResponse }>("/health", init);
  return payload.data;
}
