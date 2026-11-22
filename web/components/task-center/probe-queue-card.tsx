"use client";

import { Link } from "react-router-dom";

import type { ProbeQueueStatus } from "@/lib/api/probe";

function formatCount(value: number): string {
  return value.toLocaleString("zh-CN");
}

export function ProbeQueueCard({
  status,
  busyAction,
  onPause,
  onResume,
  onCancel,
}: {
  status: ProbeQueueStatus;
  busyAction: "pause" | "resume" | "cancel" | null;
  onPause: () => void;
  onResume: () => void;
  onCancel: (libraryId?: string) => void;
}) {
  const runningCount = status.running_metadata + status.running_fingerprint;
  const queuedCount =
    status.queued_metadata + status.queued_fingerprint + status.pending_obligations;
  const busy = busyAction != null;

  return (
    <section
      aria-label="后台媒体探测队列"
      className={`mt-6 rounded-2xl border p-4 max-md:p-3.5 ${
        status.paused
          ? "border-amber-400/25 bg-amber-500/[0.06]"
          : "border-sky-400/25 bg-sky-500/[0.06]"
      }`}
    >
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-ui font-semibold text-white">后台媒体信息探测队列</h3>
            {status.paused ? (
              <span className="inline-flex items-center gap-1.5 rounded-full border border-amber-400/30 bg-amber-500/15 px-2.5 py-0.5 text-caption font-semibold text-amber-200">
                <span className="size-1.5 rounded-full bg-amber-300" />
                已暂停 · 待手动继续
              </span>
            ) : (
              <span className="inline-flex items-center gap-1.5 rounded-full border border-sky-400/30 bg-sky-500/15 px-2.5 py-0.5 text-caption font-semibold text-sky-200">
                <span className="relative flex size-1.5" aria-hidden="true">
                  <span className="absolute inline-flex size-1.5 rounded-full bg-sky-300/75 motion-safe:animate-ping" />
                  <span className="relative inline-flex size-1.5 rounded-full bg-sky-300" />
                </span>
                正在探测
              </span>
            )}
            {status.remote_reads_paused && (
              <span className="rounded-full border border-white/15 bg-white/[0.06] px-2 py-0.5 text-caption text-white/65">
                STRM 远程读取已暂停
              </span>
            )}
          </div>

          <p className="mt-1.5 text-sub leading-6 text-white/75">
            {status.paused
              ? `后台批量媒体探测已挂起，剩余 ${formatCount(status.total_pending)} 个文件待探测`
              : `正在执行 ${formatCount(runningCount)} · 排队 ${formatCount(queuedCount)}`}
          </p>

          {status.libraries.length > 0 && (
            <div className="mt-2.5 flex flex-wrap items-center gap-2">
              {status.libraries.map((lib) => (
                <span
                  key={lib.library_id}
                  className="tnum inline-flex items-center gap-1 rounded-lg border border-white/10 bg-black/25 px-2.5 py-1 text-caption text-white/80"
                >
                  <span>
                    {lib.library_name}：{formatCount(lib.total_pending)} 项
                  </span>
                </span>
              ))}
            </div>
          )}
        </div>

        <div className="flex shrink-0 flex-wrap items-center gap-2">
          {status.paused ? (
            <button
              type="button"
              disabled={busy}
              onClick={onResume}
              className="rounded-xl bg-amber-400/90 px-3.5 py-1.5 text-ui font-semibold text-[#1a1204] transition hover:bg-amber-300 disabled:opacity-45"
            >
              {busyAction === "resume" ? "正在继续…" : "继续执行"}
            </button>
          ) : (
            <button
              type="button"
              disabled={busy}
              onClick={onPause}
              className="rounded-xl border border-white/15 bg-white/10 px-3.5 py-1.5 text-ui font-medium text-white transition hover:bg-white/20 disabled:opacity-45"
            >
              {busyAction === "pause" ? "正在暂停…" : "暂停"}
            </button>
          )}
          <button
            type="button"
            disabled={busy || queuedCount === 0}
            onClick={() => onCancel()}
            className="rounded-xl border border-white/12 bg-white/[0.05] px-3 py-1.5 text-ui font-medium text-white/80 transition hover:bg-white/[0.12] hover:text-white disabled:opacity-40"
          >
            {busyAction === "cancel" ? "正在清空…" : "清空队列"}
          </button>
          <Link
            to="/settings/scrape"
            className="rounded-xl px-2.5 py-1.5 text-caption font-medium text-white/60 transition hover:bg-white/[0.06] hover:text-white/90"
          >
            探测并发设置
          </Link>
        </div>
      </div>
    </section>
  );
}
