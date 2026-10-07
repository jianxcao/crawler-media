import type { DownloadTask } from "@/lib/api/downloaders";

/** 旧形状的兼容字段：新契约 DownloadTask 不再下发 can_replace。 */
interface LegacyReplaceableFields {
  can_replace?: boolean;
}

/**
 * 只有已进入救援窗口且仍能定位到任务时，提示区才提供立即换种。
 *
 * stalled/error/missing 都算救援窗口：任务卡死（stalled）、报错（error），或
 * 客户端可达但种子已不在下载器里（missing，订阅仍想要这些集）——都应该能给
 * 用户一条「重新寻找其他源」的路。
 *
 * 定位方式是任务中心的 `task.id`（后端编码为 `{subscribe_id}:{enclosure}`），
 * 而不是 info hash：订阅投递的任务在真正进入下载器之前拿不到 hash，
 * `info_hash` 在任务列表里恒为 null。旧形状仍按 can_replace 判断。
 */
export function shouldOfferInlineReplacement(task: DownloadTask & LegacyReplaceableFields): boolean {
  const legacyReplaceable = task.can_replace === true;
  const stalledOrError =
    task.state === "stalled" || task.state === "error" || task.state === "missing";
  const addressable = typeof task.id === "string" && task.id.length > 0;
  return addressable && (legacyReplaceable || stalledOrError);
}
