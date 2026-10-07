export interface DownloaderCapabilities {
  speed: boolean;
  queue: boolean;
  alt_speed: boolean;
}

export const DOWNLOADER_LIMITS_CAPABILITIES: DownloaderCapabilities = {
  speed: true,
  queue: false,
  alt_speed: false,
};

export function editableLimitFields(caps: DownloaderCapabilities): string[] {
  const fields: string[] = [];
  if (caps.speed) {
    fields.push("download_limit_bytes", "upload_limit_bytes");
  }
  if (caps.alt_speed) {
    fields.push("alt_speed_enabled");
  }
  if (caps.queue) {
    fields.push("queue_enabled", "max_active_downloads", "max_active_uploads", "max_active_torrents");
  }
  return fields;
}
