import type {
  ResourceTiming,
  SubscriptionActivity,
  SubscriptionDownload,
  WantedItem,
} from "@/lib/api/subscriptions";

import { formatBytes, formatDuration } from "./format.ts";
import { shouldShowResourceTiming } from "./resource-timing.ts";
import { formatDateTime, formatRelativeTime } from "./time.ts";

/**
 * wantedPresentation 长词 → 状态胶囊三字短名。整套词表恰好能全部收敛到
 * 三字（配合微型链固定占宽 → 所有胶囊等宽，右轨对成一列状态列）。
 */
export const SHORT_STATUS: Record<string, string> = {
  入库失败: "失败",
  已提交下载: "已提交",
  排队搜索: "排队中",
  预测窗口: "预测中",
};

/** 里程碑链的一站。state：done=历史（中性）/ now=卡点（状态色）/ todo=未到 */
export interface Milestone {
  lab: string;
  /** fail = 卡在这一站且不会自愈（需要用户处理），与 now 同色同亮点、走红色 */
  state: "done" | "now" | "todo" | "fail";
  /** 右侧时间列：done 站的发生时刻、搜索站的次数等辅助短语；可为空 */
  time: string;
  det: string;
  /** 红色补充行：最近一次候选被拒原因 / 投递失败原因 / 入库失败原因 */
  why?: string;
  /** 弱化补充行：投递种子名 / 资源发布→提交时间链 */
  src?: string[];
}

/**
 * 工单 → 此刻仍未解决的失败活动（入库失败 / 投递失败）。
 * 判据是「该工单最新一条活动仍是失败」而不是比时间戳。
 */
export function pendingFailures(
  activities: SubscriptionActivity[],
): Map<string, SubscriptionActivity> {
  const failures = new Map<string, SubscriptionActivity>();
  const seen = new Set<string>();
  for (const a of activities) {
    if (a.wanted_item_id === null || seen.has(a.wanted_item_id)) continue;
    seen.add(a.wanted_item_id);
    if (a.type === "import_failed" || a.type === "dispatch_failed") {
      failures.set(a.wanted_item_id, a);
    }
  }
  return failures;
}

/** 活动类型 → 时间线圆点颜色：绿=成果，红=失败/拒绝，黄=暂停，蓝=常规动作。 */
export function activityColor(type: SubscriptionActivity["type"]): string {
  switch (type) {
    case "grabbed":
    case "match_accepted":
    case "completed":
    case "downloaded":
    case "imported":
    case "replacement_promoted":
      return "var(--ok)";
    case "upgrade_grabbed":
    case "upgraded":
      return "#2dd4bf";
    case "match_rejected":
    case "dispatch_failed":
    case "import_failed":
      return "var(--danger)";
    case "paused":
    case "download_stalled":
    case "upgrade_verify_failed":
    case "spec_mismatch":
      return "var(--warn)";
    default:
      return "var(--info)";
  }
}

/**
 * 默认展开哪几季：最新一季 ∪ 有在途工单的季。
 */
export function defaultOpenSeasons(wanted: WantedItem[]): Set<number> {
  const open = new Set<number>();
  const regular = wanted.filter((w) => w.season_number > 0);
  const pool = regular.length > 0 ? regular : wanted;
  if (pool.length > 0) {
    open.add(Math.max(...pool.map((w) => w.season_number)));
  }
  for (const w of wanted) {
    if (w.status === "grabbed" || w.status === "downloaded") open.add(w.season_number);
  }
  return open;
}

/**
 * 工单 → 里程碑链：播出 → 搜索 → 投递 → 下载 → 入库 →（洗版，规则组配了目标才有）。
 */
export function milestonesOf(
  w: WantedItem,
  isMovie: boolean,
  live: SubscriptionDownload | undefined,
  failure?: SubscriptionActivity,
  now: Date = new Date(),
): Milestone[] {
  const M: Milestone[] = [];

  if (isMovie) {
    const undated = w.status === "wanted" && w.next_search_at === null;
    M.push(
      undated
        ? { lab: "上映", state: "now", time: "", det: "上映日期未公布，定档后自动排队" }
        : { lab: "上映", state: "done", time: "", det: "已定档或已上映" },
    );
  } else if (!w.air_date) {
    M.push({
      lab: "播出",
      state: "done",
      time: "暂未定档",
      det: "播出日期暂未公布，已纳入周期追踪",
    });
  } else {
    M.push({
      lab: "播出",
      state: "done",
      time: w.air_date,
      det: new Date(w.air_date) > now ? `${w.air_date} 播出` : "已播出",
    });
  }
  const preAir = Boolean(w.air_date && new Date(w.air_date) > now);

  if (w.status !== "wanted") {
    M.push({
      lab: "搜索",
      state: "done",
      time: w.last_search_at ? formatRelativeTime(w.last_search_at) : "",
      det: w.search_attempts > 0 ? `搜索 ${w.search_attempts} 次后命中` : "被动匹配命中",
    });
  } else if (preAir) {
    M.push({ lab: "搜索", state: "todo", time: "", det: "定档后进入搜索队列" });
  } else {
    M.push({
      lab: "搜索",
      state: "now",
      time: w.search_attempts > 0 ? `已搜 ${w.search_attempts} 次` : "",
      det: wantedPresentation(w, now).note,
      why: w.last_reject_reason ? `最近一次被拒：${w.last_reject_reason}` : undefined,
    });
  }

  if (w.grabbed_at) {
    const src = [
      w.grab_title,
      resourceTimingNote(w.resource_timing, false),
    ].filter((s): s is string => !!s);
    M.push({
      lab: "投递",
      state: "done",
      time: formatRelativeTime(w.grabbed_at),
      det: "已提交下载器",
      src: src.length ? src : undefined,
    });
  } else {
    const lastTiming = resourceTimingNote(w.resource_timing, true);
    const dispatchFailure = failure?.type === "dispatch_failed" ? failure : undefined;
    M.push({
      lab: "投递",
      state: "todo",
      time: "",
      det: dispatchFailure ? "上次投递未成功，已退回队列" : "尚未找到符合规则组的资源",
      why: dispatchFailure?.message,
      src: lastTiming ? [lastTiming] : undefined,
    });
  }

  const downloadedAt = w.downloaded_at ?? w.imported_at;
  if (downloadedAt) {
    M.push({
      lab: "下载",
      state: "done",
      time: formatRelativeTime(downloadedAt),
      det: "全部落盘",
    });
  } else if (w.grabbed_at) {
    M.push({
      lab: "下载",
      state: "now",
      time: "进行中",
      det: live ? downloadNote(live) : "已提交下载器，等待进度汇报",
    });
  } else {
    M.push({ lab: "下载", state: "todo", time: "", det: "等待投递完成" });
  }

  if (w.imported_at) {
    M.push({
      lab: "入库",
      state: "done",
      time: formatRelativeTime(w.imported_at),
      det: w.upgrade?.current_label
        ? `已整理入库 · ${w.upgrade.current_label}`
        : "已整理入库",
    });
  } else if (failure?.type === "import_failed") {
    M.push({
      lab: "入库",
      state: "fail",
      time: "",
      det: "下载完成了，但无法把文件整理入库",
      why: failure.message,
    });
  } else if (w.downloaded_at) {
    M.push({ lab: "入库", state: "now", time: "", det: "下载完成，等待整理入库" });
  } else {
    M.push({ lab: "入库", state: "todo", time: "", det: "下载完成后自动整理" });
  }

  if (w.upgrade) {
    if (w.upgrade.active) {
      M.push({
        lab: "洗版",
        state: "now",
        time: w.upgrade.search_attempts > 0 ? `已搜 ${w.upgrade.search_attempts} 次` : "",
        det: `当前 ${w.upgrade.current_label} → 目标 ${w.upgrade.target_label}`,
      });
    } else if (w.upgrade.indeterminate) {
      M.push({
        lab: "洗版",
        state: "now",
        time: "",
        det: `${w.upgrade.current_label} · 无法确认是否低于洗版目标，不自动洗；可在季标题「标注片源」或手动选种替换`,
      });
    } else {
      M.push({
        lab: "洗版",
        state: "done",
        time: "",
        det: `当前 ${w.upgrade.current_label}`,
      });
    }
  }
  return M;
}

/** 卡点 = 链上第一个非 done 的站；全 done（收齐且无洗版任务）时亮终点站。 */
export function stuckIndex(chain: Milestone[]): number {
  const i = chain.findIndex((m) => m.state !== "done");
  return i < 0 ? chain.length - 1 : i;
}

/** 实时快照 → 一行进度说明。词表见 SubscriptionDownload.state。 */
export function downloadNote(d: SubscriptionDownload): string {
  if (d.state === "missing") {
    return "种子已不在下载器中（可能被手动删除），稍后自动重新寻找资源";
  }
  const pct = d.progress != null ? `${Math.floor(d.progress * 100)}%` : "";
  if (d.state === "completed") return "已下载完成，等待整理入库";
  if (d.state === "paused") return `${pct} · 已在下载器中暂停`;
  if (d.state === "error") {
    return `${pct} · 下载器报告任务出错；换源判定已暂停，请在下载器中处理`;
  }
  if (d.state === "stalled") return `${pct} · 等待连接做种`;
  const parts = [pct];
  if (d.dlspeed_bytes != null && d.dlspeed_bytes > 0) {
    parts.push(`${formatBytes(d.dlspeed_bytes)}/s`);
  }
  if (d.size_bytes != null && parts.length === 1) parts.push(formatBytes(d.size_bytes));
  return parts.filter(Boolean).join(" · ");
}

/** 资源时间链 → 用户能直接感知的“隔了多久才拉到”。 */
export function resourceTimingNote(
  timing: ResourceTiming | null,
  previous: boolean,
): string | null {
  if (!timing || !shouldShowResourceTiming(timing.publish_to_submit_seconds)) return null;
  const delay = (seconds: number) => (seconds < 60 ? "不到 1 分钟" : formatDuration(seconds));
  const prefix = `${previous ? "上次 · " : ""}${timing.dry_run ? "模拟 · " : ""}${timing.site_id}：`;
  if (
    timing.publish_to_seen_seconds != null &&
    timing.seen_to_submit_seconds != null &&
    timing.publish_to_submit_seconds != null
  ) {
    return (
      `${prefix}资源发布后 ${delay(timing.publish_to_seen_seconds)}进入索引，` +
      `${delay(timing.seen_to_submit_seconds)}后提交下载器` +
      `（总耗时 ${delay(timing.publish_to_submit_seconds)}）`
    );
  }
  if (timing.publish_to_submit_seconds != null) {
    return `${prefix}资源发布后 ${delay(timing.publish_to_submit_seconds)}提交下载器`;
  }
  if (timing.seen_to_submit_seconds != null) {
    return `${prefix}进入索引后 ${delay(timing.seen_to_submit_seconds)}提交下载器`;
  }
  return null;
}

export function wantedPresentation(
  w: WantedItem,
  now: Date = new Date(),
): { label: string; color: string; note: string } {
  if (w.status === "imported") {
    if (w.upgrade?.active) {
      return {
        label: "洗版中",
        color: "#2dd4bf",
        note: `当前 ${w.upgrade.current_label} → 目标 ${w.upgrade.target_label}`,
      };
    }
    if (w.upgrade?.indeterminate) {
      return {
        label: "已入库",
        color: "var(--ok)",
        note: `${w.upgrade.current_label} · 无法确认是否低于洗版目标，不自动洗；可在季标题「标注片源」，或手动选种替换`,
      };
    }
    if (w.upgrade) {
      const label = w.upgrade.current_label ? `${w.upgrade.current_label} · ` : "";
      return {
        label: "已入库",
        color: "var(--ok)",
        note: w.imported_at ? `${label}入库于 ${formatDateTime(w.imported_at)}` : `${label}已入库`,
      };
    }
    return {
      label: "已入库",
      color: "var(--ok)",
      note: w.imported_at ? `入库于 ${formatDateTime(w.imported_at)}` : "已入库",
    };
  }
  if (w.status === "downloaded") {
    return {
      label: "已下载",
      color: "var(--ok)",
      note: `完成于 ${formatDateTime(w.downloaded_at ?? w.grabbed_at)}，待整理入库`,
    };
  }
  if (w.status === "grabbed") {
    return {
      label: "已提交下载",
      color: "var(--ok)",
      note: `${formatRelativeTime(w.grabbed_at)}提交给下载器`,
    };
  }
  if (w.next_search_at === null) {
    if (w.last_search_at) {
      return {
        label: "搜索中",
        color: "var(--info)",
        note: `已搜索（上次搜索 ${formatRelativeTime(w.last_search_at)}），等待资源命中`,
      };
    }
    return {
      label: "追踪中",
      color: "var(--info)",
      note: "已纳入追踪范围，周期自动搜索",
    };
  }
  const due = new Date(w.next_search_at);
  const forecast = w.release_forecast;
  if (
    w.air_date &&
    forecast?.version === 1 &&
    forecast.target_air_date === w.air_date &&
    forecast.confidence !== "volatile" &&
    forecast.sites.length > 0 &&
    new Date(forecast.window_end) >= now
  ) {
    const sites = forecast.sites
      .slice(0, 2)
      .map((site) => site.site_id)
      .join("、");
    return {
      label: "预测窗口",
      color: "var(--warn)",
      note: `预计 ${formatDateTime(forecast.window_start)} ～ ${formatDateTime(forecast.window_end)}，重点探测 ${sites}`,
    };
  }
  if (w.air_date && new Date(w.air_date) > now) {
    return {
      label: "待播出",
      color: "var(--warn)",
      note: `${w.air_date} 播出，${formatDateTime(w.next_search_at)} 起兜底搜索`,
    };
  }
  if (due <= now) {
    return {
      label: "排队搜索",
      color: "var(--info)",
      note: w.last_search_at
        ? `上次搜索 ${formatRelativeTime(w.last_search_at)}`
        : "等待搜索任务执行",
    };
  }
  if (!w.last_search_at && due.getTime() - now.getTime() > 60 * 60 * 1000) {
    return {
      label: "待搜索",
      color: "#f5c451",
      note: `档期未到，${formatDateTime(w.next_search_at)} 起兜底搜索`,
    };
  }
  return {
    label: "冷却中",
    color: "var(--info)",
    note: `暂无合适资源，${formatDateTime(w.next_search_at)} 再试`,
  };
}

export function weekdayLabel(weekday: number): string {
  return ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][weekday] ?? "";
}
