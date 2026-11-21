import assert from "node:assert/strict";
import test from "node:test";

import { busyLabel, unitKeyOf } from "../lib/player/player-ui.ts";

test("busyLabel maps player phases to user-facing labels", () => {
  assert.equal(busyLabel("deciding"), "正在判断播放方式…");
  assert.equal(busyLabel("session-starting"), "正在准备视频流…");
  assert.equal(busyLabel("degrading"), "这一档放不出来，正在降档重试…");
  assert.equal(busyLabel("buffering"), "正在缓冲…");
});

test("unitKeyOf formats movie and tv playback units", () => {
  assert.equal(unitKeyOf({ media_item_id: "item-1" }), "item-1/0/0");
  assert.equal(
    unitKeyOf({ media_item_id: "item-2", season_number: 2, episode_number: 5 }),
    "item-2/2/5",
  );
});
