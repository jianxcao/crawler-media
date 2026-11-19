import type { LibraryItemFile } from "@/lib/api/libraries";

import { formatVideoResolution } from "./format.ts";

export const VIDEO_CODEC_LABELS: Record<string, string> = {
  hevc: "HEVC",
  h264: "H.264",
  h265: "HEVC",
  av1: "AV1",
  vc1: "VC-1",
  mpeg2video: "MPEG-2",
  vp9: "VP9",
};

export function videoCodecLabel(codec: string | null): string | null {
  if (!codec) return null;
  return VIDEO_CODEC_LABELS[codec.toLowerCase()] ?? codec.toUpperCase();
}

export function fileResolutionLabel(file: LibraryItemFile | null | undefined): string | null {
  if (!file) return null;
  if (file.resolution) return formatVideoResolution(file.resolution);
  if (file.video_track?.width && file.video_track?.height) {
    const h = file.video_track.height;
    const w = file.video_track.width;
    if (w >= 3800 || h >= 2100) return "4K";
    if (h >= 1000) return "1080p";
    if (h >= 700) return "720p";
    return `${h}p`;
  }
  return null;
}

export function fileHdrLabel(file: LibraryItemFile | null | undefined): string | null {
  return file?.hdr ?? null;
}

export function fileCodecLabel(file: LibraryItemFile | null | undefined): string | null {
  if (!file) return null;
  const codec = file.codec || file.video_track?.codec || null;
  return videoCodecLabel(codec);
}

const HDR_FORMAT_PRIORITY = ["Dolby Vision", "HDR10+", "HDR10", "HLG", "HDR"];

export function sortHdrFormats(formats: Iterable<string>): string[] {
  return [...new Set(formats)].sort((a, b) => {
    const aIndex = HDR_FORMAT_PRIORITY.indexOf(a);
    const bIndex = HDR_FORMAT_PRIORITY.indexOf(b);
    return (aIndex < 0 ? HDR_FORMAT_PRIORITY.length : aIndex) -
      (bIndex < 0 ? HDR_FORMAT_PRIORITY.length : bIndex);
  });
}

/** 从完整文件路径提取文件名（新契约的文件只有 path，没有 file_name）。 */
export function fileNameOf(filePath: string): string {
  const cut = Math.max(filePath.lastIndexOf("/"), filePath.lastIndexOf("\\"));
  return cut < 0 ? filePath : filePath.slice(cut + 1);
}

/** 从完整文件路径提取保存目录，同时兼容 NAS/POSIX 与 Windows 分隔符。 */
export function fileDirectory(filePath: string): string {
  const normalized = filePath.replace(/[\\/]+$/, "");
  const separatorIndex = Math.max(
    normalized.lastIndexOf("/"),
    normalized.lastIndexOf("\\"),
  );
  if (separatorIndex < 0) return "—";
  if (separatorIndex === 0) return normalized.slice(0, 1);
  return normalized.slice(0, separatorIndex);
}

/** 季号 → 展示名（0 是特别篇，TMDB 的 specials 惯例）。 */
export function seasonLabel(season: number, owned: boolean): string {
  const name = season === 0 ? "特别篇" : `第 ${season} 季`;
  return owned ? name : `${name} · 未入库`;
}
