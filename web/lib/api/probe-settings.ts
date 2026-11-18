import { request } from "@/lib/http";

export interface ProbeSettings {
  metadata_workers: number;
  remote_concurrency: number;
  remote_cooldown_ms: number;
}

interface Envelope { data: ProbeSettings }

export async function getProbeSettings(): Promise<ProbeSettings> {
  return (await request<Envelope>("/settings/probe")).data;
}

export async function saveProbeSettings(settings: ProbeSettings): Promise<ProbeSettings> {
  return (await request<Envelope>("/settings/probe", {
    method: "PUT", body: JSON.stringify(settings),
  })).data;
}
