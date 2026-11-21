import type {
  PrepareResult,
  SeasonOverview,
  UpgradeRunReport,
} from "./api/subscription-types.ts";

/** 计算订阅弹窗初始化时默认勾选的季号列表。 */
export function computeDefaultSelectedSeasons(
  result: Pick<PrepareResult, "seasons" | "suggested_seasons">,
  upgradeIntent = false,
): number[] {
  const suggested = result.suggested_seasons ?? [];
  if (upgradeIntent) {
    return result.seasons.filter((s) => s.owned_count > 0).map((s) => s.season_number);
  }
  if (suggested.length > 0) return suggested;
  return result.seasons
    .filter((s) => s.season_number > 0 && s.aired_count > 0)
    .map((s) => s.season_number);
}

/** 季行里的播出进度文案。 */
export function formatSeasonProgress(
  season: Pick<SeasonOverview, "episode_count" | "aired_count">,
): string {
  const total = season.episode_count ?? 0;
  if (season.aired_count >= total && total > 0) return `全 ${total} 集已播完`;
  if (total > 0) return `已播 ${season.aired_count}/${total} 集`;
  if (season.aired_count > 0) return `已播 ${season.aired_count} 集`;
  return "未播出";
}

/** 季行里的媒体库库存提示；库中无集时返回 null。 */
export function formatSeasonOwned(
  season: Pick<SeasonOverview, "episode_count" | "owned_count">,
): string | null {
  const total = season.episode_count ?? 0;
  if (season.owned_count <= 0) return null;
  return season.owned_count >= total && total > 0
    ? "整季已在库"
    : `库里已有 ${season.owned_count} 集`;
}

/** 汇总多季洗版体检结果；`totalCount` 为请求触发的总季数。 */
export function mergeUpgradeRunReports(
  reports: UpgradeRunReport[],
  totalCount: number,
): UpgradeRunReport {
  const failed = totalCount - reports.length;
  const first = reports[0];
  return {
    ...first,
    summary:
      failed > 0
        ? `已为 ${reports.length} 季触发洗版检查，另有 ${failed} 季失败，请到订阅详情重试`
        : reports.length === 1
          ? first.summary
          : `已为 ${reports.length} 季触发洗版检查`,
    counts: reports.reduce(
      (counts, report) => {
        for (const key of Object.keys(counts) as Array<keyof typeof counts>) {
          counts[key] += report.counts[key];
        }
        return counts;
      },
      { upgradable: 0, at_cutoff: 0, in_flight: 0, not_comparable: 0, missing: 0 },
    ),
    units: reports.flatMap((report) => report.units),
  };
}
