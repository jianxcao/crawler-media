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

import {
  type EditDraft,
  formatRange,
  msToInputSeconds,
  parseSecondsInput,
  phaseLabel,
  pickDefaultSeason,
  samplingModeLabel,
  sourceBadgeLabel,
  validateEditDraft,
} from "./marker-control-helpers";

export { msToInputSeconds, parseSecondsInput };

export function MarkerControlPanel({
  libraryId,
  mediaItemId,
  seasons,
  isTv = true,
  selectedSeason,
  selectedEpisode,
  selectedFileId,
  onChanged,
}: {
  libraryId: string;
  mediaItemId: string;
  seasons: number[];
  isTv?: boolean;
  selectedSeason?: number;
  selectedEpisode?: number;
  selectedFileId?: string | null;
  onChanged?: () => void;
}) {
  const [season, setSeason] = useState<number>(() => pickDefaultSeason(seasons, selectedSeason));
  const [mode, setMode] = useState<MarkerDetectMode>(() =>
    isTv ? "reuse_valid" : "theintrodb_only",
  );
  const [diag, setDiag] = useState<SeasonMarkersDiagnostics | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<EditDraft | null>(null);

  const activeRef = useRef(true);
  const prevActiveRef = useRef(false);
  const loadSeqRef = useRef(0);
  const loadAbortRef = useRef<AbortController | null>(null);
  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;
  const seasonsRef = useRef(seasons);
  seasonsRef.current = seasons;
  const prevExternalRef = useRef({ isTv, libraryId, mediaItemId, selectedSeason });

  const effectiveMode: MarkerDetectMode = isTv ? mode : "theintrodb_only";
  const canDetectSingleEpisode = isTv && selectedSeason === season && selectedEpisode != null;

  useEffect(() => {
    const prev = prevExternalRef.current;
    prevExternalRef.current = { isTv, libraryId, mediaItemId, selectedSeason };
    if (!isTv) return;
    const itemChanged =
      prev.libraryId !== libraryId || prev.mediaItemId !== mediaItemId || prev.isTv !== isTv;
    const selectedSeasonChanged = prev.selectedSeason !== selectedSeason;
    if (selectedSeason != null && (itemChanged || selectedSeasonChanged)) {
      setSeason(selectedSeason);
    } else if (itemChanged) {
      setSeason(pickDefaultSeason(seasonsRef.current, selectedSeason));
    }
  }, [isTv, libraryId, mediaItemId, selectedSeason]);

  useEffect(() => {
    if (!isTv || seasons.length === 0) return;
    setSeason((current) =>
      seasons.includes(current) ? current : pickDefaultSeason(seasons),
    );
  }, [isTv, seasons]);

  useEffect(() => {
    setEditing(null);
  }, [mediaItemId, season, isTv]);

  const load = useCallback(async () => {
    loadAbortRef.current?.abort();
    const controller = new AbortController();
    loadAbortRef.current = controller;
    const reqId = ++loadSeqRef.current;
    const targetSeason = isTv ? season : null;
    try {
      const next = await fetchSeasonMarkers(libraryId, mediaItemId, targetSeason, controller.signal);
      if (!activeRef.current || controller.signal.aborted || reqId !== loadSeqRef.current) return;
      const finishedActive = prevActiveRef.current && !next.active;
      prevActiveRef.current = next.active;
      setDiag(next);
      setError(null);
      if (finishedActive) onChangedRef.current?.();
    } catch (cause) {
      if (!activeRef.current || controller.signal.aborted || reqId !== loadSeqRef.current) return;
      setError(`读取片头片尾诊断失败：${(cause as Error).message}`);
    }
  }, [isTv, libraryId, mediaItemId, season]);

  useEffect(() => {
    activeRef.current = true;
    prevActiveRef.current = false;
    void load();
    return () => {
      activeRef.current = false;
      loadAbortRef.current?.abort();
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
    if (busy || (singleEpisode && !canDetectSingleEpisode)) return;
    setBusy(singleEpisode ? "episode" : "season");
    setError(null);
    setMessage(null);
    try {
      const payload = !isTv
        ? { mode: "theintrodb_only" as const }
        : singleEpisode && selectedEpisode != null
          ? {
              season,
              episode: selectedEpisode,
              ...(selectedFileId ? { file_id: selectedFileId } : {}),
              mode: effectiveMode,
            }
          : { season, mode: effectiveMode };
      const receipt = await detectItemMarkers(libraryId, mediaItemId, payload);
      if (!activeRef.current) return;
      const skipNote =
        receipt.skipped_multi_unit > 0
          ? `（已自动跳过 ${receipt.skipped_multi_unit} 个合并集文件）`
          : "";
      if (effectiveMode === "theintrodb_only") {
        const count = receipt.updated_episodes ?? 0;
        setMessage(
          count > 0
            ? `TheIntroDB 云端同步完成，已更新 ${count} 项标记${skipNote}。`
            : `TheIntroDB 云端同步完成，未发现可更新标记${skipNote}。`,
        );
        await load();
        onChangedRef.current?.();
      } else {
        setMessage(
          receipt.already_running
            ? "该季已有片头片尾任务正在运行。"
            : `已提交片头片尾检测任务${skipNote}。`,
        );
        await load();
      }
    } catch (cause) {
      if (activeRef.current) setError(`提交检测失败：${(cause as Error).message}`);
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const handleCancel = async (targetFileId?: string) => {
    if (busy) return;
    setBusy("cancel");
    setError(null);
    setMessage(null);
    try {
      const receipt = await cancelFingerprintQueue({
        library_id: libraryId,
        media_id: mediaItemId,
        ...(isTv ? { season } : {}),
        ...(targetFileId ? { file_id: targetFileId } : {}),
        include_running: true,
      });
      if (!activeRef.current) return;
      setMessage(
        `已停止检测（取消 ${receipt.cancelled_units} 项，中断 ${receipt.aborted_running} 项在途读取）。`,
      );
      await load();
    } catch (cause) {
      if (activeRef.current) setError(`取消检测失败：${(cause as Error).message}`);
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const beginEdit = (row: SeasonEpisodeMarkerRow) => {
    setEditing({
      fileId: row.file_id,
      season: row.season,
      episode: row.episode,
      durationMs: row.duration_ms,
      introStart: msToInputSeconds(row.marker?.intro_start_ms),
      introEnd: msToInputSeconds(row.marker?.intro_end_ms),
      outroStart: msToInputSeconds(row.marker?.outro_start_ms),
      outroEnd: msToInputSeconds(row.marker?.outro_end_ms),
      locked: true,
    });
    setError(null);
  };

  const submitManualEdit = async () => {
    if (!editing || busy) return;
    const validated = validateEditDraft(editing);
    if ("error" in validated) {
      setError(validated.error);
      return;
    }
    setBusy("manual");
    setError(null);
    try {
      await saveManualMarker(libraryId, mediaItemId, {
        file_id: editing.fileId,
        ...(isTv && editing.season != null && editing.episode != null
          ? { season: editing.season, episode: editing.episode }
          : {}),
        intro_start_ms: validated.introStartMs,
        intro_end_ms: validated.introEndMs,
        outro_start_ms: validated.outroStartMs,
        outro_end_ms: validated.outroEndMs,
        locked: editing.locked,
      });
      if (!activeRef.current) return;
      const targetLabel =
        isTv && editing.episode != null ? `第 ${editing.episode} 集` : "正片";
      const lockAction = editing.locked ? "保存并锁定" : "保存并解锁";
      setEditing(null);
      setMessage(`已${lockAction}${targetLabel}片头片尾时间。`);
      await load();
      onChangedRef.current?.();
    } catch (cause) {
      if (activeRef.current) setError(`保存人工标记失败：${(cause as Error).message}`);
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
        ...(isTv && row.season != null && row.episode != null
          ? { season: row.season, episode: row.episode }
          : {}),
      });
      if (!activeRef.current) return;
      if (
        editing &&
        editing.fileId === row.file_id &&
        editing.season === row.season &&
        editing.episode === row.episode
      ) {
        setEditing(null);
      }
      const targetLabel = isTv && row.episode != null ? `第 ${row.episode} 集` : "正片";
      setMessage(`已清除${targetLabel}片头片尾标记。`);
      await load();
      onChangedRef.current?.();
    } catch (cause) {
      if (activeRef.current) setError(`清除标记失败：${(cause as Error).message}`);
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const activePhase = diag?.active ? phaseLabel(diag.job?.phase) : null;
  const localizedSampling = samplingModeLabel(diag?.job?.sampling_mode);

  return (
    <div className="mt-3 rounded-2xl border border-white/[0.08] bg-white/[0.03] p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2 text-caption">
          {isTv && seasons.length > 0 && (
            <select
              aria-label="检测季选择"
              value={season}
              onChange={(e) => setSeason(Number(e.target.value))}
              className="rounded-lg border border-white/10 bg-black/40 px-2.5 py-1 text-white/85"
            >
              {seasons.map((s) => (
                <option key={s} value={s}>
                  {s === 0 ? "特别篇" : `第 ${s} 季`}
                </option>
              ))}
            </select>
          )}
          <select
            aria-label="片头片尾检测策略"
            value={effectiveMode}
            onChange={(e) => setMode(e.target.value as MarkerDetectMode)}
            className="rounded-lg border border-white/10 bg-black/40 px-2.5 py-1 text-white/85"
          >
            {isTv && <option value="reuse_valid">复用缓存重算（快速本地比对）</option>}
            {isTv && <option value="recapture">重新采样声纹（覆盖旧音频缓存）</option>}
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
            {isTv ? "检测本季" : "同步云端标记"}
          </button>
          {canDetectSingleEpisode && (
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
            className="rounded-lg border border-white/10 px-2.5 py-1 text-white/60 transition hover:bg-white/[0.06] hover:text-white/90 disabled:opacity-40"
          >
            刷新诊断
          </button>
        </div>
      </div>

      {diag?.paused && (
        <p className="mt-2 text-caption text-amber-200/80">
          后台声纹队列当前处于暂停状态；上方手动触发的检测任务仍可立即执行。
        </p>
      )}

      {diag?.job && (
        <div className="mt-2.5 flex flex-wrap items-center gap-3 rounded-xl border border-white/[0.06] bg-black/20 px-3 py-2 text-caption text-white/70">
          <span>
            最近任务：
            {diag.job.status === "running" || diag.job.status === "queued"
              ? "进行中"
              : diag.job.status === "succeeded"
                ? "已完成"
                : diag.job.status === "cancelled"
                  ? "已取消"
                  : "失败"}
          </span>
          {activePhase && <span>阶段：{activePhase}</span>}
          {localizedSampling && <span>模式：{localizedSampling}</span>}
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
            手工编辑
            {isTv && editing.episode != null ? `第 ${editing.episode} 集` : "正片"}
            （支持填秒数如 90 或分:秒如 01:30，留空表示无）
          </p>
          <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
            <label className="text-white/70">
              片头起
              <input
                type="text"
                aria-label="片头起点"
                value={editing.introStart}
                onChange={(e) =>
                  setEditing((prev) => (prev ? { ...prev, introStart: e.target.value } : prev))
                }
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片头止
              <input
                type="text"
                aria-label="片头终点"
                value={editing.introEnd}
                onChange={(e) =>
                  setEditing((prev) => (prev ? { ...prev, introEnd: e.target.value } : prev))
                }
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片尾起
              <input
                type="text"
                aria-label="片尾起点"
                value={editing.outroStart}
                onChange={(e) =>
                  setEditing((prev) => (prev ? { ...prev, outroStart: e.target.value } : prev))
                }
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
            <label className="text-white/70">
              片尾止
              <input
                type="text"
                aria-label="片尾终点"
                value={editing.outroEnd}
                onChange={(e) =>
                  setEditing((prev) => (prev ? { ...prev, outroEnd: e.target.value } : prev))
                }
                className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
              />
            </label>
          </div>
          <label className="mt-2.5 inline-flex items-center gap-2 text-white/80">
            <input
              type="checkbox"
              aria-label="锁定人工标记"
              checked={editing.locked}
              onChange={(e) =>
                setEditing((prev) => (prev ? { ...prev, locked: e.target.checked } : prev))
              }
              className="rounded border-white/20 bg-black/40"
            />
            锁定此标记（防止后续自动检测覆盖）
          </label>
          <div className="mt-2.5 flex items-center gap-2">
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void submitManualEdit()}
              className="rounded-lg bg-sky-500/25 px-3 py-1 font-medium text-sky-100 hover:bg-sky-500/35 disabled:opacity-40"
            >
              {editing.locked ? "保存锁定" : "保存并解锁"}
            </button>
            <button
              type="button"
              disabled={busy != null}
              onClick={() => setEditing(null)}
              className="rounded-lg border border-white/15 px-3 py-1 text-white/70 hover:bg-white/10 disabled:opacity-40"
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
                <th className="px-3 py-1.5">{isTv ? "集" : "单元"}</th>
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
                const rowActive =
                  diag.active &&
                  (row.detection?.status === "queued" || row.detection?.status === "running");
                return (
                  <tr key={`${row.file_id}:${row.episode ?? "whole"}`}>
                    <td className="px-3 py-1.5 font-medium text-white/85">
                      {isTv && row.episode != null
                        ? `E${String(row.episode).padStart(2, "0")}`
                        : "正片"}
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
                          {rowActive && (
                            <button
                              type="button"
                              disabled={busy != null}
                              onClick={() => void handleCancel(row.file_id)}
                              className="text-amber-300 hover:underline disabled:opacity-40"
                            >
                              停止
                            </button>
                          )}
                          <button
                            type="button"
                            disabled={busy != null}
                            onClick={() => beginEdit(row)}
                            className="text-sky-300 hover:underline disabled:opacity-40"
                          >
                            编辑/锁定
                          </button>
                          {row.marker && (
                            <button
                              type="button"
                              disabled={busy != null}
                              onClick={() => void handleDeleteMarker(row)}
                              className="text-white/50 hover:text-red-300 disabled:opacity-40"
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
