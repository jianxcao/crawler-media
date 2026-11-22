import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

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

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

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

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeSubscriptionDetail(id, title = `Title ${id}`) {
  return {
    id,
    media: { id: `media-${id}`, kind: "tv", title, poster_url: null },
    coverage: { kind: "tv", season: 1, episode_from: 1, episode_to: 10 },
    fetch_mode: "both",
    filter_id: "f1",
    wash_cut: false,
    keep_old_versions: false,
    full_season_pack: false,
    tracking_state: "active",
    follow_future: false,
    search_interval_secs: 3600,
    progress: { total: 10, imported: 0, missing: 10, grabbing: 0, downloaded: 0 },
    wanted: [],
  };
}

function mountInspectorHook(overrides = {}) {
  const toastErrors = [];
  const toastSuccesses = [];
  const js = ts.transpileModule(
    readFileSync(path.join(webRoot, "components/subscription-inspector/use-subscription-inspector.ts"), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
      },
    },
  ).outputText;
  const mod = { exports: {} };
  const subApiStub = {
    getSubscription: async (id) => makeSubscriptionDetail(id),
    listSubscriptionActivities: async () => [],
    listRuleSets: async () => [],
    fetchReleaseForecast: async () => ({ days: [] }),
    listActiveSubscriptionDownloads: async () => [],
    setSubscriptionTrackingState: async () => {},
    setSubscriptionFollowFuture: async () => {},
    setSubscriptionKeepOldVersions: async () => {},
    searchMissingSubscriptionResources: async () => ({ reset_count: 0 }),
    unsubscribeFromSubscription: async () => {},
    deleteSubscriptionPermanently: async () => {},
    setSubscriptionFetchMode: async () => {},
    setSubscriptionSearchInterval: async () => {},
    updateSubscription: async () => {},
    ...overrides,
  };
  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "react-router-dom") return { useNavigate: () => () => {} };
      if (name === "@/components/feedback") {
        return {
          useConfirm: () => async () => true,
          useToast: () => ({
            error: (msg) => toastErrors.push(msg),
            success: (msg) => toastSuccesses.push(msg),
            info: () => {},
          }),
        };
      }
      if (name === "@/components/subscribe-entry") {
        return { useSubscribeEntry: () => ({ canSubscribe: true, refresh: () => {} }) };
      }
      if (name === "@/lib/api/playback") {
        return { listUpNext: async () => [], setPlaybackMarks: async () => {} };
      }
      if (name === "@/lib/api/subscriptions") return subApiStub;
      if (name === "@/lib/back-navigation") return { useBackNavigation: () => () => {} };
      if (name === "@/lib/image-proxy") return { imageUrl: (u) => u };
      if (name === "@/lib/jobs") return { useJobs: () => ({ jobs: [] }) };
      if (name === "@/lib/permissions") {
        return { usePermissions: () => ({ canManageSubscriptions: true, canSearch: true, isAdmin: false }) };
      }
      if (name === "@/lib/subscription-ui") {
        return {
          subscriptionStatus: (d) => (d.tracking_state === "paused" ? "paused" : "active"),
          subscriptionStatusMeta: { active: { label: "追踪中" }, paused: { label: "已暂停" }, completed: { label: "已收齐" } },
        };
      }
      if (name === "@/lib/use-page-title") return { usePageTitle: () => {} };
      if (name === "@/lib/use-visible-polling") return { useVisiblePolling: () => {} };
      return require(name);
    },
    mod,
    mod.exports,
  );

  let latest;
  function Harness({ id }) {
    latest = mod.exports.useSubscriptionInspector({ id, autoOpenUpgradeRun: false });
    return null;
  }
  let renderer;
  return {
    toastErrors,
    toastSuccesses,
    latest: () => latest,
    async mount(id) {
      await act(async () => {
        renderer = create(React.createElement(Harness, { id }));
      });
    },
    async update(id) {
      await act(async () => {
        renderer.update(React.createElement(Harness, { id }));
      });
    },
    async unmount() {
      await act(async () => {
        renderer.unmount();
      });
    },
  };
}

test("useSubscriptionInspector resets state on id change and discards stale out-of-order reload responses", async () => {
  const firstReq = deferred();
  const secondReq = deferred();
  const reloadTwo = deferred();
  let calls = 0;
  const h = mountInspectorHook({
    getSubscription: (id) => {
      calls += 1;
      if (id === "sub-1") return firstReq.promise;
      if (calls === 2) return secondReq.promise;
      return reloadTwo.promise;
    },
  });

  await h.mount("sub-1");
  try {
    // Switch to sub-2 while sub-1 is still in flight
    await h.update("sub-2");
    assert.equal(h.latest().detail, null);
    assert.equal(h.latest().failed, false);

    // Late rejection or resolution from sub-1 must NOT set failed/detail on sub-2
    await act(async () => {
      firstReq.reject(new Error("sub-1 failed"));
    });
    assert.equal(h.latest().failed, false, "late error from old id must be ignored");

    // Resolve sub-2
    await act(async () => {
      secondReq.resolve(makeSubscriptionDetail("sub-2", "Current Show"));
    });
    assert.equal(h.latest().detail?.id, "sub-2");

    // Now test same-id reload vs earlier out-of-order reload
    const firstSameId = deferred();
    const secondSameId = deferred();
    let seqCalls = 0;
    const h2 = mountInspectorHook({
      getSubscription: () => (++seqCalls === 1 ? firstSameId.promise : secondSameId.promise),
    });
    await h2.mount("sub-x");
    await act(async () => { h2.latest().reload(); });
    await act(async () => {
      secondSameId.resolve(makeSubscriptionDetail("sub-x", "Newer Snapshot"));
    });
    assert.equal(h2.latest().detail?.media.title, "Newer Snapshot");
    await act(async () => {
      firstSameId.resolve(makeSubscriptionDetail("sub-x", "Stale Snapshot"));
    });
    assert.equal(
      h2.latest().detail?.media.title,
      "Newer Snapshot",
      "older out-of-order reload response must not overwrite newer state",
    );
    await h2.unmount();

    // Switching back to a third id clears previous detail immediately
    const thirdReq = deferred();
    calls = 0;
    await h.update("sub-3");
    assert.equal(h.latest().detail, null, "switching id must clear previous detail");
    assert.equal(h.latest().failed, false);
    thirdReq.resolve(makeSubscriptionDetail("sub-3"));
  } finally {
    await h.unmount();
  }
});

test("useSubscriptionInspector catches errors and clears busy in togglePause, remove, handleFetchModeChange, and handleSearchIntervalChange", async () => {
  const h = mountInspectorHook({
    setSubscriptionTrackingState: async () => { throw new Error("pause boom"); },
    unsubscribeFromSubscription: async () => { throw new Error("remove boom"); },
    setSubscriptionFetchMode: async () => { throw new Error("mode boom"); },
    setSubscriptionSearchInterval: async () => { throw new Error("interval boom"); },
  });

  await h.mount("sub-1");
  try {
    await act(async () => { await h.latest().togglePause(); });
    assert.equal(h.latest().busy, false);
    await act(async () => { await h.latest().remove(); });
    assert.equal(h.latest().busy, false);
    await act(async () => { h.latest().handleFetchModeChange("rss"); });
    assert.equal(h.latest().busy, false);
    await act(async () => { h.latest().handleSearchIntervalChange(7200); });
    assert.equal(h.latest().busy, false);

    assert.deepEqual(h.toastErrors, ["pause boom", "remove boom", "mode boom", "interval boom"]);
  } finally {
    await h.unmount();
  }
});
