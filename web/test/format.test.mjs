import assert from "node:assert/strict";
import test from "node:test";

import {
  formatBytes,
  formatRuntimeMinutes,
  formatVideoResolution,
} from "../lib/format.ts";

test("体积数字部分最多 5 个字符，进位到百位就丢掉小数", () => {
  assert.equal(formatBytes(1024 * 1024 * 99.99), "99.99 MB");
  // 99.997 MB：先四舍五入再判断档位，否则会格成 "100.00 MB" 撑破窄屏定宽列
  assert.equal(formatBytes(1024 * 1024 * 99.997), "100 MB");
  assert.equal(formatBytes(1023), "1023 B");
  assert.equal(formatBytes(0), "0 B");
});

test("电影片长按小时和剩余分钟展示", () => {
  assert.equal(formatRuntimeMinutes(45), "45 分钟");
  assert.equal(formatRuntimeMinutes(120), "2 小时");
  assert.equal(formatRuntimeMinutes(126), "2 小时 6 分钟");
});

test("分辨率使用准确的消费级标签", () => {
  assert.equal(formatVideoResolution("2160p"), "4K");
  assert.equal(formatVideoResolution("1440p"), "2K");
  assert.equal(formatVideoResolution("1080p"), "1080p");
  assert.equal(formatVideoResolution("2160"), "4K");
});
