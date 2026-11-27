"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import {
  cancelFingerprintQueue,
  getFingerprintQueueStatus,
  getFingerprintSettings,
  pauseFingerprintQueue,
  resumeFingerprintQueue,
  saveFingerprintSettings,
  type FingerprintProbeSettings,
  type FingerprintQueueStatus,
} from "@/lib/api/probe";

function queueBadge(queue: FingerprintQueueStatus): { label: string; restricted: boolean } {
  if (queue.paused) {
    return { label: "声纹队列已暂停", restricted: true };
  }
  if (queue.background_paused && queue.remote_reads_paused) {
    return { label: "受全局探测暂停限制 · 远程读取已暂停", restricted: true };
  }
  if (queue.background_paused) {
    return { label: "受全局探测暂停限制", restricted: true };
  }
  if (queue.remote_reads_paused) {
    return { label: "远程读取已暂停", restricted: true };
  }
  return { label: "声纹队列已启用", restricted: false };
}

export function FingerprintSettingsCard() {
  const [saved, setSaved] = useState<FingerprintProbeSettings | null>(null);
  const [draftWorkers, setDraftWorkers] = useState<number>(1);
  const [queue, setQueue] = useState<FingerprintQueueStatus | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const activeRef = useRef(true);

  const load = useCallback(async () => {
    setError("");
    try {
      const [settings, status] = await Promise.all([
        getFingerprintSettings(),
        getFingerprintQueueStatus(),
      ]);
      if (!activeRef.current) return;
      setSaved(settings);
      setDraftWorkers(settings.workers);
      setQueue(status);
    } catch (cause) {
      if (activeRef.current) {
        setError(`读取声纹队列状态失败：${(cause as Error).message}`);
      }
    }
  }, []);

  useEffect(() => {
    activeRef.current = true;
    void load();
    return () => {
      activeRef.current = false;
    };
  }, [load]);

  const pendingTotal =
    (queue?.queued_fingerprint ?? 0) +
    (queue?.running_fingerprint ?? 0) +
    (queue?.queued_analysis ?? 0);
  const shouldPoll = pendingTotal > 0 || (queue?.active_workers ?? 0) > 0;

  useEffect(() => {
    if (!shouldPoll) return;
    let inFlight = false;
    const timer = setInterval(() => {
      if (inFlight || !activeRef.current) return;
      inFlight = true;
      void getFingerprintQueueStatus()
        .then((status) => {
          if (activeRef.current) setQueue(status);
        })
        .catch(() => undefined)
        .finally(() => {
          inFlight = false;
        });
    }, 3000);
    return () => clearInterval(timer);
  }, [shouldPoll]);

  const invalidWorkers =
    !Number.isInteger(draftWorkers) || draftWorkers < 1 || draftWorkers > 8;
  const dirty = saved != null && draftWorkers !== saved.workers;

  const handleSave = async () => {
    if (!saved || invalidWorkers || busy) return;
    setBusy("save");
    setError("");
    setMessage("");
    try {
      const next = await saveFingerprintSettings({
        workers: draftWorkers,
        paused: queue?.paused ?? saved.paused,
      });
      if (!activeRef.current) return;
      setSaved(next);
      setDraftWorkers(next.workers);
      setMessage("声纹并发设置已保存并立即生效。");
    } catch (cause) {
      if (activeRef.current) {
        setError(`保存声纹设置失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const handleTogglePause = async () => {
    if (!queue || busy) return;
    setBusy("toggle");
    setError("");
    setMessage("");
    try {
      const next = queue.paused
        ? await resumeFingerprintQueue()
        : await pauseFingerprintQueue();
      if (!activeRef.current) return;
      setQueue(next);
      setSaved((prev) => (prev ? { ...prev, paused: next.paused } : prev));
    } catch (cause) {
      if (activeRef.current) {
        setError(`切换声纹队列状态失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const handleCancel = async () => {
    if (busy) return;
    setBusy("cancel");
    setError("");
    setMessage("");
    try {
      const receipt = await cancelFingerprintQueue({ include_running: true });
      if (!activeRef.current) return;
      setMessage(
        `已取消 ${receipt.cancelled_units} 项排队声纹任务，终止 ${receipt.aborted_running} 项在途提取。`,
      );
      const status = await getFingerprintQueueStatus();
      if (activeRef.current) setQueue(status);
    } catch (cause) {
      if (activeRef.current) {
        setError(`终止声纹任务失败：${(cause as Error).message}`);
      }
    } finally {
      if (activeRef.current) setBusy(null);
    }
  };

  const badge = queue ? queueBadge(queue) : null;

  return (
    <section className="rounded-2xl border border-white/[0.08] bg-white/[0.03] p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sub font-semibold text-white/80">
            独立声纹检测队列控制
          </h3>
          <p className="mt-1 text-caption text-white/45">
            单独暂停、恢复或终止后台片头片尾声纹提取，不影响常规媒体流信息（ffprobe）探测。
          </p>
        </div>
        {badge && (
          <span
            className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-caption font-medium ${
              badge.restricted
                ? "border-amber-400/30 bg-amber-500/15 text-amber-200"
                : "border-emerald-400/30 bg-emerald-500/15 text-emerald-200"
            }`}
          >
            {badge.label}
          </span>
        )}
      </div>

      {queue && (
        <div className="mt-3 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/[0.06] bg-white/[0.02] px-3.5 py-2.5 text-caption text-white/70">
          <span>
            活动 {queue.active_workers}/{queue.workers} · 正在提取{" "}
            {queue.running_fingerprint} · 排队采集 {queue.queued_fingerprint} · 待整季分析{" "}
            {queue.queued_analysis}
          </span>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void handleTogglePause()}
              className="rounded-lg border border-white/15 bg-white/10 px-3 py-1 text-caption font-medium text-white transition hover:bg-white/20 disabled:opacity-40"
            >
              {queue.paused ? "恢复声纹队列" : "暂停声纹队列"}
            </button>
            <button
              type="button"
              disabled={busy != null || pendingTotal === 0}
              onClick={() => void handleCancel()}
              className="rounded-lg border border-red-400/25 bg-red-500/10 px-3 py-1 text-caption font-medium text-red-200 transition hover:bg-red-500/20 disabled:opacity-40"
            >
              终止声纹任务
            </button>
          </div>
        </div>
      )}

      <div className="mt-4 flex flex-wrap items-end gap-3">
        <label className="block text-sub text-white/75">
          声纹检测并发
          <input
            type="number"
            aria-label="声纹检测并发"
            min={1}
            max={8}
            step={1}
            disabled={busy != null || saved == null}
            value={Number.isNaN(draftWorkers) ? "" : draftWorkers}
            onChange={(e) => {
              const next = e.target.value === "" ? Number.NaN : Number(e.target.value);
              setDraftWorkers(next);
              setMessage("");
            }}
            className="mt-1.5 block w-36 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
          />
          <span className="mt-1 block text-caption text-white/40">
            默认 1（范围 1–8）；远程 STRM 仍受共享远程并发与冷却间隔限制。
          </span>
        </label>
        <button
          type="button"
          disabled={busy != null || invalidWorkers || !dirty}
          onClick={() => void handleSave()}
          className="rounded-xl bg-white/15 px-4 py-2 text-ui font-medium text-white transition hover:bg-white/25 disabled:opacity-40"
        >
          {busy === "save" ? "保存中…" : "保存声纹设置"}
        </button>
      </div>

      {message && (
        <p role="status" className="mt-2.5 text-caption text-emerald-300/85">
          {message}
        </p>
      )}
      {error && (
        <div className="mt-2.5 flex flex-wrap items-center gap-2">
          <p role="alert" className="text-caption text-[var(--danger)]">
            {error}
          </p>
          {saved == null && (
            <button
              type="button"
              disabled={busy != null}
              onClick={() => void load()}
              className="rounded-lg border border-white/15 px-2.5 py-1 text-caption text-white/80 hover:bg-white/10 disabled:opacity-40"
            >
              重试
            </button>
          )}
        </div>
      )}
    </section>
  );
}
