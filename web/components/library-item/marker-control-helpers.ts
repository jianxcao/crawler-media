const MAX_MARKER_MS = 86_400_000;

export type ParsedSecondsInput =
  | { kind: "empty" }
  | { kind: "valid"; ms: number }
  | { kind: "invalid"; reason: string };

export interface EditDraft {
  fileId: string;
  season: number | null;
  episode: number | null;
  durationMs: number | null;
  introStart: string;
  introEnd: string;
  outroStart: string;
  outroEnd: string;
  locked: boolean;
}

export function formatMs(ms: number | null | undefined): string {
  if (ms == null || ms < 0) return "—";
  const totalSec = Math.floor(ms / 1000);
  const mins = Math.floor(totalSec / 60);
  const secs = totalSec % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

export function formatRange(
  startMs: number | null | undefined,
  endMs: number | null | undefined,
): string {
  if (startMs == null || endMs == null) return "";
  return `${formatMs(startMs)}–${formatMs(endMs)}`;
}

export function sourceBadgeLabel(
  source: string | undefined,
  locked: boolean | undefined,
): string {
  if (locked) return "人工锁定";
  if (source === "manual") return "人工设定";
  if (source === "theintrodb") return "TheIntroDB";
  if (source === "fingerprint") return "声纹";
  if (source === "chapter") return "内嵌章节";
  return source || "未知";
}

export function phaseLabel(phase: string | null | undefined): string | null {
  if (!phase) return null;
  if (phase === "probing_metadata") return "读取流信息";
  if (phase === "capturing") return "采样声纹中";
  if (phase === "analyzing") return "同季比对中";
  if (phase === "queued") return "排队等待中";
  return phase;
}

export function samplingModeLabel(mode: string | null | undefined): string | null {
  if (!mode) return null;
  if (mode === "adaptive") return "自适应采样";
  if (mode === "full_window") return "完整窗口";
  return mode;
}

export function parseSecondsInput(raw: string): ParsedSecondsInput {
  const trimmed = raw.trim();
  if (!trimmed) return { kind: "empty" };
  if (trimmed.includes(":")) {
    const parts = trimmed.split(":");
    if (parts.length !== 2 && parts.length !== 3) {
      return { kind: "invalid", reason: "时间格式无效，请填写秒数、分:秒或时:分:秒" };
    }
    const headParts = parts.slice(0, -1);
    const secPart = parts[parts.length - 1];
    if (headParts.some((p) => !/^\d+$/.test(p)) || !/^\d+(?:\.\d+)?$/.test(secPart)) {
      return { kind: "invalid", reason: "时间格式无效，请勿包含空格、负号或空段" };
    }
    const nums = parts.map(Number);
    if (nums.some((n) => !Number.isFinite(n) || n < 0)) {
      return { kind: "invalid", reason: "时间数值无效" };
    }
    if (parts.length === 2) {
      const [mm, ss] = nums;
      if (ss >= 60) return { kind: "invalid", reason: "分:秒格式中秒数必须小于 60" };
      const ms = mm * 60_000 + Math.round(ss * 1000);
      if (!Number.isFinite(ms) || ms > MAX_MARKER_MS) {
        return { kind: "invalid", reason: "时间超出有效范围（最大 24 小时）" };
      }
      return { kind: "valid", ms };
    }
    const [hh, mm, ss] = nums;
    if (mm >= 60 || ss >= 60) {
      return { kind: "invalid", reason: "时:分:秒格式中分钟与秒数必须小于 60" };
    }
    const ms = hh * 3_600_000 + mm * 60_000 + Math.round(ss * 1000);
    if (!Number.isFinite(ms) || ms > MAX_MARKER_MS) {
      return { kind: "invalid", reason: "时间超出有效范围（最大 24 小时）" };
    }
    return { kind: "valid", ms };
  }
  if (!/^\d+(?:\.\d+)?$/.test(trimmed)) {
    return { kind: "invalid", reason: "时间格式无效，请输入非负秒数或分:秒" };
  }
  const secs = Number(trimmed);
  if (!Number.isFinite(secs) || secs < 0) return { kind: "invalid", reason: "时间数值无效" };
  const ms = Math.round(secs * 1000);
  if (!Number.isFinite(ms) || ms > MAX_MARKER_MS) {
    return { kind: "invalid", reason: "时间超出有效范围（最大 24 小时）" };
  }
  return { kind: "valid", ms };
}

export function msToInputSeconds(ms: number | null | undefined): string {
  if (ms == null) return "";
  return String(ms / 1000);
}

export function validateEditDraft(editing: EditDraft):
  | { error: string }
  | {
      introStartMs: number | null;
      introEndMs: number | null;
      outroStartMs: number | null;
      outroEndMs: number | null;
    } {
  const fields = [
    ["片头起点", parseSecondsInput(editing.introStart)],
    ["片头终点", parseSecondsInput(editing.introEnd)],
    ["片尾起点", parseSecondsInput(editing.outroStart)],
    ["片尾终点", parseSecondsInput(editing.outroEnd)],
  ] as const;
  for (const [label, parsed] of fields) {
    if (parsed.kind === "invalid") return { error: `${label}：${parsed.reason}` };
  }
  const introStartMs = fields[0][1].kind === "valid" ? fields[0][1].ms : null;
  const introEndMs = fields[1][1].kind === "valid" ? fields[1][1].ms : null;
  const outroStartMs = fields[2][1].kind === "valid" ? fields[2][1].ms : null;
  const outroEndMs = fields[3][1].kind === "valid" ? fields[3][1].ms : null;

  if ((introStartMs == null) !== (introEndMs == null)) {
    return { error: "片头起止时间需要同时填写或同时留空" };
  }
  if ((outroStartMs == null) !== (outroEndMs == null)) {
    return { error: "片尾起止时间需要同时填写或同时留空" };
  }
  if (introStartMs == null && outroStartMs == null) {
    return { error: "请至少填写片头或片尾起止时间；如需移除标记请点击「清除」" };
  }
  if (introStartMs != null && introEndMs != null && introStartMs >= introEndMs) {
    return { error: "片头开始时间必须小于结束时间" };
  }
  if (outroStartMs != null && outroEndMs != null && outroStartMs >= outroEndMs) {
    return { error: "片尾开始时间必须小于结束时间" };
  }
  if (introEndMs != null && outroStartMs != null && introEndMs > outroStartMs) {
    return { error: "片头结束时间不能晚于片尾开始时间" };
  }
  if (editing.durationMs != null) {
    if (introEndMs != null && introEndMs > editing.durationMs) {
      return { error: `片头结束时间不能超过媒体总时长（${formatMs(editing.durationMs)}）` };
    }
    if (outroEndMs != null && outroEndMs > editing.durationMs) {
      return { error: `片尾结束时间不能超过媒体总时长（${formatMs(editing.durationMs)}）` };
    }
  }
  return { introStartMs, introEndMs, outroStartMs, outroEndMs };
}

export function pickDefaultSeason(seasons: number[], selectedSeason?: number): number {
  return selectedSeason ?? seasons.find((s) => s > 0) ?? seasons[0] ?? 1;
}
