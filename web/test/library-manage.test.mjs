import assert from "node:assert/strict";
import test from "node:test";

import {
  accessLabel,
  accessRestricted,
  chapterJobLabel,
  configNotes,
  filterIsActive,
  filterLibraries,
  inventoryLabel,
  libraryIsBusy,
  libraryNeedsAttention,
  libraryStatus,
  moveInList,
  summarizeLibraries,
} from "../lib/library-manage.ts";

const ctx = { phaseLabels: {}, relativeTime: () => "2 小时前" };

// MediaLibrary 实体 + stats；扫描作业进度不在列表下行。
function lib(overrides = {}) {
  return {
    id: 1,
    name: "电影",
    kind: "movie",
    root_paths: ["/remote/media/电影"],
    root_missing: false,
    is_default: false,
    access_mode: "everyone",
    admin_visible: true,
    member_ids: [],
    stats: {
      item_count: 10,
      file_count: 12,
      total_size_bytes: 1024 ** 3,
    },
    ...overrides,
  };
}

test("新契约：状态列恒为空闲，即使带着旧契约的长任务字段", () => {
  const s = libraryStatus(
    lib({
      scanning: true,
      scan_progress: { phase: "ingesting", processed: 42, total: 100 },
      organizing: true,
      chapter_job: { status: "running", processed: 1, total: 2 },
      last_scan: { finished_at: "x", deferred: 3 },
    }),
    ctx,
  );
  assert.deepEqual(s, { tone: "idle", kind: "idle", title: "空闲", detail: "", percent: null });
});

test("空闲态不带无法核实的时间信息", () => {
  assert.equal(libraryStatus(lib(), ctx).detail, "");
  assert.equal(libraryStatus(lib(), ctx).percent, null);
});

test("新契约：章节作业状态不再进状态列，菜单项文案保留同一口径", () => {
  const running = { status: "running", processed: 35, total: 100, stopping: false };
  assert.equal(libraryStatus(lib({ chapter_job: running }), ctx).kind, "idle");
  assert.equal(chapterJobLabel(null), "生成章节");
  assert.equal(chapterJobLabel({ status: "queued" }), "生成章节排队中");
  assert.equal(chapterJobLabel(running), "正在生成章节 35%");
  assert.equal(chapterJobLabel({ status: "cancelling", stopping: true }), "正在停止生成章节");
  // 刚开始跑、还没统计出分母：不给百分比也不写 0 / 0
  assert.equal(chapterJobLabel({ status: "running", processed: 0, total: 0 }), "正在生成章节");
});

test("根路径缺失算待处理；列表没有作业进度所以不算忙碌", () => {
  assert.equal(libraryNeedsAttention(lib()), false);
  assert.equal(libraryNeedsAttention(lib({ root_missing: true })), true);
  assert.equal(libraryIsBusy(lib()), false);
  assert.equal(
    libraryIsBusy(lib({ scanning: true, organizing: true, metadata_refresh: { refreshing: true } })),
    false,
  );
});

test("页头摘要：规模事实照常计算；根路径缺失计入待处理", () => {
  const libs = [
    lib({ id: 1 }),
    lib({ id: 2, stats: { item_count: 5, file_count: 5, total_size_bytes: 1024 ** 3 } }),
    lib({ id: 3, stats: { item_count: 5, file_count: 5, total_size_bytes: 0 }, root_missing: true }),
  ];
  const s = summarizeLibraries(libs);
  assert.equal(s.facts, "3 个媒体库 · 20 个条目 · 2.00 GB");
  assert.equal(s.busy, 0);
  assert.equal(s.attention, 1);
  assert.equal(s.missing, true);
  assert.deepEqual(summarizeLibraries([]), {
    facts: "0 个媒体库 · 0 个条目 · 0 B",
    busy: 0,
    attention: 0,
    missing: false,
  });
});

test("配置备注：只说偏离默认的部分", () => {
  assert.deepEqual(configNotes(lib()), []);
  assert.deepEqual(configNotes(lib({ exclude_from_home: true, realtime_watch: false })), [
    "不在首页",
    "未开实时监控",
  ]);
});

test("筛选：类型与搜索词（库名或根目录）仍然生效", () => {
  const libs = [
    lib({ id: 1, name: "电影", kind: "movie" }),
    lib({ id: 2, name: "剧集", kind: "tv", root_paths: ["/mnt/nas2/剧集"] }),
    lib({ id: 3, name: "演唱会", kind: "video", root_paths: ["/remote/media/演唱会"] }),
  ];
  const ids = (r) => r.map((l) => l.id);
  const f = (overrides) => ({ query: "", kind: null, focus: null, ...overrides });
  assert.deepEqual(ids(filterLibraries(libs, f())), [1, 2, 3]);
  assert.deepEqual(ids(filterLibraries(libs, f({ kind: "tv" }))), [2]);
  assert.deepEqual(ids(filterLibraries(libs, f({ query: "NAS2" }))), [2]);
  assert.deepEqual(ids(filterLibraries(libs, f({ query: "演唱" }))), [3]);
  assert.deepEqual(ids(filterLibraries(libs, f({ query: "电影", kind: "tv" }))), []);
  assert.equal(filterIsActive(f({ query: "  " })), false);
  assert.equal(filterIsActive(f({ kind: "movie" })), true);
  assert.equal(filterIsActive(f({ focus: "attention" })), true);
});

test("换位：向后、向前、越界与原地", () => {
  const list = ["a", "b", "c", "d"];
  assert.deepEqual(moveInList(list, 0, 2), ["b", "c", "a", "d"]);
  assert.deepEqual(moveInList(list, 3, 1), ["a", "d", "b", "c"]);
  assert.equal(moveInList(list, 1, 1), list);
  assert.equal(moveInList(list, 0, 4), list);
  assert.equal(moveInList(list, -1, 0), list);
});

test("可见范围读 access_mode / member_ids", () => {
  assert.equal(accessLabel(lib()), "全部成员");
  assert.equal(accessLabel(lib({ access_mode: "selected", member_ids: ["u1"] })), "指定 1 名成员");
  assert.equal(accessRestricted(lib()), false);
  assert.equal(accessRestricted(lib({ access_mode: "selected" })), true);
});

test("库存文案：影视库按「部」、video 库按「条目」", () => {
  assert.deepEqual(inventoryLabel(lib()), { primary: "10 部", secondary: "12 个文件" });
  assert.deepEqual(inventoryLabel(lib({ kind: "video" })), { primary: "10 个条目", secondary: "12 个文件" });
});
