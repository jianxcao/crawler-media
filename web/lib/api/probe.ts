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
