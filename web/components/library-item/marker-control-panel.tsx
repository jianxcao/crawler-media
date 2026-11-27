"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import {
  deleteManualMarker,
  detectItemMarkers,
  fetchSeasonMarkers,
  saveManualMarker,
  type MarkerDetectMode,
  type SeasonEpisodeMarkerRow,
  type SeasonMarkersDiagnostics,
} from "@/lib/api/libraries";
import { cancelFingerprintQueue } from "@/lib/api/probe";

function formatMs(ms: number | null | undefined): string {
  if (ms == null || ms < 0) return "—";
  const totalSec = Math.floor(ms / 1000);
  const mins = Math.floor(totalSec / 60);
  const secs = totalSec % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

function formatRange(startMs: number | null | undefined, endMs: number | null | undefined): string {
  if (startMs == null || endMs == null) return "";
  return `${formatMs(startMs)}–${formatMs(endMs)}`;
}

function sourceBadgeLabel(source: string | undefined, locked: boolean | undefined): string {
  if (locked || source === "manual") return "人工锁定";
  if (source === "theintrodb") return "TheIntroDB";
  if (source === "fingerprint") return "声纹";
  if (source === "chapter") return "内嵌章节";
  return source || "未知";
}

function parseSecondsInput(raw: string): number | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (trimmed.includes(":")) {
    const parts = trimmed.split(":").map((p) => Number(p.trim()));
    if (parts.some((n) => !Number.isFinite(n) || n < 0)) return null;
    if (parts.length === 2) return Math.round((parts[0] * 60 + parts[1]) * 1000);
    if (parts.length === 3) {
      return Math.round((parts[0] * 3600 + parts[1] * 60 + parts[2]) * 1000);
    }
    return null;
  }
  const secs = Number(trimmed);
  if (!Number.isFinite(secs) || secs < 0) return null;
  return Math.round(secs * 1000);
}

function msToInputSeconds(ms: number | null | undefined): string {
  if (ms == null) return "";
  return String(Math.round(ms / 100) / 10);
}

interface EditDraft {
  fileId: string;
  season: number;
  episode: number;
  introStart: string;
  introEnd: string;
  outroStart: string;
  outroEnd: string;
}

export function MarkerControlPanel({
  libraryId,
  mediaItemId,
  seasons,
  selectedSeason,
  selectedEpisode,
  selectedFileId,
  onChanged,
}: {
  libraryId: string;
  mediaItemId: string;
  seasons: number[];
  selectedSeason?: number;
  selectedEpisode?: number;
  selectedFileId?: string | null;
  onChanged?: () => void;
}) {
  const initialSeason = selectedSeason ?? seasons[0] ?? 1;
  const [season, setSeason] = useState<number>(initialSeason);
  const [mode, setMode] = useState<MarkerDetectMode>("reuse_valid");
  const [diag, setDiag] = useState<SeasonMarkersDiagnostics | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<EditDraft | null>(null);
  const activeRef = useRef(true);

  useEffect(() => {
    if (selectedSeason != null) setSeason(selectedSeason);
  }, [selectedSeason]);

  const load = useCallback(async () => {
    try {
      const next = await fetchSeasonMarkers(libraryId, mediaItemId, season);
      if (activeRef.current) setDiag(next);
    } catch (cause) {
      if (activeRef.current) {
        setError(`读取片头片尾诊断失败：${(cause as Error).message}`);
      }
    }
  }, [libraryId, mediaItemId, season]);

  useEffect(() => {
    activeRef.current = true;
    void load();
    return () => {
      activeRef.current = false;
    };
  }, [load]);

  useEffect(() => {
    if (!diag?.active) return;
    const timer = setInterval(() => {
      void load();
    }, 2000);
    return () => clearInterval(timer);
  }, [diag?.active, load]);

  const runDetect = async (singleEpisode: boolean) => {
    if (busy) return;
    setBusy(singleEpisode ? "episode" : "season");
    setError(null);
    setMessage(null);
    try {
      const payload = singleEpisode && selectedEpisode != null
        ? {
            season,
            episode: selectedEpisode,
            ...(selectedFileId ? { file_id: selectedFileId } : {}),
            mode,
          }
        : { season, mode };
      const receipt = await detectItemMarkers(libraryId, mediaItemId, payload);
      if (!activeRef.current) return;
      if (receipt.already_running) {
        setMessage("该季已有片头片尾任务正在运行。");
      } else {
        const skipNote =
          receipt.skipped_multi_unit > 0
            ? `（已自动跳过 ${receipt.skipped_multi_unit} 个合并集文件）`
            : "";
        setMessage(`已提交片头片尾检测任务${skipNote}。`);
      }
      await load();
      onChanged?.();
    } catch (cause) {
      if (activeRef.current) {
        setError(`提交检测失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const handleCancel = async () => {
    if (busy) return;
    setBusy("cancel");
    setError(null);
    setMessage(null);
    try {
      const receipt = await cancelFingerprintQueue({
        media_id: mediaItemId,
        season,
        include_running: true,
      });
      if (!activeRef.current) return;
      setMessage(
        `已停止当前季检测（取消 ${receipt.cancelled_units} 项，中断 ${receipt.aborted_running} 项在途读取）。`,
      );
      await load();
      onChanged?.();
    } catch (cause) {
      if (activeRef.current) {
        setError(`取消检测失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const beginEdit = (row: SeasonEpisodeMarkerRow) => {
    setEditing({
      fileId: row.file_id,
      season: row.season,
      episode: row.episode,
      introStart: msToInputSeconds(row.marker?.intro_start_ms),
      introEnd: msToInputSeconds(row.marker?.intro_end_ms),
      outroStart: msToInputSeconds(row.marker?.outro_start_ms),
      outroEnd: msToInputSeconds(row.marker?.outro_end_ms),
    });
    setError(null);
  };

  const submitManualEdit = async () => {
    if (!editing || busy) return;
    const introStartMs = parseSecondsInput(editing.introStart);
    const introEndMs = parseSecondsInput(editing.introEnd);
    const outroStartMs = parseSecondsInput(editing.outroStart);
    const outroEndMs = parseSecondsInput(editing.outroEnd);
    if ((introStartMs == null) !== (introEndMs == null)) {
      setError("片头起止时间需要同时填写或同时留空");
      return;
    }
    if ((outroStartMs == null) !== (outroEndMs == null)) {
      setError("片尾起止时间需要同时填写或同时留空");
      return;
    }
    setBusy("manual");
    setError(null);
    try {
      await saveManualMarker(libraryId, mediaItemId, {
        file_id: editing.fileId,
        season: editing.season,
        episode: editing.episode,
        intro_start_ms: introStartMs,
        intro_end_ms: introEndMs,
        outro_start_ms: outroStartMs,
        outro_end_ms: outroEndMs,
        locked: true,
      });
      if (!activeRef.current) return;
      setEditing(null);
      setMessage(`已保存并锁定第 ${editing.episode} 集片头片尾时间。`);
      await load();
      onChanged?.();
    } catch (cause) {
      if (activeRef.current) {
        setError(`保存人工标记失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const handleDeleteMarker = async (row: SeasonEpisodeMarkerRow) => {
    if (busy) return;
    setBusy("delete");
    setError(null);
    try {
      await deleteManualMarker(libraryId, mediaItemId, {
        file_id: row.file_id,
        season: row.season,
        episode: row.episode,
      });
      if (!activeRef.current) return;
      setMessage(`已清除第 ${row.episode} 集片头片尾标记。`);
      await load();
      onChanged?.();
    } catch (cause) {
      if (activeRef.current) {
        setError(`清除标记失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  return (
    <div className="mt-3 rounded-2xl border border-white/[0.08] bg-white/[0.03] p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2 text-caption">
          {seasons.length > 1 && (
            <select
              aria-label="检测季选择"
              value={season}
              onChange={(e) => setSeason(Number(e.target.value))}
              className="rounded-lg border border-white/10 bg-black/40 px-2.5 py-1 text-white/85"
            >
              {seasons.map((s) => (
                <option key={s} value={s}>
                  第 {s} 季
                </option>
              ))}
            </select>
          )}
          <select
            aria-label="片头片尾检测策略"
            value={mode}
            onChange={(e) => setMode(e.target.value as MarkerDetectMode)}
            className="rounded-lg border border-white/10 bg-black/40 px-2.5 py-1 text-white/85"
          >
            <option value="reuse_valid">复用缓存重算（快速本地比对）</option>
            <option value="recapture">重新采样声纹（覆盖旧音频缓存）</option>
            <option value="theintrodb_only">仅同步 TheIntroDB 云端</option>
          </select>
        </div>

        <div className="flex flex-wrap items-center gap-1.5 text-caption">
          <button
            type="button"
            disabled={busy != null}
            onClick={() => void runDetect(false)}
            className="rounded-lg bg-sky-500/20 px-3 py-1 font-medium text-sky-200 transition hover:bg-sky-500/30 disabled:opacity-40"
          >
            检测本季
          </button>
          {selectedEpisode != null && (
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void runDetect(true)}
              className="rounded-lg border border-white/12 bg-white/[0.06] px-3 py-1 text-white/85 transition hover:bg-white/[0.12] disabled:opacity-40"
            >
              {`检测第 ${selectedEpisode} 集`}
            </button>
          )}
          {diag?.active && (
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void handleCancel()}
              className="rounded-lg border border-red-400/25 bg-red-500/15 px-3 py-1 text-red-200 transition hover:bg-red-500/25 disabled:opacity-40"
            >
              取消当前检测
            </button>
          )}
          <button
            type="button"
            disabled={busy != null}
            onClick={() => void load()}
            className="rounded-lg border border-white/10 px-2.5 py-1 text-white/60 transition hover:bg-white/[0.06] hover:text-white/90"
          >
            刷新诊断
          </button>
        </div>
      </div>

      {diag?.job && (
        <div className="mt-2.5 flex flex-wrap items-center gap-3 rounded-xl border border-white/[0.06] bg-black/20 px-3 py-2 text-caption text-white/70">
          <span>
            最近任务：{diag.job.status === "running" || diag.job.status === "queued" ? "进行中" : diag.job.status === "succeeded" ? "已完成" : diag.job.status === "cancelled" ? "已取消" : "失败"}
          </span>
          {diag.job.phase && <span>阶段：{diag.job.phase}</span>}
          {diag.job.sampling_mode && <span>模式：{diag.job.sampling_mode}</span>}
          <span>
            进度：{diag.job.completed}/{diag.job.total}（成功 {diag.job.succeeded}
            {diag.job.failed > 0 ? `，失败 ${diag.job.failed}` : ""}）
          </span>
        </div>
      )}

      {message && (
        <p role="status" className="mt-2 text-caption text-emerald-300/85">
          {message}
        </p>
      )}
      {error && (
        <p role="alert" className="mt-2 text-caption text-amber-300/90">
          {error}
        </p>
      )}

      {editing && (
        <div className="mt-3 rounded-xl border border-sky-400/25 bg-sky-500/[0.06] p-3 text-caption">
          <p className="font-medium text-sky-200">
            手工编辑并锁定第 {editing.episode} 集（支持填秒数如 90 或分:秒如 01:30，留空表示无）
          </p>
          <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
            <label className="text-white/70">
              片头起
              <input
                type="text"
                aria-label="片头起点"
                value={editing.introStart}
                onChange={(e) => setEditing({ ...editing, introStart: e.target.value })}
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片头止
              <input
                type="text"
                aria-label="片头终点"
                value={editing.introEnd}
                onChange={(e) => setEditing({ ...editing, introEnd: e.target.value })}
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片尾起
              <input
                type="text"
                aria-label="片尾起点"
                value={editing.outroStart}
                onChange={(e) => setEditing({ ...editing, outroStart: e.target.value })}
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片尾止
              <input
                type="text"
                aria-label="片尾终点"
                value={editing.outroEnd}
                onChange={(e) => setEditing({ ...editing, outroEnd: e.target.value })}
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
          </div>
          <div className="mt-2.5 flex items-center gap-2">
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void submitManualEdit()}
              className="rounded-lg bg-sky-500/25 px-3 py-1 font-medium text-sky-100 hover:bg-sky-500/35"
            >
              保存锁定
            </button>
            <button
              type="button"
              onClick={() => setEditing(null)}
              className="rounded-lg border border-white/15 px-3 py-1 text-white/70 hover:bg-white/10"
            >
              取消
            </button>
          </div>
        </div>
      )}

      {diag && diag.episodes.length > 0 && (
        <div className="mt-3 max-h-72 overflow-y-auto rounded-xl border border-white/[0.06]">
          <table className="w-full text-left text-caption text-white/75">
            <thead className="border-b border-white/[0.06] bg-white/[0.02] text-white/50">
              <tr>
                <th className="px-3 py-1.5">集</th>
                <th className="px-3 py-1.5">片头</th>
                <th className="px-3 py-1.5">片尾</th>
                <th className="px-3 py-1.5">来源</th>
                <th className="px-3 py-1.5 text-right">操作</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-white/[0.04]">
              {diag.episodes.map((row) => {
                const introRange = formatRange(
                  row.marker?.intro_start_ms,
                  row.marker?.intro_end_ms,
                );
                const outroRange = formatRange(
                  row.marker?.outro_start_ms,
                  row.marker?.outro_end_ms,
                );
                return (
                  <tr key={`${row.file_id}:${row.episode}`}>
                    <td className="px-3 py-1.5 font-medium text-white/85">
                      E{String(row.episode).padStart(2, "0")}
                    </td>
                    <td className="px-3 py-1.5">
                      {introRange || (
                        <span className="text-white/45">
                          {row.multi_unit
                            ? "合并文件跳过"
                            : row.detection?.intro?.reason_label ?? "未识别"}
                        </span>
                      )}
                    </td>
                    <td className="px-3 py-1.5">
                      {outroRange || (
                        <span className="text-white/45">
                          {row.multi_unit
                            ? "合并文件跳过"
                            : row.detection?.outro?.reason_label ?? "未识别"}
                        </span>
                      )}
                    </td>
                    <td className="px-3 py-1.5">
                      {row.marker ? (
                        <span className="rounded bg-white/[0.08] px-1.5 py-0.5 text-micro text-white/80">
                          {sourceBadgeLabel(row.marker.source, row.marker.locked)}
                        </span>
                      ) : (
                        <span className="text-white/35">—</span>
                      )}
                    </td>
                    <td className="px-3 py-1.5 text-right">
                      {!row.multi_unit && (
                        <div className="inline-flex items-center gap-1.5">
                          <button
                            type="button"
                            onClick={() => beginEdit(row)}
                            className="text-sky-300 hover:underline"
                          >
                            编辑/锁定
                          </button>
                          {row.marker && (
                            <button
                              type="button"
                              onClick={() => void handleDeleteMarker(row)}
                              className="text-white/50 hover:text-red-300"
                            >
                              清除
                            </button>
                          )}
                        </div>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
