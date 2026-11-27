import { request } from "@/lib/http";

export interface ProbeLibraryQueueSummary {
  library_id: string;
  library_name: string;
  queued_count: number;
  running_count: number;
  obligation_count: number;
  total_pending: number;
}

export interface ProbeQueueStatus {
  paused: boolean;
  remote_reads_paused: boolean;
  queued_metadata: number;
  running_metadata: number;
  queued_fingerprint: number;
  running_fingerprint: number;
  pending_obligations: number;
  total_pending: number;
  libraries: ProbeLibraryQueueSummary[];
}

export interface ProbeCancelReceipt {
  cancelled_units: number;
  cleared_obligations: number;
}

export interface FingerprintProbeSettings {
  workers: number;
  paused: boolean;
}

export interface FingerprintQueueStatus {
  paused: boolean;
  background_paused: boolean;
  remote_reads_paused: boolean;
  workers: number;
  active_workers: number;
  queued_fingerprint: number;
  running_fingerprint: number;
  queued_analysis: number;
}

export interface FingerprintCancelOptions {
  library_id?: string;
  media_id?: string;
  season?: number;
  file_id?: string;
  include_running?: boolean;
}

export interface FingerprintCancelReceipt {
  cancelled_units: number;
  aborted_running: number;
}

interface Envelope<T> {
  data: T;
}

export async function getProbeStatus(): Promise<ProbeQueueStatus> {
  return (await request<Envelope<ProbeQueueStatus>>("/probe/status")).data;
}

export async function pauseProbeQueue(): Promise<ProbeQueueStatus> {
  return (
    await request<Envelope<ProbeQueueStatus>>("/probe/pause", {
      method: "POST",
      body: JSON.stringify({}),
    })
  ).data;
}

export async function resumeProbeQueue(): Promise<ProbeQueueStatus> {
  return (
    await request<Envelope<ProbeQueueStatus>>("/probe/resume", {
      method: "POST",
      body: JSON.stringify({}),
    })
  ).data;
}

export async function cancelProbeQueue(libraryId?: string): Promise<ProbeCancelReceipt> {
  const payload = libraryId ? { library_id: libraryId } : {};
  return (
    await request<Envelope<ProbeCancelReceipt>>("/probe/cancel", {
      method: "POST",
      body: JSON.stringify(payload),
    })
  ).data;
}

export async function getFingerprintSettings(): Promise<FingerprintProbeSettings> {
  return (await request<Envelope<FingerprintProbeSettings>>("/settings/probe/fingerprint")).data;
}

export async function saveFingerprintSettings(
  settings: FingerprintProbeSettings,
): Promise<FingerprintProbeSettings> {
  return (
    await request<Envelope<FingerprintProbeSettings>>("/settings/probe/fingerprint", {
      method: "PUT",
      body: JSON.stringify(settings),
    })
  ).data;
}

export async function getFingerprintQueueStatus(): Promise<FingerprintQueueStatus> {
  return (await request<Envelope<FingerprintQueueStatus>>("/probe/fingerprint/status")).data;
}

export async function pauseFingerprintQueue(): Promise<FingerprintQueueStatus> {
  return (
    await request<Envelope<FingerprintQueueStatus>>("/probe/fingerprint/pause", {
      method: "POST",
      body: JSON.stringify({}),
    })
  ).data;
}

export async function resumeFingerprintQueue(): Promise<FingerprintQueueStatus> {
  return (
    await request<Envelope<FingerprintQueueStatus>>("/probe/fingerprint/resume", {
      method: "POST",
      body: JSON.stringify({}),
    })
  ).data;
}

export async function cancelFingerprintQueue(
  options: FingerprintCancelOptions = {},
): Promise<FingerprintCancelReceipt> {
  return (
    await request<Envelope<FingerprintCancelReceipt>>("/probe/fingerprint/cancel", {
      method: "POST",
      body: JSON.stringify(options),
    })
  ).data;
}
