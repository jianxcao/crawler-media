import assert from "node:assert/strict";
import test from "node:test";

import {
  FILTER_DIMS,
  buildFilterConditionGroups,
  dropFilterValue,
  facetLabelOf,
  hasSecondaryFilter,
} from "../lib/library-filter-helpers.ts";
import {
  KIND_ORDER,
  computeDropPosition,
  formatManageBytes,
} from "../lib/library-manage-helpers.ts";
import {
  filterHistoryGroups,
  groupHistory,
  isMediaHistory,
  parseSearchPaletteState,
  tabKeyOf,
} from "../lib/search-palette.ts";
import {
  computeDefaultSelectedSeasons,
  formatSeasonOwned,
  formatSeasonProgress,
  mergeUpgradeRunReports,
} from "../lib/subscribe-dialog-helpers.ts";

test("library-manage helpers order kinds, detect drop half, and format byte sizes", () => {
  assert.deepEqual(KIND_ORDER, ["movie", "tv", "video"]);
  assert.ok(KIND_ORDER.indexOf("movie") < KIND_ORDER.indexOf("tv"));

  assert.equal(computeDropPosition(119, 100, 40), "before");
  assert.equal(computeDropPosition(120, 100, 40), "after");
  assert.equal(computeDropPosition(135, 100, 40), "after");

  assert.equal(formatManageBytes(0), "0 B");
  assert.equal(formatManageBytes(512), "512 B");
  assert.equal(formatManageBytes(1536), "1.5 KB");
  assert.equal(formatManageBytes(200 * 1024), "200 KB");
  assert.equal(formatManageBytes(3.25 * 1024 * 1024), "3.3 MB");
  assert.equal(formatManageBytes(12.5 * 1024 ** 3), "12.5 GB");
});

test("library-filter helpers detect secondary dims, resolve facet labels, drop values, and build groups", () => {
  assert.equal(hasSecondaryFilter({ genres: ["878"], watch: "unwatched" }), false);
  assert.equal(hasSecondaryFilter({ ratingGte: 8 }), true);
  assert.equal(hasSecondaryFilter({ runtimes: ["gt120"] }), true);
  assert.equal(hasSecondaryFilter({ hdr: false }), true);
  assert.equal(hasSecondaryFilter({ stock: ["missing"] }), true);

  const facets = {
    total: 5,
    genres: [{ value: "878", label: "科幻", count: 3 }],
    countries: [{ value: "JP", label: "日本", count: 2 }],
    decades: [],
    watch: [{ value: "unwatched", label: "未看", count: 4 }],
    ratings: [{ value: "8", label: "≥ 8 分", count: 2 }],
    runtimes: [{ value: "gt120", label: "> 120′", count: 1 }],
    languages: [],
    resolutions: [],
    hdr: [{ value: "1", label: "HDR", count: 1 }],
    stock: [],
  };

  assert.equal(facetLabelOf("genres", "878", facets), "科幻");
  assert.equal(facetLabelOf("resolutions", "2160p", null), "2160p");
  assert.equal(facetLabelOf("languages", "ja", null), "ja");
  assert.equal(facetLabelOf("genres", "878", null), "…");
  assert.equal(facetLabelOf("countries", "JP", null), "…");
  assert.equal(facetLabelOf("runtimes", "gt120", null), "…");

  assert.deepEqual(
    dropFilterValue({ genres: ["878", "18"], watch: "unwatched" }, "genres", "878"),
    { genres: ["18"], watch: "unwatched" },
  );
  assert.deepEqual(
    dropFilterValue({ genres: ["878"], watch: "unwatched" }, "watch", "unwatched"),
    { genres: ["878"], watch: null },
  );
  assert.deepEqual(
    dropFilterValue({ ratingGte: 8, hdr: true }, "rating_gte", "8"),
    { ratingGte: null, hdr: true },
  );
  assert.deepEqual(
    dropFilterValue({ ratingGte: 8, hdr: true }, "hdr", "1"),
    { ratingGte: 8, hdr: null },
  );

  const groups = buildFilterConditionGroups({
    genres: ["878"],
    watch: "unwatched",
    ratingGte: 8,
    hdr: true,
    stock: ["missing"],
  });
  assert.equal(FILTER_DIMS.length, 10);
  assert.deepEqual(groups, [
    { key: "genres", label: "类型", values: ["878"] },
    { key: "watch", label: "观看", values: ["unwatched"] },
    { key: "ratingGte", label: "评分", values: ["8"] },
    { key: "hdr", label: "动态范围", values: ["1"] },
    { key: "stock", label: "库存", values: ["missing"] },
  ]);
});

test("search-palette helpers validate stored state, build tab keys, and group/filter history", () => {
  assert.deepEqual(parseSearchPaletteState(null), { mode: "media", tabKey: "all" });
  assert.deepEqual(parseSearchPaletteState("not-json{"), { mode: "media", tabKey: "all" });
  assert.deepEqual(parseSearchPaletteState('{"mode":"invalid","tabKey":""}'), {
    mode: "media",
    tabKey: "all",
  });
  assert.deepEqual(parseSearchPaletteState('{"mode":"torrent","tabKey":"preset:4k"}'), {
    mode: "torrent",
    tabKey: "preset:4k",
  });
  assert.deepEqual(parseSearchPaletteState('{"mode":"library","tabKey":"category:movie"}'), {
    mode: "library",
    tabKey: "category:movie",
  });

  assert.equal(tabKeyOf({ type: "category", id: "movie", visible: true }), "category:movie");
  assert.equal(
    tabKeyOf({
      type: "preset",
      id: "4k",
      name: "4K",
      visible: true,
      categories: [],
      site_ids: [],
      poster_mode: false,
      skip_history: false,
    }),
    "preset:4k",
  );

  const items = [
    { id: "1", query: " Dune ", provider: "douban", created_at: "2026-01-02T00:00:00Z" },
    { id: "2", query: "dune", provider: "torrents", created_at: "2026-01-01T00:00:00Z" },
    { id: "3", query: "Severance", provider: "torrents", created_at: "2025-12-30T00:00:00Z" },
  ];
  assert.equal(isMediaHistory(items[0]), true);
  assert.equal(isMediaHistory(items[1]), false);

  const groups = groupHistory(items);
  assert.equal(groups.length, 2);
  assert.equal(groups[0].key, "dune");
  assert.equal(groups[0].keyword, " Dune ");
  assert.deepEqual(
    groups[0].items.map((item) => item.id),
    ["1", "2"],
  );

  assert.deepEqual(
    filterHistoryGroups(groups, " SEV ").map((group) => group.key),
    ["severance"],
  );
  assert.deepEqual(filterHistoryGroups(groups, "  "), groups);
});

test("subscribe-dialog helpers compute default seasons, format season badges, and merge upgrade reports", () => {
  const prepareResult = {
    status: "ready",
    media: null,
    seasons: [
      { season_number: 0, name: "Specials", air_date: "2024-01-01", episode_count: 2, aired_count: 2, owned_count: 0 },
      { season_number: 1, name: "S1", air_date: "2024-02-01", episode_count: 10, aired_count: 10, owned_count: 10 },
      { season_number: 2, name: "S2", air_date: "2025-02-01", episode_count: 8, aired_count: 5, owned_count: 0 },
      { season_number: 3, name: "S3", air_date: null, episode_count: 8, aired_count: 0, owned_count: 0 },
    ],
    existing_subscription_id: null,
    movie_owned: false,
    suggested_seasons: [],
    candidates: [],
  };

  assert.deepEqual(computeDefaultSelectedSeasons(prepareResult, false), [1, 2]);
  assert.deepEqual(computeDefaultSelectedSeasons(prepareResult, true), [1]);
  assert.deepEqual(
    computeDefaultSelectedSeasons({ ...prepareResult, suggested_seasons: [2] }, false),
    [2],
  );

  assert.equal(formatSeasonProgress(prepareResult.seasons[1]), "全 10 集已播完");
  assert.equal(formatSeasonProgress(prepareResult.seasons[2]), "已播 5/8 集");
  assert.equal(formatSeasonProgress(prepareResult.seasons[3]), "已播 0/8 集");
  assert.equal(formatSeasonProgress({ episode_count: null, aired_count: 0 }), "未播出");
  assert.equal(formatSeasonProgress({ episode_count: null, aired_count: 3 }), "已播 3 集");

  assert.equal(formatSeasonOwned(prepareResult.seasons[1]), "整季已在库");
  assert.equal(formatSeasonOwned({ episode_count: 10, owned_count: 4 }), "库里已有 4 集");
  assert.equal(formatSeasonOwned(prepareResult.seasons[2]), null);

  const single = mergeUpgradeRunReports(
    [
      {
        target_label: "4K",
        filter_id: "r1",
        summary: "第 1 季洗版完成",
        counts: { upgradable: 2, at_cutoff: 1, in_flight: 0, not_comparable: 0, missing: 0 },
        units: [{ season_number: 1, episode_number: 1, state: "upgradable", current_label: null, target_label: "4K" }],
      },
    ],
    1,
  );
  assert.equal(single.summary, "第 1 季洗版完成");
  assert.equal(single.counts.upgradable, 2);

  const multi = mergeUpgradeRunReports(
    [
      {
        target_label: "4K",
        filter_id: "r1",
        summary: "S1",
        counts: { upgradable: 2, at_cutoff: 1, in_flight: 0, not_comparable: 0, missing: 0 },
        units: [{ season_number: 1, episode_number: 1, state: "upgradable", current_label: null, target_label: "4K" }],
      },
      {
        target_label: "4K",
        filter_id: "r1",
        summary: "S2",
        counts: { upgradable: 1, at_cutoff: 3, in_flight: 1, not_comparable: 0, missing: 2 },
        units: [{ season_number: 2, episode_number: 1, state: "missing", current_label: null, target_label: "4K" }],
      },
    ],
    3,
  );
  assert.equal(multi.summary, "已为 2 季触发洗版检查，另有 1 季失败，请到订阅详情重试");
  assert.deepEqual(multi.counts, {
    upgradable: 3,
    at_cutoff: 4,
    in_flight: 1,
    not_comparable: 0,
    missing: 2,
  });
  assert.equal(multi.units.length, 2);
});
