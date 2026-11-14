import assert from "node:assert/strict";
import test from "node:test";

import { shouldHeartbeat } from "../lib/player/heartbeat.ts";

/**
 * 进度心跳的准入判定。
 *
 * 用户暂停时后端会按 `last_report_at` 空闲回收会话；心跳一停，回来点播放就是
 * 一直转圈。所以暂停中必须继续喂——而暂停后的拖动 / 卡住会把播放阶段顶到
 * seeking / buffering 上（`seeked` 不会把阶段推回 playing），这两档同样要算。
 */

test("正在播：心跳照常跑", () => {
  assert.equal(shouldHeartbeat({ phase: "playing", announced: true, paused: false }), true);
});

test("暂停中：无论阶段停在播放 / 拖动 / 缓冲，心跳都不能停", () => {
  for (const phase of ["playing", "seeking", "buffering"]) {
    assert.equal(
      shouldHeartbeat({ phase, announced: true, paused: true }),
      true,
      `暂停在 ${phase} 阶段时必须继续喂心跳`,
    );
  }
});

test("还没报过 start 的会话不喂：否则服务端会凭空建出一条播放会话", () => {
  assert.equal(shouldHeartbeat({ phase: "buffering", announced: false, paused: true }), false);
  assert.equal(shouldHeartbeat({ phase: "playing", announced: false, paused: true }), false);
});

test("暂停但阶段已经了结/出错：不喂", () => {
  for (const phase of ["ended", "error", "consent", "deciding", "session-starting", "degrading", "idle"]) {
    assert.equal(
      shouldHeartbeat({ phase, announced: true, paused: true }),
      false,
      `${phase} 阶段不该继续喂心跳`,
    );
  }
});

test("未暂停时仍只在播放阶段心跳（拖动/缓冲不打断原行为）", () => {
  assert.equal(shouldHeartbeat({ phase: "seeking", announced: true, paused: false }), false);
  assert.equal(shouldHeartbeat({ phase: "buffering", announced: true, paused: false }), false);
  assert.equal(shouldHeartbeat({ phase: "ended", announced: true, paused: false }), false);
});
