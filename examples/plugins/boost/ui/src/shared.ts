export interface BoostTaskDto {
  handle: string;
  task_id: string;
  quota_key: string;
  downloader_id: string;
  site_id?: string | null;
  info_hash: string;
  state: string;
  task_bytes: number;
  retained_bytes: number;
  uploaded_bytes: number;
  hr_protected?: boolean | null;
  hold_until?: number | null;
  free_until?: number | null;
  protection_state: string;
  transfer_active: boolean;
  library_referenced: boolean;
  observed_at: number;
  revision: number;
}

export interface BoostReservationDto {
  handle: string;
  operation_id: string;
  quota_key: string;
  downloader_id: string;
  site_id?: string | null;
  info_hash?: string | null;
  reserved_bytes: number;
  stage: string;
  expires_at?: number | null;
  revision: number;
}

export interface BoostFullSnapshot {
  revision: string | number;
  used_bytes: number | null;
  reserved_bytes: number | null;
  retained_file_bytes: number | null;
  reservations?: BoostReservationDto[];
  tasks?: BoostTaskDto[];
  disk: {
    available_bytes?: number | null;
    observed_at?: string | number | null;
    status: string;
  };
}

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return "-";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let val = bytes / 1024;
  let idx = 0;
  while (val >= 1024 && idx < units.length - 1) {
    val /= 1024;
    idx += 1;
  }
  return `${val.toFixed(val >= 100 ? 0 : 2)} ${units[idx]}`;
}

export function protectionTone(state: string): "ok" | "warn" | "danger" | "neutral" {
  switch (state) {
    case "eligible":
      return "ok";
    case "hr-protected":
    case "hold-protected":
    case "active-transfer":
    case "library-referenced":
      return "warn";
    case "blocked":
      return "danger";
    default:
      return "neutral";
  }
}
