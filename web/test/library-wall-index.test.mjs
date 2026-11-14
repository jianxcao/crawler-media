import assert from "node:assert/strict";
import test from "node:test";

import {
  activeWallInitialAtViewport,
  wallBucketAtOffset,
  wallBucketOffsets,
  wallInitialAtOffset,
} from "../lib/library-wall-index.ts";

// Task 26：线上只传 {initial, count, cursor}，档位位置由 count 前缀和还原。
const index = [
  { initial: "A", count: 12 },
  { initial: "B", count: 28 },
  { initial: "M", count: 40 },
  { initial: "#", count: 5 },
];

test("档位位置是 count 的前缀和，不需要线上带绝对下标", () => {
  assert.deepEqual(wallBucketOffsets(index), [0, 12, 40, 80]);
  assert.deepEqual(wallBucketOffsets([]), []);
  // count 异常（负数/缺失）不得把后续档位的下标带偏
  assert.deepEqual(wallBucketOffsets([{ initial: "A", count: -3 }, { initial: "B", count: 4 }]), [0, 0]);
});

test("绝对位置映射到所在字母档（含边界与越界）", () => {
  assert.equal(wallInitialAtOffset(index, 0), "A");
  assert.equal(wallInitialAtOffset(index, 11), "A");
  assert.equal(wallInitialAtOffset(index, 12), "B");
  assert.equal(wallInitialAtOffset(index, 39), "B");
  assert.equal(wallInitialAtOffset(index, 40), "M");
  assert.equal(wallInitialAtOffset(index, 80), "#");
  assert.equal(wallInitialAtOffset(index, 84), "#");
  assert.equal(wallInitialAtOffset(index, -1), null);
  assert.equal(wallInitialAtOffset([], 0), null);
});

test("档下标用于取该档的游标", () => {
  assert.equal(wallBucketAtOffset(index, 0), 0);
  assert.equal(wallBucketAtOffset(index, 13), 1);
  assert.equal(wallBucketAtOffset(index, 85), 3);
  assert.equal(wallBucketAtOffset(index, -5), -1);
});

test("滚动时选中最后一个越过视口顶边的字母锚点", () => {
  const markers = [
    { initial: "A", top: -500 },
    { initial: "B", top: -20 },
    { initial: "M", top: 260 },
  ];
  assert.equal(activeWallInitialAtViewport(markers, 0, "A"), "B");
  assert.equal(activeWallInitialAtViewport(markers, 300, "A"), "M");
});

test("尚未滚到首个字母锚点时保留当前分页档", () => {
  assert.equal(
    activeWallInitialAtViewport([{ initial: "M", top: 400 }], 0, "M"),
    "M",
  );
});
