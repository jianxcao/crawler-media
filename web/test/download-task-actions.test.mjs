import assert from "node:assert/strict";
import test from "node:test";

import { shouldOfferInlineReplacement } from "../lib/download-task-actions.ts";

test("15 分钟无进度且可定位任务时在提示区提供立即换种", () => {
  assert.equal(
    shouldOfferInlineReplacement({ id: "sub-1:https://pt/dl?id=1", state: "stalled" }),
    true,
  );
});

test("出错的任务同样提供立即换种", () => {
  assert.equal(
    shouldOfferInlineReplacement({ id: "sub-1:https://pt/dl?id=1", state: "error" }),
    true,
  );
});

test("客户端里已找不到的 missing 任务同样提供立即换种", () => {
  assert.equal(
    shouldOfferInlineReplacement({ id: "sub-1:https://pt/dl?id=1", state: "missing" }),
    true,
  );
});

test("尚未进入救援窗口时不提前展示立即换种", () => {
  assert.equal(
    shouldOfferInlineReplacement({ id: "sub-1:https://pt/dl?id=1", state: "downloading" }),
    false,
  );
});

test("没有任务 id 时不展示不可执行的换种按钮", () => {
  assert.equal(shouldOfferInlineReplacement({ state: "stalled" }), false);
});

test("旧形状仍按 can_replace 判断", () => {
  assert.equal(
    shouldOfferInlineReplacement({
      id: "sub-1:https://pt/dl?id=1",
      can_replace: true,
      downloader_id: 3,
    }),
    true,
  );
});
