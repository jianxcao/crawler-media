import assert from "node:assert/strict";
import test from "node:test";

import {
  activityColor,
  defaultOpenSeasons,
  downloadNote,
  milestonesOf,
  pendingFailures,
  resourceTimingNote,
  SHORT_STATUS,
  stuckIndex,
  wantedPresentation,
  weekdayLabel,
} from "../lib/subscription-inspector.ts";

const makeWanted = (overrides = {}) => ({
  id: "w1",
  season_number: 1,
  episode_number: 1,
  title: null,
  air_date: "2025-01-01",
  status: "wanted",
  last_search_at: null,
  next_search_at: null,
  search_attempts: 0,
  last_reject_reason: null,
  grab_title: null,
  grabbed_at: null,
  downloaded_at: null,
  imported_at: null,
  info_hash: null,
  resource_timing: null,
  release_forecast: null,
  upgrade: null,
  ...overrides,
});

test("pendingFailures tracks only latest unresolved failures per wanted item", () => {
  const activities = [
    { id: "a0", wanted_item_id: null, type: "import_failed", message: "ignored" },
    { id: "a1", wanted_item_id: "w1", type: "downloaded", message: "newer success" },
    { id: "a2", wanted_item_id: "w1", type: "import_failed", message: "stale fail" },
    { id: "a3", wanted_item_id: "w2", type: "import_failed", message: "still broken" },
    { id: "a4", wanted_item_id: "w3", type: "dispatch_failed", message: "retry later" },
  ];
  const map = pendingFailures(activities);
  assert.equal(map.has("w1"), false);
  assert.equal(map.get("w2")?.id, "a3");
  assert.equal(map.get("w3")?.id, "a4");
  assert.equal(map.size, 2);
});

test("defaultOpenSeasons opens latest regular season plus in-flight seasons", () => {
  assert.deepEqual([...defaultOpenSeasons([])], []);
  assert.deepEqual(
    [...defaultOpenSeasons([makeWanted({ season_number: 0 }), makeWanted({ season_number: 0 })])],
    [0],
  );
  const open = defaultOpenSeasons([
    makeWanted({ id: "w0", season_number: 0, status: "wanted" }),
    makeWanted({ id: "w1", season_number: 1, status: "grabbed" }),
    makeWanted({ id: "w2", season_number: 2, status: "imported", upgrade: { active: true } }),
    makeWanted({ id: "w3", season_number: 3, status: "downloaded" }),
    makeWanted({ id: "w4", season_number: 5, status: "wanted" }),
  ]);
  assert.deepEqual([...open].sort((a, b) => a - b), [1, 3, 5]);
});

test("downloadNote formats all download states cleanly", () => {
  assert.match(downloadNote({ state: "missing", progress: 0.5 }), /不在下载器中/);
  assert.equal(downloadNote({ state: "completed", progress: 1 }), "已下载完成，等待整理入库");
  assert.equal(downloadNote({ state: "paused", progress: 0.42 }), "42% · 已在下载器中暂停");
  assert.match(downloadNote({ state: "error", progress: 0.8 }), /^80% · 下载器报告任务出错/);
  assert.equal(downloadNote({ state: "stalled", progress: 0.12 }), "12% · 等待连接做种");
  assert.equal(
    downloadNote({ state: "downloading", progress: 0.5, dlspeed_bytes: 2 * 1024 * 1024 }),
    "50% · 2.00 MB/s",
  );
  assert.equal(
    downloadNote({ state: "downloading", progress: 0.5, dlspeed_bytes: 0, size_bytes: 1024 ** 3 }),
    "50% · 1.00 GB",
  );
});

test("resourceTimingNote enforces 7-day window and renders prefixes", () => {
  assert.equal(resourceTimingNote(null, false), null);
  assert.equal(
    resourceTimingNote({ publish_to_submit_seconds: 7 * 86400 + 1, site_id: "mteam", dry_run: false }, false),
    null,
  );
  assert.equal(
    resourceTimingNote(
      {
        site_id: "mteam",
        dry_run: true,
        publish_to_seen_seconds: 30,
        seen_to_submit_seconds: 120,
        publish_to_submit_seconds: 150,
      },
      true,
    ),
    "上次 · 模拟 · mteam：资源发布后 不到 1 分钟进入索引，2 分钟后提交下载器（总耗时 3 分钟）",
  );
});

test("wantedPresentation, milestonesOf, stuckIndex, and helper lookups cover key states", () => {
  const now = new Date("2026-05-01T12:00:00Z");

  // Undated movie wanted
  const undatedMovie = makeWanted({ air_date: null, next_search_at: null, status: "wanted" });
  const movieChain = milestonesOf(undatedMovie, true, undefined, undefined, now);
  assert.equal(movieChain[0].lab, "上映");
  assert.equal(movieChain[0].state, "now");
  assert.equal(stuckIndex(movieChain), 0);

  // TV pre-air & forecast window & pending > 1h & cooldown
  assert.equal(
    wantedPresentation(makeWanted({ air_date: "2026-06-01", next_search_at: "2026-06-02T00:00:00Z" }), now).label,
    "待播出",
  );
  assert.equal(
    wantedPresentation(
      makeWanted({
        air_date: "2026-05-02",
        next_search_at: "2026-05-02T00:00:00Z",
        release_forecast: {
          version: 1,
          target_air_date: "2026-05-02",
          confidence: "steady",
          window_start: "2026-05-01T20:00:00Z",
          window_end: "2026-05-02T02:00:00Z",
          sites: [{ site_id: "pt1" }, { site_id: "pt2" }],
        },
      }),
      now,
    ).label,
    "预测窗口",
  );
  assert.equal(
    wantedPresentation(makeWanted({ next_search_at: "2026-05-01T10:00:00Z" }), now).label,
    "排队搜索",
  );
  assert.equal(
    wantedPresentation(makeWanted({ next_search_at: "2026-05-01T15:00:00Z" }), now).label,
    "待搜索",
  );
  assert.equal(
    wantedPresentation(
      makeWanted({ last_search_at: "2026-05-01T11:30:00Z", next_search_at: "2026-05-01T15:00:00Z" }),
      now,
    ).label,
    "冷却中",
  );

  // Import failure milestone overrides imported state and lights up fail node
  const importFailChain = milestonesOf(
    makeWanted({
      status: "downloaded",
      grabbed_at: "2026-04-30T10:00:00Z",
      downloaded_at: "2026-04-30T11:00:00Z",
    }),
    false,
    undefined,
    { id: "f1", type: "import_failed", message: "目标路径不存在" },
    now,
  );
  assert.equal(stuckIndex(importFailChain), 4);
  assert.equal(importFailChain[4].state, "fail");
  assert.equal(importFailChain[4].why, "目标路径不存在");

  // Imported with upgrade variants
  assert.equal(
    wantedPresentation(
      makeWanted({ status: "imported", upgrade: { active: true, current_label: "1080p", target_label: "4K" } }),
      now,
    ).label,
    "洗版中",
  );
  assert.match(
    wantedPresentation(
      makeWanted({ status: "imported", upgrade: { active: false, indeterminate: true, current_label: "未知" } }),
      now,
    ).note,
    /无法确认/,
  );

  assert.equal(SHORT_STATUS["已提交下载"], "已提交");
  assert.equal(activityColor("upgrade_grabbed"), "#2dd4bf");
  assert.equal(activityColor("import_failed"), "var(--danger)");
  assert.equal(weekdayLabel(0), "周日");
  assert.equal(weekdayLabel(6), "周六");
});
