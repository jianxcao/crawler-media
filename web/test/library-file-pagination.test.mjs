import assert from "node:assert/strict";
import test from "node:test";

import {
  collectAllFilePages,
  fileServesEpisode,
  fileUnits,
  isPlayableFile,
} from "../lib/library-file-pages.ts";

/** 真实后端形状的文件行（含 Task 6 新增的 units/presence/publication）。 */
const dto = (id, extra = {}) => ({
  file_id: id,
  path: `Paged/${id}.mkv`,
  units: [],
  presence: "present",
  publication: "published",
  season: null,
  episode: null,
  ...extra,
});

/** 用内存分页模拟后端：limit 个一页，末页 next_cursor 为 null。 */
function fakeBackend(total, limit) {
  const rows = Array.from({ length: total }, (_, i) =>
    dto(`f${String(i).padStart(3, "0")}`),
  );
  const page = (offset) => {
    const slice = rows.slice(offset, offset + limit);
    const next = offset + limit < rows.length ? `cursor-${offset + limit}` : null;
    return { files: slice, next_cursor: next };
  };
  return {
    firstPage: page(0),
    fetchAfter: async (after) => {
      const offset = Number(after.slice("cursor-".length));
      return page(offset);
    },
  };
}

for (const total of [0, 49, 50, 51, 100, 101]) {
  for (const limit of [1, 7, 50]) {
    test(`分页遍历覆盖 ${total} 个文件（limit=${limit}）不重不漏`, async () => {
      const backend = fakeBackend(total, limit);
      const files = await collectAllFilePages(backend.firstPage, backend.fetchAfter);
      assert.equal(files.length, total);
      assert.equal(new Set(files.map((f) => f.file_id)).size, total);
      assert.deepEqual(
        files.map((f) => f.file_id),
        Array.from({ length: total }, (_, i) => `f${String(i).padStart(3, "0")}`),
      );
    });
  }
}

test("末页 next_cursor 为 null 就停止，不再多取一页", async () => {
  const backend = fakeBackend(50, 50);
  let calls = 0;
  const files = await collectAllFilePages(backend.firstPage, async (after) => {
    calls += 1;
    return backend.fetchAfter(after);
  });
  assert.equal(files.length, 50);
  assert.equal(calls, 0, "整页刚好取完时不应请求下一页");
});

test("重复 file_id 必须显式失败，不能被静默去重", async () => {
  const firstPage = { files: [dto("dupe")], next_cursor: "next" };
  await assert.rejects(
    () => collectAllFilePages(firstPage, async () => ({ files: [dto("dupe")], next_cursor: null })),
    /重复 file_id/,
  );
});

test("分页未结束却拿不到取数函数时失败", async () => {
  const firstPage = { files: [dto("a")], next_cursor: "next" };
  await assert.rejects(() => collectAllFilePages(firstPage), /缺少下一页取数函数/);
});

test("多集文件按完整 units 判断服务哪一集，不只认首个坐标", () => {
  const multi = dto("multi", {
    units: [
      { season: 1, episode: 1 },
      { season: 1, episode: 2 },
    ],
    season: 1,
    episode: 1,
  });
  assert.equal(fileServesEpisode(multi, 1, 2), true, "E02 必须被识别为有文件");
  assert.equal(fileServesEpisode(multi, 1, 3), false);
  assert.deepEqual(fileUnits(multi), [
    { season: 1, episode: 1 },
    { season: 1, episode: 2 },
  ]);
});

test("缺失/未知文件可查看但不可作为可播版本", () => {
  assert.equal(isPlayableFile(dto("present")), true);
  assert.equal(isPlayableFile(dto("missing", { presence: "missing" })), false);
  assert.equal(isPlayableFile(dto("unknown", { presence: "unknown" })), false);
  assert.equal(isPlayableFile({}), true, "旧响应没有 presence 时按可播处理");
});
