/**
 * 一条下载任务**此刻是否要用户处理**，以及按作品合并的分组规则。
 *
 * 从 `task-activity` 里单拎出来，理由与 `job-attention` 相同：侧栏角标、活动页
 * 一级切换器、任务卡片上的"需要处理："前缀读的必须是同一份判定，各算各的就会
 * 出现角标数与页面对不上的自相矛盾。这里不碰 React、不发请求，可以直接单测。
 */

import type { DownloadTask } from "@/lib/api/downloaders";
import type { JobView } from "@/lib/api/jobs";
// 值导入必须带 .ts 后缀的相对路径：本模块要进 node --test（见 tsconfig 注释）
import { jobNeedsAttention } from "./job-attention.ts";

/** 旧 DownloadTask 的兼容字段：新契约已删除（见 lib/api/downloaders.ts）。 */
interface LegacyDownloadTaskFields {
  source?: "subscription" | "manual" | "boost" | "external" | null;
  can_replace?: boolean;
  landing_error?: string | null;
  rescue_message?: string | null;
  media_item_id?: number | null;
  poster_url?: string | null;
}

type DownloadTaskLike = DownloadTask & LegacyDownloadTaskFields;

export interface DownloadTaskGroup {
  key: string;
  mediaItemId: number | null;
  title: string;
  kind: string | null;
  posterUrl: string | null;
  tasks: DownloadTask[];
}

/**
 * 任务来源的组件层派生。新契约 DownloadTask 不再下发 source 字段，按订阅上下文
 * 与媒体身份归类：有订阅引用的是订阅投递，有媒体身份（手动下载）的是 manual，
 * 两者皆无的视为刷流做种；旧形状仍优先读 source 字段。
 */
export function taskSourceOf(
  task: DownloadTaskLike,
): "subscription" | "manual" | "boost" | "external" {
  if (task.source) return task.source;
  if (task.subscriptions.length > 0) return "subscription";
  if (task.media_title || task.media_kind) return "manual";
  return "boost";
}

/** 刷流做种：无媒体身份与入库流转（订阅/手动投递才有），折叠为独立分组。 */
export function isBoostTask(task: DownloadTask): boolean {
  return taskSourceOf(task) === "boost";
}

/** 订阅投递的任务（新契约以 subscriptions 非空标识）。 */
export function isSubscriptionTask(task: DownloadTask): boolean {
  return taskSourceOf(task) === "subscription";
}

/** 媒体条目主键：新契约挪进了 subscriptions[0]，旧形状仍在任务本体上。 */
function mediaItemIdOf(task: DownloadTaskLike): number | null {
  return task.subscriptions[0]?.media_item_id ?? task.media_item_id ?? null;
}

function posterUrlOf(task: DownloadTaskLike): string | null {
  return task.subscriptions[0]?.poster_url ?? task.poster_url ?? null;
}

export function groupDownloadTasks(tasks: DownloadTask[]): DownloadTaskGroup[] {
  const groups = new Map<string, DownloadTaskGroup>();
  for (const task of tasks) {
    // 只用数据库里的媒体条目主键合并。同名但未识别的资源必须各自保留，
    // 不能因为标题解析相似就把两个版本或两部同名作品错误折叠。
    const mediaItemId = mediaItemIdOf(task);
    const key = mediaItemId != null ? `media:${mediaItemId}` : `task:${task.id}`;
    const existing = groups.get(key);
    if (existing) {
      existing.tasks.push(task);
      const poster = posterUrlOf(task);
      if (!existing.posterUrl && poster) existing.posterUrl = poster;
      continue;
    }
    groups.set(key, {
      key,
      mediaItemId,
      title:
        task.media_title ||
        task.subscriptions[0]?.media_title ||
        task.name ||
        task.info_hash ||
        task.id,
      kind: task.media_kind ?? task.subscriptions[0]?.media_kind ?? null,
      posterUrl: posterUrlOf(task),
      tasks: [task],
    });
  }
  return [...groups.values()];
}

/**
 * 内容核验证明种子里没有的集（声明的覆盖范围与实际文件不符）。
 *
 * 这些集已经退回重新寻找资源，等这个种子永远等不到——不能再按"其余等待下载
 * 完成"讲，否则会出现「下载完成 7.59 GB」和「等待下载完成」同框的自相矛盾。
 */
export function contentMissingLabel(task: DownloadTask): string | null {
  const units = (task.subscriptions[0]?.units ?? []).filter((unit) => unit.content_missing);
  if (units.length === 0) return null;
  const first = units[0];
  const label = `S${String(first.season_number).padStart(2, "0")}E${String(
    first.episode_number,
  ).padStart(2, "0")}`;
  return units.length === 1 ? label : `${label} 等 ${units.length} 集`;
}

/**
 * 这条下载任务是不是**现在**要用户动手。
 *
 * 外部任务（不是 crawler-media 投递、也没有手动认领身份的种子）不参与：它们没有
 * 工单可救援、没有入库可推进，crawler-media 对它们能做的只有"看一眼"。下载器里
 * 积压的几十上百个陈年错误种子若全算成待办，会把真正需要处理的订阅任务淹没，
 * 用户点开只看到"请检查下载器"，而下载器本身是正常的（真实教训）。它们仍按
 * 真实状态显示在「进行中」里，只是不再报警、不再计数。
 */
export function downloadTaskNeedsAttention(
  task: DownloadTask,
  ingestJob: JobView | null | undefined,
): boolean {
  const legacy = task as DownloadTaskLike;
  if (taskSourceOf(legacy) === "external") return false;
  return (
    legacy.can_replace === true ||
    task.state === "error" ||
    task.state === "missing" ||
    // 下载完成但 crawler-media 看不到文件：侧栏红灯亮着，卡片不能还写"等待入库"
    legacy.landing_error != null ||
    contentMissingLabel(task) != null ||
    (ingestJob != null && jobNeedsAttention(ingestJob))
  );
}

export function downloadGroupNeedsAttention(
  group: DownloadTaskGroup,
  ingestJobsByHash: Map<string, JobView>,
): boolean {
  return group.tasks.some((task) =>
    downloadTaskNeedsAttention(
      task,
      task.info_hash ? ingestJobsByHash.get(task.info_hash.toLowerCase()) : undefined,
    ),
  );
}
