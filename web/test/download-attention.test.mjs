import assert from "node:assert/strict";
import test from "node:test";

import {
  downloadGroupNeedsAttention,
  downloadTaskNeedsAttention,
  groupDownloadTasks,
} from "../lib/download-attention.ts";
import { markJobDismissed } from "../lib/job-attention.ts";

/** 一条下载器实时任务（新契约 DownloadTask：没有 source/can_replace/landing_error）。 */
function task(overrides = {}) {
  return {
    id: "1:abc",
    info_hash: "ABC",
    name: "Some.Torrent",
    downloader_id: "dl-1",
    downloader_name: "qBit",
    progress: 0.5,
    size_bytes: 1000,
    dlspeed_bytes: 10,
    upspeed_bytes: 0,
    uploaded_bytes: 0,
    completed_bytes: 500,
    state: "downloading",
    site_id: "site-1",
    media_title: null,
    media_kind: null,
    subscriptions: [],
    ...overrides,
  };
}

/** 一条订阅投递引用：带媒体身份与集清单——「这部作品被 crawler-media 投递过」的唯一凭据。 */
function subscription({ mediaItemId = "00000000-0000-4000-8000-000000000007", units = [] } = {}) {
  return {
    id: "sub-1",
    media_item_id: mediaItemId,
    media_title: "剧",
    media_kind: "tv",
    poster_url: null,
    purpose: "download",
    upgrade_keep_old: false,
    units,
  };
}

// 真实教训：下载器里几十个陈年 error 种子把「需要处理」塞满，用户点开只看到
// "检查下载器"，而下载器本身是正常的。判定必须区分"谁的任务"和"错在哪"。

test("订阅任务在下载器里报错要用户处理", () => {
  const subscribed = { subscriptions: [subscription()] };
  assert.equal(downloadTaskNeedsAttention(task({ ...subscribed, state: "error" }), null), true);
  assert.equal(downloadTaskNeedsAttention(task({ ...subscribed, state: "missing" }), null), true);
});

test("内容核验证明种子里没有这一集算待办：等它永远等不到", () => {
  const contentMissing = {
    subscriptions: [
      subscription({
        units: [
          {
            season_number: 1,
            episode_number: 3,
            status: "downloaded",
            replaced: false,
            content_missing: true,
          },
        ],
      }),
    ],
  };
  assert.equal(
    downloadTaskNeedsAttention(task({ ...contentMissing, state: "completed" }), null),
    true,
  );
  assert.equal(downloadTaskNeedsAttention(task({ state: "completed" }), null), false);
});

test("手动下载的任务同样受 crawler-media 照看", () => {
  assert.equal(
    downloadTaskNeedsAttention(
      task({ media_title: "电影", media_kind: "movie", state: "error" }),
      null,
    ),
    true,
  );
});

test("外部任务只观察不报警：它没有工单可救、没有入库可推", () => {
  const external = { site_id: null };
  assert.equal(downloadTaskNeedsAttention(task({ ...external, state: "error" }), null), false);
  assert.equal(downloadTaskNeedsAttention(task({ ...external, state: "missing" }), null), false);
});

test("正常下载中的任务不算待办", () => {
  assert.equal(downloadTaskNeedsAttention(task(), null), false);
  assert.equal(downloadTaskNeedsAttention(task({ state: "stalled" }), null), false);
  assert.equal(downloadTaskNeedsAttention(task({ state: "unknown" }), null), false);
});

test("关联入库 Job 失败时算待办，被忽略后不再算", () => {
  const failed = { id: "job_1", last_status: "failed" };
  assert.equal(downloadTaskNeedsAttention(task(), failed), true);
  markJobDismissed("job_1d");
  assert.equal(downloadTaskNeedsAttention(task(), { ...failed, id: "job_1d" }), false);
});

test("分组按 infohash 反查入库 Job，任一任务待办则整组待办", () => {
  const groups = groupDownloadTasks([
    task({
      id: "1:a",
      info_hash: "AAA",
      subscriptions: [subscription({ mediaItemId: "00000000-0000-4000-8000-000000000007" })],
      media_title: "剧",
    }),
    task({
      id: "1:b",
      info_hash: "BBB",
      subscriptions: [subscription({ mediaItemId: "00000000-0000-4000-8000-000000000007" })],
      media_title: "剧",
      state: "error",
    }),
    task({ id: "1:c", info_hash: "CCC", site_id: null, state: "error" }),
  ]);
  assert.equal(groups.length, 2);
  const byKey = new Map(groups.map((group) => [group.key, group]));
  assert.equal(
    downloadGroupNeedsAttention(
      byKey.get("media:00000000-0000-4000-8000-000000000007"),
      new Map(),
    ),
    true,
  );
  assert.equal(downloadGroupNeedsAttention(byKey.get("task:1:c"), new Map()), false);
  // 入库 Job 用小写 hash 建索引，任务侧的大写 hash 也要能命中
  const jobs = new Map([["aaa", { id: "job_2", last_status: "failed" }]]);
  assert.equal(
    downloadGroupNeedsAttention(
      groupDownloadTasks([
        task({
          info_hash: "AAA",
          subscriptions: [subscription({ mediaItemId: "00000000-0000-4000-8000-000000000008" })],
        }),
      ])[0],
      jobs,
    ),
    true,
  );
});
