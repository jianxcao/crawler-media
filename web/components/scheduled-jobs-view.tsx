"use client";

import { useCallback, useEffect, useState } from "react";

import { useToast } from "@/components/feedback";
import { MaintenanceJobSettings } from "@/components/maintenance-job-settings";
import {
  isMaintenanceJob,
  listJobs,
  parseMaintenanceReport,
  runJob,
  toggleJob,
  updateJobSchedule,
  updateMaintenanceConfig,
  type JobView,
} from "@/lib/api/jobs";
import { maintenanceReportSummary } from "@/lib/maintenance-preview";

function formatInterval(secs: number): string {
  if (secs < 60) return `每 ${secs} 秒`;
  if (secs < 3600) return `每 ${Math.round(secs / 60)} 分钟`;
  if (secs < 86400) return `每 ${Math.round(secs / 3600)} 小时`;
  return `每 ${Math.round(secs / 86400)} 天`;
}

function formatCountdown(targetTimestamp: number | string | null, currentMs: number): string {
  if (!targetTimestamp) return "待调度";
  const ts =
    typeof targetTimestamp === "string"
      ? new Date(targetTimestamp).getTime()
      : targetTimestamp * 1000;
  const diffSec = Math.round((ts - currentMs) / 1000);
  if (diffSec <= 0) return "即将执行";
  if (diffSec < 60) return `${diffSec} 秒后`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)} 分 ${diffSec % 60} 秒后`;
  const hours = Math.floor(diffSec / 3600);
  const mins = Math.floor((diffSec % 3600) / 60);
  return `${hours} 小时 ${mins} 分后`;
}

export function ScheduledJobsView({ embedded = false }: { embedded?: boolean } = {}) {
  const toast = useToast();
  const [jobs, setJobs] = useState<JobView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [runningJobId, setRunningJobId] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);

  const loadJobs = useCallback(async () => {
    try {
      setLoading(true);
      const data = await listJobs({ scope: "system" });
      const scheduled = data.filter((j) => j.schedule != null);
      setJobs(scheduled);
      setError(null);
    } catch (e) {
      setError((e as Error).message || "加载定时任务失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadJobs();
    const interval = setInterval(() => {
      void loadJobs();
    }, 10000);
    return () => clearInterval(interval);
  }, [loadJobs]);

  const [togglingId, setTogglingId] = useState<string | null>(null);
  const [editingScheduleId, setEditingScheduleId] = useState<string | null>(null);
  const [scheduleSecs, setScheduleSecs] = useState<number>(30);
  const [updatingSchedule, setUpdatingSchedule] = useState(false);
  const [settingsJobId, setSettingsJobId] = useState<string | null>(null);
  const [maintenanceError, setMaintenanceError] = useState<string | null>(null);

  const handleUpdateSchedule = async (jobId: string, secs: number) => {
    try {
      setUpdatingSchedule(true);
      const targetJob = jobs.find((j) => j.id === jobId);
      if (targetJob && isMaintenanceJob(targetJob)) {
        const draft = targetJob.payload || {};
        await updateMaintenanceConfig(jobId, draft, draft, secs);
      } else {
        await updateJobSchedule(jobId, secs);
      }
      setEditingScheduleId(null);
      await loadJobs();
      toast.success("执行周期已更新");
    } catch (e) {
      toast.error(`修改调度周期失败: ${(e as Error).message}`);
    } finally {
      setUpdatingSchedule(false);
    }
  };

  const handleToggle = async (job: JobView) => {
    try {
      setTogglingId(job.id);
      await toggleJob(job.id, !job.enabled);
      await loadJobs();
      toast.success(job.enabled ? "已暂停该定时计划" : "已启用该定时计划");
    } catch (e) {
      toast.error(`切换状态失败: ${(e as Error).message}`);
    } finally {
      setTogglingId(null);
    }
  };

  const handleRunNow = async (job: JobView) => {
    if (runningJobId === job.id || job.last_status === "running") {
      return;
    }
    try {
      setRunningJobId(job.id);
      await runJob(job.id);
      await loadJobs();
      toast.success("已触发立即执行");
    } catch (e) {
      toast.error(`触发失败: ${(e as Error).message}`);
    } finally {
      setRunningJobId(null);
    }
  };

  const content = (
    <>
      {embedded ? (
        <div className="mt-5 flex flex-wrap items-center justify-between gap-3">
          <p className="text-sub text-[var(--text-muted)]">
            查看系统周期性自动化作业、下次执行倒计时，并支持随时一键手动执行。
          </p>
          <button
            type="button"
            onClick={() => void loadJobs()}
            disabled={loading}
            className="rounded-lg bg-white/10 px-3 py-1.5 text-caption font-medium text-white transition hover:bg-white/15 disabled:opacity-50"
          >
            {loading ? "正在刷新…" : "刷新"}
          </button>
        </div>
      ) : (
        <div className="flex flex-wrap items-center justify-between gap-4 border-b border-white/10 pb-6">
          <div>
            <h1 className="text-2xl font-bold tracking-tight">定时任务调度中心</h1>
            <p className="mt-1 text-sm text-[var(--text-muted)]">
              查看系统周期性自动化作业、下次执行倒计时，并支持随时一键手动执行。
            </p>
          </div>
          <div className="flex items-center gap-3">
            <button
              type="button"
              onClick={() => void loadJobs()}
              disabled={loading}
              className="rounded-lg bg-white/10 px-3.5 py-1.5 text-sm font-medium text-white transition hover:bg-white/15 disabled:opacity-50"
            >
              {loading ? "正在刷新..." : "刷新"}
            </button>
          </div>
        </div>
      )}

      {error && (
        <div className="mt-4 rounded-lg border border-red-500/20 bg-red-500/10 p-4 text-sm text-red-400">
          {error}
        </div>
      )}

      <div className={`${embedded ? "mt-4" : "mt-6"} grid grid-cols-1 gap-4 md:grid-cols-2`}>
        {jobs.map((job) => {
          const isRunning = runningJobId === job.id || job.last_status === "running";
          const intervalLabel = job.schedule
            ? formatInterval(job.schedule.interval_secs)
            : "手动触发";
          const countdownLabel = !job.enabled
            ? "已暂停调度"
            : formatCountdown(job.next_run_after, now);

          return (
            <div
              key={job.id}
              className="flex flex-col justify-between rounded-xl border border-white/10 bg-white/[0.02] p-5 transition hover:border-white/20 hover:bg-white/[0.04]"
            >
              <div>
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <h3
                      className="truncate text-base font-semibold text-white"
                      title={job.friendly_name || job.name}
                    >
                      {job.friendly_name || job.name}
                    </h3>
                    <div className="mt-1 flex flex-wrap items-center gap-2">
                      <button
                        type="button"
                        onClick={() => {
                          setEditingScheduleId(editingScheduleId === job.id ? null : job.id);
                          setScheduleSecs(job.schedule ? job.schedule.interval_secs : 30);
                        }}
                        title="点击调整调度周期"
                        className="inline-flex cursor-pointer items-center gap-1 rounded bg-white/10 px-2 py-0.5 text-xs text-white/90 transition hover:bg-white/20"
                      >
                        <span>{intervalLabel}</span>
                        <span className="text-[10px] text-white/50">✏️</span>
                      </button>
                      {job.last_status && (
                        <span
                          className={`rounded px-2 py-0.5 text-xs font-medium ${
                            job.last_status === "succeeded"
                              ? "bg-emerald-500/20 text-emerald-400"
                              : job.last_status === "failed"
                                ? "bg-red-500/20 text-red-400"
                                : "bg-sky-500/20 text-sky-400"
                          }`}
                        >
                          上次:{" "}
                          {job.last_status === "succeeded"
                            ? "成功"
                            : job.last_status === "failed"
                              ? "失败"
                              : "运行中"}
                        </span>
                      )}
                      {isMaintenanceJob(job) && (
                        <button
                          type="button"
                          onClick={() => {
                            setSettingsJobId(settingsJobId === job.id ? null : job.id);
                            setMaintenanceError(null);
                          }}
                          title="维护参数：保留天数 / dry_run / 预览 / VACUUM"
                          className="inline-flex cursor-pointer items-center gap-1 rounded bg-white/10 px-2 py-0.5 text-xs text-white/90 transition hover:bg-white/20"
                        >
                          <span>维护参数</span>
                          <span className="text-[10px] text-white/50">⚙️</span>
                        </button>
                      )}
                    </div>
                  </div>

                  <button
                    type="button"
                    onClick={() => void handleToggle(job)}
                    disabled={togglingId === job.id}
                    title={job.enabled ? "点击暂停任务调度" : "点击启用任务调度"}
                    className={`inline-flex shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition ${
                      job.enabled
                        ? "border border-emerald-500/20 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20"
                        : "border border-white/10 bg-white/5 text-white/40 hover:bg-white/10"
                    }`}
                  >
                    <span
                      className={`h-1.5 w-1.5 rounded-full ${
                        job.enabled ? "animate-pulse bg-emerald-400" : "bg-white/40"
                      }`}
                    />
                    {togglingId === job.id ? "切换中..." : job.enabled ? "已启用" : "已暂停"}
                  </button>
                </div>

                {editingScheduleId === job.id && (
                  <div className="mt-3 rounded-lg border border-white/10 bg-white/[0.05] p-3 text-xs">
                    <div className="mb-2 font-semibold text-white/90">修改执行周期:</div>
                    <div className="mb-2 flex flex-wrap gap-2">
                      {(isMaintenanceJob(job)
                        ? [
                            { label: "1小时", secs: 3600 },
                            { label: "6小时", secs: 21600 },
                            { label: "12小时", secs: 43200 },
                            { label: "1天", secs: 86400 },
                            { label: "7天", secs: 604800 },
                          ]
                        : [
                            { label: "30秒", secs: 30 },
                            { label: "1分钟", secs: 60 },
                            { label: "5分钟", secs: 300 },
                            { label: "10分钟", secs: 600 },
                            { label: "30分钟", secs: 1800 },
                            { label: "1小时", secs: 3600 },
                          ]
                      ).map((preset) => (
                        <button
                          key={preset.secs}
                          type="button"
                          onClick={() => setScheduleSecs(preset.secs)}
                          className={`rounded px-2 py-1 text-xs transition ${
                            scheduleSecs === preset.secs
                              ? "bg-sky-500 font-medium text-white"
                              : "bg-white/10 text-white/70 hover:bg-white/20"
                          }`}
                        >
                          {preset.label}
                        </button>
                      ))}
                    </div>
                    <div className="mt-2 flex items-center gap-2">
                      <span className="text-white/60">自定义秒数:</span>
                      <input
                        type="number"
                        min={isMaintenanceJob(job) ? 3600 : 5}
                        max={isMaintenanceJob(job) ? 31536000 : 86400 * 7}
                        value={scheduleSecs}
                        onChange={(e) => {
                          const floor = isMaintenanceJob(job) ? 3600 : 5;
                          setScheduleSecs(Math.max(floor, parseInt(e.target.value, 10) || floor));
                        }}
                        className="w-24 rounded border border-white/20 bg-black/40 px-2 py-1 text-xs text-white"
                      />
                      <button
                        type="button"
                        disabled={updatingSchedule}
                        onClick={() => void handleUpdateSchedule(job.id, scheduleSecs)}
                        className="rounded bg-sky-500 px-3 py-1 text-xs font-semibold text-white transition hover:bg-sky-600 disabled:opacity-50"
                      >
                        {updatingSchedule ? "保存中..." : "保存周期"}
                      </button>
                      <button
                        type="button"
                        onClick={() => setEditingScheduleId(null)}
                        className="rounded bg-white/10 px-2.5 py-1 text-xs text-white/70 transition hover:bg-white/20"
                      >
                        取消
                      </button>
                    </div>
                  </div>
                )}

                {isMaintenanceJob(job) && settingsJobId === job.id && (
                  <>
                    <MaintenanceJobSettings
                      job={job}
                      intervalSecs={job.schedule?.interval_secs}
                      onSaved={async () => {
                        setMaintenanceError(null);
                        await loadJobs();
                      }}
                      onError={setMaintenanceError}
                    />
                    {maintenanceError && (
                      <div className="mt-2 rounded border border-red-500/20 bg-red-500/10 p-2 text-xs text-red-300">
                        {maintenanceError}
                      </div>
                    )}
                  </>
                )}

                <div className="mt-4 space-y-1.5 rounded-lg bg-black/20 p-3 font-mono text-xs text-white/80">
                  <div className="flex justify-between">
                    <span className="font-sans text-[var(--text-muted)]">下次计划执行:</span>
                    <span
                      className={`font-sans font-bold ${
                        job.enabled ? "text-sky-400" : "text-white/40"
                      }`}
                    >
                      {countdownLabel}
                    </span>
                  </div>
                  {job.last_finished_at && (
                    <div className="flex justify-between">
                      <span className="font-sans text-[var(--text-muted)]">上次执行时间:</span>
                      <span>
                        {new Date(
                          typeof job.last_finished_at === "number"
                            ? job.last_finished_at * 1000
                            : job.last_finished_at,
                        ).toLocaleTimeString()}
                      </span>
                    </div>
                  )}
                  {maintenanceReportSummary(parseMaintenanceReport(job.last_report)) && (
                    <div className="mt-2 rounded bg-white/5 p-2 font-sans text-white/70">
                      最近报告：{maintenanceReportSummary(parseMaintenanceReport(job.last_report))}
                    </div>
                  )}
                  {job.last_status === "failed" && job.last_error && (
                    <div className="mt-2 break-all rounded border border-red-500/20 bg-red-500/10 p-2 font-sans text-red-300">
                      <div className="font-semibold text-red-400">失败原因:</div>
                      <div>{job.last_error}</div>
                    </div>
                  )}
                </div>
              </div>

              <div className="mt-4 flex items-center justify-end gap-3 border-t border-white/5 pt-4">
                <button
                  type="button"
                  onClick={() => void handleRunNow(job)}
                  disabled={isRunning}
                  className="rounded-lg bg-sky-500/20 px-3.5 py-1.5 text-xs font-semibold text-sky-300 transition hover:bg-sky-500/30 disabled:opacity-50"
                >
                  {isRunning ? "正在执行..." : "立即执行"}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {jobs.length === 0 && !loading && (
        <div className="mt-12 text-center text-sm text-[var(--text-muted)]">
          暂无已注册的定时任务。
        </div>
      )}
    </>
  );

  if (embedded) {
    return <div className="text-white">{content}</div>;
  }

  return <div className="mx-auto max-w-6xl px-4 py-8 text-white">{content}</div>;
}
