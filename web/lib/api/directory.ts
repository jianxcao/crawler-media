import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

export type TransferMode = "hardlink" | "copy" | "move";

export interface DirectorySettings {
  movie_root: string;
  tv_root: string;
  transfer_mode: TransferMode;
  movie_naming: string;
  tv_naming: string;
  scrape: boolean;
  watch_intake: string | null;
  watch_inplace: string | null;
}

export async function getDirectory(): Promise<DirectorySettings> {
  const envelope = await request<ApiEnvelope<DirectorySettings>>("/directory");
  return envelope.data;
}

export async function putDirectory(body: DirectorySettings): Promise<DirectorySettings> {
  const envelope = await request<ApiEnvelope<DirectorySettings>>("/directory", {
    method: "PUT",
    body: JSON.stringify(body),
  });
  return envelope.data;
}

export async function setTransferMode(mode: TransferMode): Promise<DirectorySettings> {
  const current = await getDirectory();
  return putDirectory({ ...current, transfer_mode: mode });
}

export async function setWatchIntake(path: string | null): Promise<DirectorySettings> {
  const current = await getDirectory();
  const trimmed = path?.trim();
  return putDirectory({ ...current, watch_intake: trimmed ? trimmed : null });
}
