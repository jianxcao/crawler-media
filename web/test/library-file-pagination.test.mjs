import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

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
  assert.equal(isPlayableFile(dto("pending", { publication: "pending" })), false);
  assert.equal(isPlayableFile({}), false, "缺字段不 fail-open：不可播");
});

/** 加载真实 `lib/api/libraries.ts`，注入真实分页 collector，只伪造 HTTP request。 */
async function loadLibrariesApi(request) {
  const source = await readFile(new URL("../lib/api/libraries.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  const pages = await import("../lib/library-file-pages.ts");
  new Function("require", "module", "exports", js)((name) => {
    if (name === "@/lib/http") return { request, resolveRequestUrl: (path) => path };
    if (name === "@/lib/library-file-pages") return pages;
    if (name === "@/lib/library-removal") return { waitForLibraryRemoval: async () => {} };
    if (name === "@/lib/library-operations") return { waitForLibraryOperation: async () => {} };
    if (name === "@/lib/library-filter") return {};
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return module.exports;
}

/** 服务端真实形状：首详情给 `next_file_cursor`，续页给 `next_cursor`。 */
function serverFixture(total, limit) {
  const rows = Array.from({ length: total }, (_, i) =>
    dto(`f${String(i).padStart(3, "0")}`, i === 0
      ? { units: [{ season: 1, episode: 1 }, { season: 1, episode: 2 }], presence: "missing" }
      : {}),
  );
  const requests = [];
  const request = async (path) => {
    requests.push(path);
    const match = path.match(/files\?after=([^&]+)/);
    const offset = match ? Number(decodeURIComponent(match[1]).slice("c-".length)) : 0;
    const slice = rows.slice(offset, offset + limit);
    const next = offset + limit < rows.length ? `c-${offset + limit}` : null;
    if (!match) {
      return { ok: true, data: { media_item_id: "m", kind: "tv", title: "T", files: slice, next_file_cursor: next } };
    }
    return { ok: true, data: { files: slice, next_cursor: next } };
  };
  return { request, requests };
}

for (const [total, expectedRequests] of [[50, 1], [100, 2], [101, 3], [201, 5]]) {
  test(`真实 adapter 读完 ${total} 个文件并保留覆盖与在位状态`, async () => {
    const server = serverFixture(total, 50);
    const api = await loadLibrariesApi(server.request);
    const detail = await api.getLibraryItemDetail("lib", "m");
    assert.equal(detail.files.length, total);
    assert.equal(server.requests.length, expectedRequests);
    assert.deepEqual(detail.files[0].units, [{ season: 1, episode: 1 }, { season: 1, episode: 2 }]);
    assert.equal(detail.files[0].presence, "missing");
    assert.equal(detail.files[0].publication, "published");
    assert.equal(isPlayableFile(detail.files[0]), false);
    assert.equal(isPlayableFile(detail.files[1]), true);
  });
}

test("真实 adapter 对缺状态字段的文件不 fail-open", async () => {
  const api = await loadLibrariesApi(async () => ({
    ok: true,
    data: { media_item_id: "m", kind: "movie", title: "T", files: [{ file_id: "x", path: "x.mkv" }], next_file_cursor: null },
  }));
  const detail = await api.getLibraryItemDetail("lib", "m");
  assert.equal(isPlayableFile(detail.files[0]), false);
});
