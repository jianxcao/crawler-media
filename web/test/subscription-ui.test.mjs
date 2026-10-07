import assert from "node:assert/strict";
import test from "node:test";

import {
  groupTodayArrivals,
  subscriptionCollectionMeta,
  subscriptionFullyCollected,
  subscriptionRibbon,
  todayArrivalPresentation,
} from "../lib/subscription-ui.ts";

// 新契约订阅：状态由字段派生（不再下发 status / season_collection），
// 季号取 coverage.season，进度用订阅级 progress 快照。
function subscription({
  kind = "tv",
  season = 1,
  followFuture = true,
  trackingState = "active",
  total = 0,
  imported = 0,
  missing = 0,
  grabbing = 0,
  downloaded = 0,
} = {}) {
  return {
    media: { kind },
    coverage:
      kind === "tv"
        ? { kind: "tv", season, episode_from: 1, episode_to: null }
        : { kind: "movie" },
    tracking_state: trackingState,
    follow_future: followFuture,
    progress: { total, imported, missing, grabbing, downloaded },
  };
}

test("追新中的剧集角标不随工单阶段变化，仍是自动续订", () => {
  for (const progress of [
    { total: 2, missing: 2 },
    { total: 2, missing: 1, grabbing: 1 },
    { total: 2, missing: 1, downloaded: 1 },
    { total: 12, imported: 12 },
  ]) {
    assert.deepEqual(subscriptionRibbon(subscription(progress)), {
      label: "自动续订",
      tone: "subscribed",
    });
  }
});

test("创建时内容已在库且没有工单也显示自动续订", () => {
  assert.deepEqual(subscriptionRibbon(subscription()), {
    label: "自动续订",
    tone: "subscribed",
  });
});

test("关闭自动续订、又没收齐的订阅不显示角标", () => {
  assert.equal(subscriptionRibbon(subscription({ followFuture: false })), undefined);
  assert.equal(
    subscriptionRibbon(subscription({ kind: "movie", total: 1, missing: 1 })),
    undefined,
  );
});

// --- issue #221：订阅墙要一眼看出哪些已经到手 ---

test("已入库的电影订阅打「已入库」角标", () => {
  assert.deepEqual(
    subscriptionRibbon(subscription({ kind: "movie", total: 1, imported: 1 })),
    { label: "已入库", tone: "owned" },
  );
});

test("电影只要还没入库就不打已入库，哪怕已经在下载", () => {
  assert.equal(
    subscriptionRibbon(subscription({ kind: "movie", total: 1, downloaded: 1 })),
    undefined,
  );
  assert.equal(
    subscriptionRibbon(subscription({ kind: "movie", total: 1, grabbing: 1 })),
    undefined,
  );
});

test("收齐且不再追新的剧说「已收齐」，并压过自动续订", () => {
  assert.deepEqual(
    subscriptionRibbon(
      subscription({ total: 24, imported: 24, followFuture: false }),
    ),
    { label: "已收齐", tone: "owned" },
  );
});

test("追新剧永远不算收齐——它确实还没完", () => {
  // 已经入库 12 集，但订阅仍在追新（follow_future）：不能盖上"到手了"的章
  assert.deepEqual(subscriptionRibbon(subscription({ total: 12, imported: 12 })), {
    label: "自动续订",
    tone: "subscribed",
  });
  assert.equal(
    subscriptionFullyCollected(subscription({ total: 12, imported: 12 })),
    false,
  );
});

test("不再追新但一集都没入库时不算收齐", () => {
  assert.equal(
    subscriptionFullyCollected(subscription({ followFuture: false, imported: 0 })),
    false,
  );
});

test("在播剧展示覆盖季与收录进度", () => {
  const result = subscriptionCollectionMeta(
    subscription({ season: 2, total: 7, imported: 5, missing: 2 }),
  );
  assert.deepEqual(result, { label: "第 2 季", value: "5 / 7", tracking: true });
});

test("全部收齐的剧按总数展示且不再亮追更绿点", () => {
  const result = subscriptionCollectionMeta(
    subscription({ season: 8, total: 73, imported: 73, followFuture: false }),
  );
  assert.deepEqual(result, { label: "第 8 季", value: "73 / 73", tracking: false });
});

test("开放窗口且缺集时按总数展示已收录", () => {
  const result = subscriptionCollectionMeta(
    subscription({ season: 3, total: 30, imported: 24, missing: 6 }),
  );
  assert.deepEqual(result, { label: "第 3 季", value: "24 / 30", tracking: true });
});

test("总数未知但缺集时直接说缺多少", () => {
  const result = subscriptionCollectionMeta(
    subscription({ season: 1, total: 0, imported: 0, missing: 6 }),
  );
  assert.deepEqual(result, { label: "第 1 季", value: "缺 6 集", tracking: true });
});

test("最新一季尚未播出时不展示零进度", () => {
  const result = subscriptionCollectionMeta(subscription({ season: 3 }));
  assert.deepEqual(result, { label: "第 3 季", value: "待开播", tracking: true });
});

test("暂停的订阅不亮追更绿点", () => {
  const paused = subscriptionCollectionMeta(
    subscription({ season: 2, total: 7, imported: 5, missing: 2, trackingState: "paused" }),
  );
  assert.equal(paused.tracking, false);

  const active = subscriptionCollectionMeta(
    subscription({ season: 2, total: 7, imported: 5, missing: 2 }),
  );
  assert.equal(active.tracking, true);
});

test("电影不展示收录摘要", () => {
  assert.equal(subscriptionCollectionMeta(subscription({ kind: "movie" })), undefined);
});

function todayArrival(overrides = {}) {
  return {
    subscription_id: 1,
    wanted_id: 10,
    media_title: "测试剧集",
    media_kind: "tv",
    season_number: 2,
    episode_number: 5,
    status: "wanted",
    air_date: "2030-01-01",
    expected_day: "2030-01-01",
    days_ahead: 0,
    release_forecast: null,
    next_probe_at: null,
    info_hash: null,
    grabbed_at: null,
    downloaded_at: null,
    estimated_release_to_import_minutes: 60,
    estimated_download_to_import_minutes: 10,
    ...overrides,
  };
}

test("待播集只展示按历史耗时换算后的预计入库时间", () => {
  const now = new Date("2030-01-01T10:00:00Z");
  const predictedAt = "2030-01-01T12:00:00Z";
  const result = todayArrivalPresentation(
    todayArrival({
      release_forecast: {
        predicted_at: predictedAt,
        window_end: "2030-01-01T14:00:00Z",
      },
    }),
    undefined,
    now,
  );
  const expectedClock = new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date("2030-01-01T13:00:00Z"));
  assert.equal(result.statusLabel, "预计入库");
  assert.equal(result.timeLabel, `约 ${expectedClock}`);
});

test("首个预计入库时间过期后滚动到下一探测点", () => {
  const now = new Date("2030-01-01T13:10:00Z");
  const result = todayArrivalPresentation(
    todayArrival({
      release_forecast: {
        predicted_at: "2030-01-01T12:00:00Z",
        window_end: "2030-01-01T14:00:00Z",
      },
      next_probe_at: "2030-01-01T13:30:00Z",
    }),
    undefined,
    now,
  );

  assert.equal(result.statusLabel, "等待资源");
  assert.equal(result.estimatedAt, Date.parse("2030-01-01T14:30:00Z"));
});

test("所有探测点对应的入库时间都过期后不展示旧时间", () => {
  const result = todayArrivalPresentation(
    todayArrival({
      release_forecast: {
        predicted_at: "2030-01-01T12:00:00Z",
        window_end: "2030-01-01T14:00:00Z",
      },
    }),
    undefined,
    new Date("2030-01-01T14:00:00Z"),
  );

  assert.equal(result.statusLabel, "等待资源");
  assert.equal(result.timeLabel, "时间待更新");
  assert.equal(result.estimatedAt, null);
});

test("下载中不展示进度，入库时刻回任务中心看", () => {
  // 新契约下载任务没有 ETA：状态保持在「下载中」，不再假装推算时间。
  const now = new Date("2030-01-01T10:00:00Z");
  const result = todayArrivalPresentation(
    todayArrival({ status: "grabbed", info_hash: "abc" }),
    { state: "downloading" },
    now,
  );
  assert.equal(result.statusLabel, "下载中");
  assert.equal(result.timeLabel, "时间待更新");
  assert.equal(result.estimatedAt, null);
  assert.equal("progress" in result, false);
});

test("下载完成后首页收敛为整理中", () => {
  const result = todayArrivalPresentation(todayArrival({ status: "downloaded" }));
  assert.equal(result.statusLabel, "整理中");
  assert.equal(result.timeLabel, "即将完成");
});

function presented(
  arrival,
  presentation = { statusLabel: "预计入库", timeLabel: "时间待更新", estimatedAt: null },
) {
  return { arrival, presentation };
}

test("同一部剧同日连续更新多集时合并为一个范围", () => {
  const groups = groupTodayArrivals([
    presented(todayArrival({ wanted_id: 16, episode_number: 16 })),
    presented(todayArrival({ wanted_id: 17, episode_number: 17 })),
    presented(todayArrival({ wanted_id: 20, episode_number: 20 })),
    presented(todayArrival({ wanted_id: 18, episode_number: 18 })),
    presented(todayArrival({ wanted_id: 19, episode_number: 19 })),
  ]);

  assert.equal(groups.length, 1);
  assert.equal(groups[0].episodeLabel, "S02E16–E20");
  assert.equal(groups[0].episodeCount, 5);
});

test("非连续集数和跨季更新不会被误写成连续范围", () => {
  const groups = groupTodayArrivals([
    presented(todayArrival({ episode_number: 16 })),
    presented(todayArrival({ wanted_id: 12, episode_number: 18 })),
    presented(todayArrival({ wanted_id: 13, season_number: 3, episode_number: 1 })),
    presented(todayArrival({ wanted_id: 14, season_number: 3, episode_number: 2 })),
  ]);

  assert.equal(groups[0].episodeLabel, "S02E16、E18 · S03E01–E02");
});

test("合并行使用尚未完成且预计最晚的一集作为整体状态", () => {
  const groups = groupTodayArrivals([
    presented(todayArrival({ wanted_id: 10, status: "downloaded" }), {
      statusLabel: "整理中",
      timeLabel: "即将完成",
      estimatedAt: 100,
    }),
    presented(todayArrival({ wanted_id: 11, episode_number: 6, status: "grabbed" }), {
      statusLabel: "下载中",
      timeLabel: "约 20:00",
      estimatedAt: 200,
    }),
    presented(todayArrival({ wanted_id: 12, episode_number: 7 }), {
      statusLabel: "等待资源",
      timeLabel: "时间待更新",
      estimatedAt: null,
    }),
  ]);

  assert.equal(groups[0].presentation.statusLabel, "等待资源");
  assert.equal(groups[0].presentation.timeLabel, "时间待更新");
});

test("给不出入库时刻时优先报下次探测时刻，让用户看到系统在动", () => {
  const result = todayArrivalPresentation(
    todayArrival({
      release_forecast: {
        predicted_at: "2030-01-01T12:00:00Z",
        window_end: "2030-01-01T14:00:00Z",
        confidence: "volatile",
      },
      next_probe_at: "2030-01-01T13:30:00Z",
    }),
    undefined,
    new Date("2030-01-01T10:00:00Z"),
  );
  const expectedClock = new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date("2030-01-01T13:30:00Z"));

  assert.equal(result.estimatedAt, null);
  assert.equal(result.timeLabel, `${expectedClock} 探测`);
});

test("未来预告没有可用时刻时退到播出日期，而不是一句时间待更新", () => {
  const result = todayArrivalPresentation(
    todayArrival({ expected_day: "2030-01-04", days_ahead: 3 }),
    undefined,
    new Date("2030-01-01T10:00:00Z"),
  );

  assert.equal(result.statusLabel, "预计入库");
  assert.equal(result.timeLabel, "1月4日 播出");
});

test("几天后的预告不写只有时分的探测时刻，避免被当成今天", () => {
  const result = todayArrivalPresentation(
    todayArrival({
      expected_day: "2030-01-04",
      days_ahead: 3,
      next_probe_at: "2030-01-04T13:30:00Z",
    }),
    undefined,
    new Date("2030-01-01T10:00:00Z"),
  );

  assert.equal(result.timeLabel, "1月4日 播出");
});

test("电影不展示 S00E00 哨兵季集号", () => {
  const groups = groupTodayArrivals([
    presented(
      todayArrival({
        media_kind: "movie",
        media_title: "测试电影",
        season_number: 0,
        episode_number: 0,
        status: "grabbed",
      }),
      { statusLabel: "下载中", timeLabel: "约 20:00", estimatedAt: 200 },
    ),
  ]);

  assert.equal(groups[0].episodeLabel, "电影");
  assert.equal(groups[0].daysAhead, 0);
});

test("两个不同 UUID 订阅调用 groupTodayArrivals 得到两个独立分组且 subscriptionId 不生成 NaN", () => {
  const uuid1 = "11111111-1111-4111-8111-111111111111";
  const uuid2 = "22222222-2222-4222-8222-222222222222";
  const groups = groupTodayArrivals([
    presented(
      todayArrival({
        subscription_id: uuid1,
        wanted_id: "w-1",
        media_title: "剧集 A",
        season_number: 1,
        episode_number: 1,
      }),
      { statusLabel: "等待中", timeLabel: "今日", estimatedAt: 100 },
    ),
    presented(
      todayArrival({
        subscription_id: uuid2,
        wanted_id: "w-2",
        media_title: "剧集 B",
        season_number: 1,
        episode_number: 2,
      }),
      { statusLabel: "等待中", timeLabel: "今日", estimatedAt: 200 },
    ),
  ]);

  assert.equal(groups.length, 2, "两个不同 UUID 订阅必须形成两个独立分组");
  assert.equal(groups[0].subscriptionId, uuid1);
  assert.equal(groups[1].subscriptionId, uuid2);
  assert.equal(groups[0].mediaTitle, "剧集 A");
  assert.equal(groups[1].mediaTitle, "剧集 B");
  assert.notEqual(groups[0].subscriptionId, "NaN");
});
