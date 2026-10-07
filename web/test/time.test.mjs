import assert from "node:assert/strict";
import test from "node:test";

import {
  formatNextRunTime,
  formatTimelineDayLabel,
  formatTimelineTime,
  timelineDayKey,
} from "../lib/time.ts";

test("当天的 Feed 记录显示时分", () => {
  assert.equal(
    formatTimelineTime("2026-08-15T14:32:00+08:00", "2026-08-15T20:00:00+08:00"),
    "14:32",
  );
});

test("跨天的 Feed 记录显示月日，避免历史时间被误看成当天", () => {
  assert.equal(
    formatTimelineTime("2026-08-12T14:32:00+08:00", "2026-08-15T20:00:00+08:00"),
    "08/12",
  );
});

test("空时间沿用全局占位符", () => {
  assert.equal(formatTimelineTime(null), "—");
});

test("历史 Feed 按本地日期生成稳定分组键", () => {
  assert.equal(timelineDayKey("2026-08-14T23:30:00+08:00"), "2026-08-14");
});

test("历史 Feed 使用今天早些时候、昨天和具体日期标题", () => {
  const reference = "2026-08-15T20:00:00+08:00";
  assert.equal(formatTimelineDayLabel("2026-08-15T08:00:00+08:00", reference), "今天早些时候");
  assert.equal(formatTimelineDayLabel("2026-08-14T08:00:00+08:00", reference), "昨天");
  assert.equal(formatTimelineDayLabel("2026-08-12T08:00:00+08:00", reference), "8月12日");
  assert.equal(
    formatTimelineDayLabel("2025-08-12T08:00:00+08:00", reference),
    "2025年8月12日",
  );
});

test("formatNextRunTime 跨天及当年友好提示", () => {
  const reference = "2026-08-15T10:00:00+08:00";
  // 当天显示时分
  assert.equal(formatNextRunTime("2026-08-15T14:26:00+08:00", reference), "14:26");
  // 明天显示「明天 HH:mm」
  assert.equal(formatNextRunTime("2026-08-16T14:26:00+08:00", reference), "明天 14:26");
  // 后天显示「后天 HH:mm」
  assert.equal(formatNextRunTime("2026-08-17T14:26:00+08:00", reference), "后天 14:26");
  // 当年更晚显示「MM/DD HH:mm」
  assert.equal(formatNextRunTime("2026-09-25T14:26:00+08:00", reference), "09/25 14:26");
  // 跨年显示完整年月日时分
  assert.equal(formatNextRunTime("2027-01-01T14:26:00+08:00", reference), "2027/01/01 14:26");
});
