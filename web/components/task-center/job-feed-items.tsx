"use client";

import { JOB_STATUS_LABELS, JOB_TYPE_LABELS } from "@/components/job-center";
import { ChevronRightIcon } from "@/components/icons";
import { OverflowText } from "@/components/overflow-text";
import { isSystemCancelled, type JobView } from "@/lib/api/jobs";
import { buildIngestHistoryDetail, type IngestHistoryDetail } from "@/lib/ingest-history";
import { isDismissed } from "@/lib/job-attention";
import {
  formatClockTime,
  formatNextRunTime,
  formatTimelineDayLabel,
  timelineDayKey,
} from "@/lib/time";

import { TaskTimelineItem } from "./task-layout";

export function jobTimelineTone(job: JobView): "active" | "waiting" {
  return job.last_status === "running" ? "active" : "waiting";
}

const ACTIVE_JOB_ACTIONS: Record<string, string> = {
  "subtitle.generate": "正在生成字幕",
  "library.scan": "正在扫描",
  "library.metadata.refresh": "正在刷新媒体库元数据",
  "media.metadata.refresh": "正在刷新元数据",
  "library.chapter_images": "正在生成章节",
  "library.organize": "正在整理文件",
  "library.transfer": "正在转移文件",
  "library.ingest": "正在入库",
  subscribe_search: "正在搜索资源",
  subscribe_rss: "正在抓取 RSS",
  transfer: "正在转移文件",
  scrape: "正在刮削元数据",
  check_in: "正在打卡签到",
  catalog_refresh: "正在刷新元数据",
  watch_intake: "正在导入监控目录",
};

const COMPLETED_JOB_ACTIONS: Record<string, string> = {
  "subtitle.generate": "字幕生成",
  "library.scan": "扫描",
  "library.metadata.refresh": "元数据刷新",
  "media.metadata.refresh": "元数据刷新",
  "library.chapter_images": "章节生成",
  "library.organize": "文件整理",
  "library.transfer": "文件转移",
  "library.ingest": "入库",
  subscribe_search: "资源搜索",
  subscribe_rss: "RSS 抓取",
  transfer: "文件转移",
  scrape: "元数据刮削",
  check_in: "打卡签到",
  catalog_refresh: "元数据刷新",
  watch_intake: "目录导入",
};

function jobDetailString(job: JobView, key: string): string | null {
  // 新契约 JobDef 无运行期 progress，只能读入参 payload；运行期的 message 等
  // 键名若入参里有（如 file_name/media_title）仍能取到。
  const detail = job.payload[key];
  return typeof detail === "string" && detail ? detail : null;
}

function jobDetailNumber(job: JobView, key: string): number | null {
  const detail = job.payload[key];
  return typeof detail === "number" && Number.isFinite(detail) ? detail : null;
}

/** Feed 主标题优先使用执行结果确认过的作品名，避免把长 release 名当业务标题。 */
function jobFeedIdentity(job: JobView): string | null {
  if (job.friendly_name) {
    return job.friendly_name;
  }
  const title =
    jobDetailString(job, "media_title") ||
    jobDetailString(job, "title") ||
    job.name;
  if (!title) return null;
  if (/^[《「].+[》」]$/.test(title)) return title;
  if (job.kind === "library.scan") return `「${title}」`;
  if (
    job.kind === "library.ingest" ||
    job.kind === "subtitle.generate" ||
    job.kind.includes("metadata.refresh")
  ) {
    return `《${title}》`;
  }
  return title;
}

function jobOriginLabel(job: JobView): string {
  // 新契约 JobDef 无 origin/actor_name；有调度定义（schedule）的任务视为系统自动。
  return job.schedule ? "系统自动" : "后台任务";
}

function activeJobStatus(job: JobView): string {
  if (job.last_status === "running") {
    return ACTIVE_JOB_ACTIONS[job.kind] || "正在处理";
  }
  return JOB_STATUS_LABELS[job.last_status ?? ""] || job.last_status || "等待执行";
}

function historicalJobTitle(job: JobView): string {
  const identity = jobFeedIdentity(job);
  const action = COMPLETED_JOB_ACTIONS[job.kind];
  // 忽略只是"不再提醒"，不是"做完了"。历史里如实写「未完成」，否则用户回头
  // 翻记录会以为这件事已经成了。
  const result =
    job.last_status === "cancelled" ? "已取消" : job.last_status === "failed" ? "未完成" : "完成";
  if (identity && action) return `${identity}${action}${result}`;
  return `${identity || JOB_TYPE_LABELS[job.kind] || job.kind}${result}`;
}

function historicalJobSummary(job: JobView): string {
  if (job.kind === "library.scan") {
    const identified = jobDetailNumber(job, "identified");
    const unidentified = jobDetailNumber(job, "unidentified");
    const parts = [
      identified != null ? `识别 ${identified} 个` : null,
      unidentified != null ? `待识别 ${unidentified} 个` : null,
    ].filter((item): item is string => item != null);
    if (parts.length > 0) return parts.join(" · ");
  }
  if (job.last_status === "failed" && isDismissed(job)) {
    return job.last_error || jobDetailString(job, "message") || "任务失败，已被忽略";
  }
  return (
    (job.last_status === "failed" ? job.last_error : null) ||
    jobDetailString(job, "message") ||
    (job.last_status === "cancelled" ? "任务已取消" : "任务已完成")
  );
}

export function ActiveJobFeedItem({
  job,
  cancelling,
  onCancel,
}: {
  job: JobView;
  cancelling: boolean;
  onCancel: () => void;
}) {
  const cancellable = job.last_status !== "cancelled";
  const title = jobFeedIdentity(job) || JOB_TYPE_LABELS[job.kind] || job.kind;
  return (
    <article className="min-w-0 py-1.5">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-ui font-semibold leading-5 text-white/90">
            <OverflowText>{title}</OverflowText>
          </h3>
          <p className="mt-1 flex flex-wrap items-center gap-x-1.5 text-sub text-white/60">
            <span className="font-medium text-white/70">{activeJobStatus(job)}</span>
          </p>
        </div>
        {cancellable && (
          <button
            type="button"
            onClick={onCancel}
            disabled={cancelling}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/45 transition hover:bg-white/[0.06] hover:text-white/75 disabled:cursor-wait disabled:opacity-45"
          >
            {cancelling ? "正在取消…" : "取消任务"}
          </button>
        )}
      </div>
      {jobDetailString(job, "message") && (
        <OverflowText lines={2} className="mt-1.5 text-caption leading-5 text-white/42">
          {jobDetailString(job, "message")}
        </OverflowText>
      )}
      <p className="mt-2 text-caption text-white/30">
        {jobOriginLabel(job)}
        {job.last_status === "queued" &&
          job.next_run_after != null &&
          job.next_run_after !== "" && (
          <>
            <span aria-hidden="true"> · </span>
            {job.next_run_after === 0
              ? "即将执行"
              : `${formatNextRunTime(job.next_run_after)} 下次运行`}
          </>
        )}
        {job.last_status === "running" && (
          <>
            <span aria-hidden="true"> · </span>
            正在执行
          </>
        )}
      </p>
    </article>
  );
}

interface HistoryDayGroup {
  key: string;
  label: string;
  jobs: JobView[];
}

function groupHistoricalJobs(jobs: JobView[]): HistoryDayGroup[] {
  const groups = new Map<string, HistoryDayGroup>();
  for (const job of jobs) {
    const timestamp = job.last_finished_at || job.next_run_after || "";
    const key = timelineDayKey(timestamp);
    const existing = groups.get(key);
    if (existing) {
      existing.jobs.push(job);
    } else {
      groups.set(key, { key, label: formatTimelineDayLabel(timestamp), jobs: [job] });
    }
  }
  return [...groups.values()];
}

function HistoricalJobFeedItem({
  job,
  time,
  retrying,
  onRetry,
  undismissing,
  onUndismiss,
}: {
  job: JobView;
  /** 完成时刻。不再占用左侧独立列，跟摘要同行显示，让条目与分组标题左对齐。 */
  time: string;
  retrying: boolean;
  onRetry: () => void;
  undismissing: boolean;
  onUndismiss: () => void;
}) {
  // 忽略必须能反悔——用户是在信息不全时按下的它（"先不管这个"），
  // 撤销入口就得留在他能找回这条记录的地方，而不是只能靠重试再造一条新任务。
  const dismissed = job.last_status === "failed" && isDismissed(job);
  const ingestDetail = buildIngestHistoryDetail(job);
  return (
    <article className="group/history min-w-0 py-1.5">
      <div className="flex min-w-0 items-start gap-3">
        <div className="min-w-0 flex-1">
          <h3 className="text-ui font-medium leading-5 text-white/78">
            <OverflowText>{historicalJobTitle(job)}</OverflowText>
          </h3>
          <div className="mt-1 flex min-w-0 items-baseline gap-2 text-caption leading-5 text-white/40">
            <span className="tnum shrink-0 text-white/32">{time}</span>
            <OverflowText lines={2} className="min-w-0 flex-1">
              {ingestDetail?.summary ?? historicalJobSummary(job)}
            </OverflowText>
          </div>
          {ingestDetail && <IngestHistoryFiles detail={ingestDetail} />}
        </div>
        {dismissed && (
          <button
            type="button"
            onClick={onUndismiss}
            disabled={undismissing}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/35 transition hover:bg-white/[0.05] hover:text-white/65 disabled:cursor-wait disabled:opacity-40"
          >
            {undismissing ? "撤销中…" : "撤销忽略"}
          </button>
        )}
        {job.last_status === "cancelled" && !isSystemCancelled(job) && (
          <button
            type="button"
            onClick={onRetry}
            disabled={retrying}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-caption font-medium text-white/35 transition hover:bg-white/[0.05] hover:text-white/65 disabled:cursor-wait disabled:opacity-40"
          >
            {retrying ? "重新执行中…" : "重新执行"}
          </button>
        )}
      </div>
    </article>
  );
}

/** 入库历史默认只占一行，需要核对时再展开真实文件名与目标媒体库。 */
function IngestHistoryFiles({ detail }: { detail: IngestHistoryDetail }) {
  if (detail.fileGroups.length === 0 && detail.context == null) return null;
  const fileCount = detail.fileGroups.reduce((total, group) => total + group.files.length, 0);
  return (
    <details className="group/files mt-1.5 max-w-full text-caption">
      <summary className="flex w-fit max-w-full cursor-pointer list-none items-center gap-1.5 rounded-md py-1 pr-1 text-white/32 transition hover:text-white/60 [&::-webkit-details-marker]:hidden">
        <ChevronRightIcon className="size-3.5 shrink-0 transition-transform group-open/files:rotate-90" />
        <span>查看文件明细</span>
        {fileCount > 0 && <span className="tnum text-white/22">{fileCount} 个</span>}
      </summary>
      <div className="mt-1.5 hidden rounded-lg border border-white/[0.06] bg-white/[0.025] px-3 py-2.5 group-open/files:block">
        {detail.context && <p className="mb-2 text-white/38">{detail.context}</p>}
        <div className="space-y-2.5">
          {detail.fileGroups.map((group) => (
            <section key={group.label} aria-label={group.label}>
              <p className="mb-1 text-micro font-medium text-white/35">
                {group.label} · {group.files.length} 个
              </p>
              <ul className="space-y-1 text-white/52">
                {group.files.map((file) => (
                  <li key={file} title={file} className="break-all leading-5">
                    {file}
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      </div>
    </details>
  );
}

/** 历史按本地日期拆成轻量 Feed，首组默认展开，避免完成卡片占满纵向空间。 */
export function TaskHistorySection({
  jobs,
  initiallyOpen,
  separated,
  retryingJobId,
  onRetry,
  undismissingJobId,
  onUndismiss,
}: {
  jobs: JobView[];
  initiallyOpen: boolean;
  separated: boolean;
  retryingJobId: string | null;
  onRetry: (job: JobView) => void;
  undismissingJobId: string | null;
  onUndismiss: (job: JobView) => void;
}) {
  const groups = groupHistoricalJobs(jobs);
  return (
    <section
      className={separated ? "mt-6 border-t border-white/[0.1]" : "mt-3"}
      aria-label="历史任务"
    >
      {groups.map((group, index) => (
        <details
          key={group.key}
          open={initiallyOpen || index === 0 || undefined}
          className="group border-b border-white/[0.07] py-3.5 last:border-b-0"
        >
          <summary className="flex cursor-pointer list-none items-center gap-2 rounded-lg py-1 text-ui text-white/55 transition hover:text-white/80 [&::-webkit-details-marker]:hidden">
            <span className="font-semibold">{group.label}</span>
            <span className="tnum text-caption text-white/30">{group.jobs.length} 项</span>
            <span className="ml-auto text-caption text-white/30 group-open:hidden">展开</span>
            <span className="ml-auto hidden text-caption text-white/30 group-open:inline">收起</span>
            <ChevronRightIcon className="size-4 transition-transform group-open:rotate-90" />
          </summary>
          <div className="mt-3">
            {group.jobs.map((job) => (
              <TaskTimelineItem
                key={job.id}
                tone={job.last_status === "succeeded" ? "success" : "cancelled"}
              >
                <HistoricalJobFeedItem
                  job={job}
                  time={formatClockTime(job.last_finished_at ?? job.next_run_after)}
                  retrying={retryingJobId === job.id}
                  onRetry={() => onRetry(job)}
                  undismissing={undismissingJobId === job.id}
                  onUndismiss={() => onUndismiss(job)}
                />
              </TaskTimelineItem>
            ))}
          </div>
        </details>
      ))}
    </section>
  );
}
