import assert from "node:assert/strict";
import test from "node:test";

import { coverFetchKey, rowFetchKey, rowItemQuery } from "../lib/home-row-items.ts";

const libRow = (over = {}) => ({
  id: "lib:1",
  kind: "library",
  hidden: false,
  sort: "added_at",
  reversed: false,
  unwatched: true,
  name: "",
  library: { id: "abc", name: "电影", kind: "movie", viewer_access: true, exclude_from_home: false },
  builtin: true,
  ...over,
});

test("默认库行与库卡片封面共用同一份「最近入库」的键", () => {
  // 两处算出不同的键 → 同一个库同一份数据打两次请求
  assert.equal(rowFetchKey(libRow({ unwatched: false })), coverFetchKey("abc"));
});

test("未看优先的库行：不再筛观看状态，只让服务端把观看分级排进顺序", () => {
  const query = rowItemQuery(libRow(), 20);
  assert.equal(query.sort, "added_at");
  assert.equal(query.limit, 20);
  assert.equal(query.unwatchedFirst, true);
  // 只排不筛：筛完只剩一张卡的小库看起来像坏了
  assert.equal(query.filter, undefined);
  // 自然方向不带 order（服务端按自然方向排，与加方向之前逐字相同）
  assert.equal(query.order, undefined);
});

test("反转过的行把方向写进 order，未看优先照旧", () => {
  const query = rowItemQuery(libRow({ sort: "rating", reversed: true }), 12);
  assert.equal(query.sort, "rating");
  assert.equal(query.order, "asc");
  assert.equal(query.unwatchedFirst, true);
  assert.equal(query.filter, undefined);
});

test("关掉未看优先就是纯浏览：既不分级也不筛", () => {
  const query = rowItemQuery(libRow({ unwatched: false }), 20);
  assert.equal(query.filter, undefined);
  assert.equal(query.unwatchedFirst, undefined);
});

test("「最近观看」行只要播过的（w=seen），不参与未看优先", () => {
  // 度量档会把没播过的沉底而不是排除，首页这一行不能这样
  const query = rowItemQuery(libRow({ sort: "last_played", unwatched: false }), 20);
  assert.equal(query.filter.watch, "seen");
  assert.equal(query.unwatchedFirst, undefined);
});

test("「最近观看」行即使被写上了未看优先开关，也不改口径", () => {
  const query = rowItemQuery(libRow({ sort: "last_played", unwatched: true }), 20);
  assert.equal(query.filter.watch, "seen");
  assert.equal(query.unwatchedFirst, undefined);
});
