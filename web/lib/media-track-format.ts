/**
 * 音轨 / 字幕 / 视频流的规格格式化与语言分组排序纯逻辑。
 */

import type { AudioStream, LibraryItemFile, SubtitleStream } from "./api/libraries.ts";
import { formatVideoResolution } from "./format.ts";

/** 音轨编码的完整读法，用在列表行的主文案上。 */
export const AUDIO_CODEC_LABELS: Record<string, string> = {
  aac: "AAC",
  ac3: "Dolby Digital",
  eac3: "Dolby Digital+",
  truehd: "Dolby TrueHD",
  dts: "DTS",
  flac: "FLAC",
  opus: "Opus",
  mp3: "MP3",
  vorbis: "Vorbis",
};

/** 同一批编码的紧凑写法，只用在列表行首那枚格式色块上（放不下完整读法）。 */
export const AUDIO_FORMAT_TOKENS: Record<string, string> = {
  aac: "AAC",
  ac3: "AC3",
  eac3: "EAC3",
  truehd: "TrueHD",
  dts: "DTS",
  flac: "FLAC",
  opus: "Opus",
  mp3: "MP3",
  vorbis: "Vorbis",
};

export const SUBTITLE_CODEC_LABELS: Record<string, string> = {
  subrip: "SRT",
  srt: "SRT",
  ass: "ASS",
  ssa: "SSA",
  hdmv_pgs_subtitle: "PGS",
  dvd_subtitle: "VobSub",
  mov_text: "Text",
  webvtt: "VTT",
  vtt: "VTT",
  sub: "VobSub",
  sup: "PGS",
};

export const LANGUAGE_LABELS: Record<string, string> = {
  chs: "简体中文",
  cht: "繁体中文",
  chi: "中文",
  zho: "中文",
  cmn: "中文",
  yue: "粤语",
  eng: "英语",
  jpn: "日语",
  kor: "韩语",
  fre: "法语",
  fra: "法语",
  ger: "德语",
  deu: "德语",
  spa: "西班牙语",
  rus: "俄语",
  ita: "意大利语",
  por: "葡萄牙语",
  tha: "泰语",
  hin: "印地语",
};

export function languageLabel(code: string | null): string | null {
  if (!code || code === "und") return null;
  return LANGUAGE_LABELS[code.toLowerCase()] ?? code;
}

const LANGUAGE_RANK: Record<string, number> = {
  chs: 1,
  cht: 2,
  chi: 3,
  zho: 3,
  cmn: 3,
  yue: 4,
  eng: 5,
  jpn: 6,
  kor: 7,
};
const OTHER_LANGUAGE_RANK = 40;
const BILINGUAL_RANK = 80;
const UNKNOWN_LANGUAGE_RANK = 90;
const UNKNOWN_LANGUAGE = "未标语言";

export function languageRank(code: string | null): number {
  if (!code || code === "und") return UNKNOWN_LANGUAGE_RANK;
  return LANGUAGE_RANK[code.toLowerCase()] ?? OTHER_LANGUAGE_RANK;
}

/** 声道数 → 惯用布局标签（channel_layout 可用时优先，去掉 (side) 等后缀）。 */
export function channelsLabel(stream: AudioStream): string | null {
  const layout = stream.channel_layout?.split("(")[0]?.trim();
  if (layout && /^\d/.test(layout)) return layout;
  const map: Record<number, string> = { 1: "单声道", 2: "2.0", 6: "5.1", 7: "6.1", 8: "7.1" };
  if (stream.channels == null) return null;
  return map[stream.channels] ?? `${stream.channels} 声道`;
}

const GENERIC_PROFILES = new Set(["lc", "main", "high", "baseline", "main 10"]);

export function informativeProfile(stream: AudioStream): string | null {
  if (!stream.profile) return null;
  return GENERIC_PROFILES.has(stream.profile.toLowerCase()) ? null : stream.profile;
}

/** 音轨 → 列表行主文案：格式（有信息量的 profile 优先）· 声道。 */
export function audioSpecLabel(stream: AudioStream): string {
  const codec =
    informativeProfile(stream) ||
    (stream.codec
      ? (AUDIO_CODEC_LABELS[stream.codec.toLowerCase()] ?? stream.codec.toUpperCase())
      : "未知格式");
  const sample = stream.sample_rate
    ? `${(Number(stream.sample_rate) / 1000).toFixed(1)}kHz`
    : null;
  return [codec, channelsLabel(stream), sample].filter(Boolean).join(" · ");
}

const AUDIO_CODEC_TIER: Record<string, number> = {
  truehd: 5,
  dts: 4,
  eac3: 3,
  flac: 3,
  ac3: 2,
  opus: 1,
  aac: 1,
  vorbis: 0,
  mp3: 0,
};

export function topAudioSpec(streams: AudioStream[]): string | null {
  if (streams.length < 2) return null;
  let best: AudioStream | null = null;
  let bestScore = -1;
  for (const stream of streams) {
    const tier = AUDIO_CODEC_TIER[stream.codec?.toLowerCase() ?? ""] ?? 0;
    const score = tier * 100 + (stream.channels ?? 0);
    if (score > bestScore) {
      bestScore = score;
      best = stream;
    }
  }
  if (!best) return null;
  const codec =
    informativeProfile(best) ||
    (best.codec ? (AUDIO_CODEC_LABELS[best.codec.toLowerCase()] ?? best.codec.toUpperCase()) : null);
  return [codec, channelsLabel(best)].filter(Boolean).join(" ") || null;
}

/** AI 生成字幕的标题里编了目标语言，翻译回中文读法（双语保留两个语言）。 */
export function generatedSubtitleLabel(stream: SubtitleStream): string | null {
  const title = stream.title?.toLowerCase();
  if (!title) return null;
  if (title.startsWith("ai-bilingual-")) {
    const languages = title.slice("ai-bilingual-".length).split("-");
    if (languages.length === 2) {
      return `${languageLabel(languages[0]) ?? languages[0]} + ${languageLabel(languages[1]) ?? languages[1]}`;
    }
  }
  if (title === "ai-chs") return "简体中文";
  if (title === "ai-cht") return "繁体中文";
  return null;
}

/** 字幕 → 分组用的语言名（AI 产物的语言写在标题里，优先认它）。 */
export function subtitleLanguageName(stream: SubtitleStream): string {
  return (
    generatedSubtitleLabel(stream) ??
    languageLabel(stream.language) ??
    stream.title?.trim() ??
    UNKNOWN_LANGUAGE
  );
}

export function subtitleFormatToken(stream: SubtitleStream): string {
  if (!stream.codec) return "字幕";
  return SUBTITLE_CODEC_LABELS[stream.codec.toLowerCase()] ?? stream.codec.toUpperCase();
}

/** 字幕色块只承担格式识别，颜色刻意保持低饱和，避免重新变成徽章墙。 */
export const TrackTone = {
  Srt: "srt",
  Ass: "ass",
  Pgs: "pgs",
  VobSub: "vobsub",
  Text: "text",
  Ai: "ai",
  Other: "other",
} as const;
export type TrackTone = (typeof TrackTone)[keyof typeof TrackTone];

export const TRACK_TONE_CLASS: Record<TrackTone, string> = {
  [TrackTone.Srt]: "bg-sky-300/[0.1] text-sky-100/90",
  [TrackTone.Ass]: "bg-violet-300/[0.1] text-violet-100/90",
  [TrackTone.Pgs]: "bg-amber-300/[0.1] text-amber-100/90",
  [TrackTone.VobSub]: "bg-orange-300/[0.1] text-orange-100/90",
  [TrackTone.Text]: "bg-cyan-300/[0.1] text-cyan-100/90",
  [TrackTone.Ai]: "bg-[#cdd6e6]/[0.12] text-[#eef2f8]/90",
  [TrackTone.Other]: "bg-white/[0.065] text-white/75",
};

export function isAiSubtitle(stream: SubtitleStream): boolean {
  const title = stream.title?.toLowerCase() ?? "";
  const fileTokens = stream.file_name?.toLowerCase().split(".") ?? [];
  return (
    title.startsWith("ai-") || fileTokens.some((token) => token === "ai" || token.startsWith("ai-"))
  );
}

export function subtitleTone(stream: SubtitleStream): TrackTone {
  if (isAiSubtitle(stream)) return TrackTone.Ai;
  switch (stream.codec?.toLowerCase()) {
    case "subrip":
    case "srt":
      return TrackTone.Srt;
    case "ass":
    case "ssa":
      return TrackTone.Ass;
    case "hdmv_pgs_subtitle":
    case "sup":
      return TrackTone.Pgs;
    case "dvd_subtitle":
    case "sub":
      return TrackTone.VobSub;
    case "mov_text":
    case "webvtt":
    case "vtt":
      return TrackTone.Text;
    default:
      return TrackTone.Other;
  }
}

export function audioTone(stream: AudioStream): TrackTone {
  switch (stream.codec?.toLowerCase()) {
    case "aac":
    case "mp3":
      return TrackTone.Srt;
    case "truehd":
      return TrackTone.Ass;
    case "ac3":
    case "eac3":
      return TrackTone.Pgs;
    case "dts":
      return TrackTone.VobSub;
    case "flac":
    case "opus":
    case "vorbis":
      return TrackTone.Text;
    default:
      return TrackTone.Other;
  }
}

export function externalSubtitleSuffix(stream: SubtitleStream): string {
  const tokens = stream.file_name?.toLowerCase().split(".") ?? [];
  if (tokens.some((token) => token === "ai" || token.startsWith("ai-"))) return "AI 外挂";
  if (tokens.includes("pgs-ocr")) return "PGS 转换";
  return "外挂";
}

export function externalSubtitleLabel(fileName: string | null, videoStem: string): string {
  if (!fileName) return "外挂字幕";
  if (videoStem && fileName.toLowerCase().startsWith(videoStem.toLowerCase())) {
    const rest = fileName.slice(videoStem.length).replace(/^\./, "");
    if (rest) return rest;
  }
  return fileName;
}

export function subtitleTrackRef(stream: SubtitleStream, index: number): string | null {
  if (!stream.external) return `embedded:${index}`;
  return stream.file_name ? `external:${stream.file_name}` : null;
}

export interface TrackEntry {
  key: string;
  language: string;
  rank: number;
  format: string;
  tone: TrackTone;
  primary: string;
  secondary: string;
  flags: string[];
  external: boolean | null;
  order: number;
  deletable: string | null;
  preview: { stream: SubtitleStream; track: string; label: string } | null;
}

export function audioEntries(streams: AudioStream[]): TrackEntry[] {
  return streams.map((stream, index) => {
    const spec = audioSpecLabel(stream);
    const title = stream.title?.trim();
    return {
      key: `audio:${index}`,
      language: languageLabel(stream.language) ?? UNKNOWN_LANGUAGE,
      rank: languageRank(stream.language),
      format: stream.codec
        ? (AUDIO_FORMAT_TOKENS[stream.codec.toLowerCase()] ?? stream.codec.toUpperCase())
        : "音轨",
      tone: audioTone(stream),
      primary: title ? `${spec} · ${title}` : spec,
      secondary: `轨 ${index + 1}`,
      flags: stream.default ? ["默认"] : [],
      external: null,
      order: stream.default ? 0 : 1,
      deletable: null,
      preview: null,
    };
  });
}

const VIDEO_CODEC_LABELS: Record<string, string> = {
  h264: "H.264",
  hevc: "H.265",
  h265: "H.265",
  av1: "AV1",
  vp9: "VP9",
  vp8: "VP8",
  mpeg2video: "MPEG-2",
  mpeg4: "MPEG-4",
  xvid: "Xvid",
};

export function formatBits(bits: number): string {
  if (bits >= 1_000_000) return `${(bits / 1_000_000).toFixed(1)} Mbps`;
  if (bits >= 1_000) return `${(bits / 1_000).toFixed(0)} Kbps`;
  return `${bits} bps`;
}

export function videoTrackEntries(file: LibraryItemFile | null | undefined): TrackEntry[] {
  const v = file?.video_track;
  if (file && !file.probed && !v) {
    return [
      {
        key: "video:pending",
        language: "视频",
        rank: 0,
        format: "…",
        tone: TrackTone.Other,
        primary: "正在后台探测媒体流信息（首次访问稍候自动刷新）",
        secondary: "",
        flags: [],
        external: null,
        order: 0,
        deletable: null,
        preview: null,
      },
    ];
  }
  if (!v) return [];
  const res = file?.resolution
    ? formatVideoResolution(file.resolution)
    : v.width && v.height
      ? `${v.width}×${v.height}`
      : null;
  const parts = [
    v.codec ? (VIDEO_CODEC_LABELS[v.codec.toLowerCase()] ?? v.codec.toUpperCase()) : null,
    res,
    v.frame_rate ? `${Math.round(v.frame_rate * 100) / 100}fps` : null,
    v.bit_rate ? formatBits(v.bit_rate) : null,
    v.duration_secs ? `${Math.round(v.duration_secs / 60)} 分钟` : null,
  ].filter((x): x is string => Boolean(x));
  if (parts.length === 0) return [];
  return [
    {
      key: "video:0",
      language: "视频",
      rank: 0,
      format: parts[0] ?? "视频流",
      tone: TrackTone.Other,
      primary: parts.join(" · "),
      secondary: file?.quality_source ? `片源 ${file.quality_source}` : "",
      flags: file?.hdr ? [file.hdr] : [],
      external: null,
      order: 0,
      deletable: null,
      preview: null,
    },
  ];
}

export function subtitleEntries(streams: SubtitleStream[], videoStem: string): TrackEntry[] {
  let embeddedOrdinal = 0;
  return streams.map((stream, index) => {
    const language = subtitleLanguageName(stream);
    const format = subtitleFormatToken(stream);
    const external = stream.external;
    if (!external) embeddedOrdinal += 1;
    const track = subtitleTrackRef(stream, index);
    const label = [language, format].filter(Boolean).join(" · ");
    return {
      key: `subtitle:${track ?? `unknown:${index}`}`,
      language,
      rank: subtitleRank(stream, language),
      format,
      tone: subtitleTone(stream),
      primary: external
        ? externalSubtitleLabel(stream.file_name, videoStem)
        : `内封轨 ${embeddedOrdinal}`,
      secondary: external ? externalSubtitleSuffix(stream) : "内封",
      flags: [stream.default ? "默认" : null, stream.forced ? "强制" : null].filter(
        (flag): flag is string => flag !== null,
      ),
      external,
      order: stream.default ? 0 : isAiSubtitle(stream) ? 3 : external ? 2 : 1,
      deletable: external ? (stream.file_name ?? null) : null,
      preview: track ? { stream, track, label } : null,
    };
  });
}

export function subtitleRank(stream: SubtitleStream, language: string): number {
  if (language.includes(" + ")) return BILINGUAL_RANK;
  if (language === UNKNOWN_LANGUAGE) return UNKNOWN_LANGUAGE_RANK;
  const byCode = languageRank(stream.language);
  if (byCode !== UNKNOWN_LANGUAGE_RANK) return byCode;
  const code = Object.keys(LANGUAGE_LABELS).find((key) => LANGUAGE_LABELS[key] === language);
  return code ? languageRank(code) : OTHER_LANGUAGE_RANK;
}

export interface LanguageGroup {
  language: string;
  entries: TrackEntry[];
  rank: number;
  hasDefault: boolean;
}

/**
 * 按语言分组并排序：默认轨所在语言 → 简中 → 繁中 → 中文/粤语 → 其余按条数降序 → 双语与未标语言垫底。
 */
export function groupByLanguage(entries: TrackEntry[]): LanguageGroup[] {
  const buckets = new Map<string, TrackEntry[]>();
  for (const entry of entries) {
    const bucket = buckets.get(entry.language);
    if (bucket) bucket.push(entry);
    else buckets.set(entry.language, [entry]);
  }
  const groups = [...buckets].map(([language, list]) => ({
    language,
    entries: [...list].sort((a, b) => a.order - b.order),
    rank: Math.min(...list.map((entry) => entry.rank)),
    hasDefault: list.some((entry) => entry.order === 0),
  }));
  groups.sort(
    (a, b) =>
      Number(b.hasDefault) - Number(a.hasDefault) ||
      a.rank - b.rank ||
      b.entries.length - a.entries.length ||
      a.language.localeCompare(b.language, "zh-Hans-CN"),
  );
  return groups;
}

/** 从文件路径取文件名（新契约的文件只有 path，没有 file_name）。 */
export function fileNameOf(file: LibraryItemFile): string {
  const cut = Math.max(file.path.lastIndexOf("/"), file.path.lastIndexOf("\\"));
  return cut < 0 ? file.path : file.path.slice(cut + 1);
}
