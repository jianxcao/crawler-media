import { request } from "@/lib/http";

interface ApiEnvelope<T> { ok: boolean; data: T }
export type TransferMode = "hardlink" | "copy" | "move";
export interface TransferSettings {
  revision: number;
  transfer_mode: TransferMode;
  scrape: boolean;
  watch_intake: string | null;
  watch_inplace: string | null;
}
export type TransferSettingsPatch = Partial<Omit<TransferSettings, "revision">>;

export async function getTransferSettings(): Promise<TransferSettings> {
  return (await request<ApiEnvelope<TransferSettings>>("/settings/transfer")).data;
}

export async function patchTransferSettings(
  expectedRevision: number,
  patch: TransferSettingsPatch,
): Promise<TransferSettings> {
  return (await request<ApiEnvelope<TransferSettings>>("/settings/transfer", {
    method: "PATCH",
    body: JSON.stringify({ expected_revision: expectedRevision, ...patch }),
  })).data;
}
