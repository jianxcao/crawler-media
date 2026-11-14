import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

/**
 * 浏览列表的分页契约：键集游标（`cursor` → `next_cursor`），`offset` 已删除。
 *
 * 后端对任何 `offset` 都回 400，所以前端一旦写出这个参数就是必挂的请求；
 * 这里既锁 `buildBrowseQuery` 的序列化，也锁「信封上的 next_cursor/total
 * 被正确读出来」——不然游标拿不到，第二页永远翻不过去。
 */

async function loadLibraries(request) {
  const source = await readFile(new URL("../lib/api/libraries.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((dependency) => {
    if (dependency === "@/lib/http") return { request, resolveRequestUrl: (path) => path };
    if (dependency.endsWith("library-filter")) return { filterQuery: (filter, query) => {
      if (filter?.genre) query.set("g", filter.genre);
    } };
    if (dependency.endsWith("media-types")) return {};
    return {};
  }, module, module.exports);
  return module.exports;
}

const envelope = (data, meta = {}) => ({ ok: true, data, ...meta });

test("buildBrowseQuery 用 cursor 分页，绝不发 offset", async () => {
  const { buildBrowseQuery } = await loadLibraries(async () => envelope([]));
  const plain = buildBrowseQuery({ limit: 60, sort: "title", order: "asc" });
  assert.equal(plain.get("limit"), "60");
  assert.equal(plain.get("sort"), "title");
  assert.equal(plain.get("order"), "asc");
  assert.equal(plain.has("offset"), false, "首屏也不能带 offset");

  const next = buildBrowseQuery({ cursor: "opaque-cursor", limit: 60 });
  assert.equal(next.get("cursor"), "opaque-cursor");
  assert.equal(next.has("offset"), false, "续页只带 cursor");
});

test("buildBrowseQuery 在没有任何参数时是空查询串", async () => {
  const { buildBrowseQuery } = await loadLibraries(async () => envelope([]));
  assert.equal(buildBrowseQuery().toString(), "");
  assert.equal(buildBrowseQuery({ cursor: null }).toString(), "");
});

test("listLibraryItems 从信封上读出 next_cursor 与 total", async () => {
  const seen = [];
  const api = await loadLibraries(async (path) => {
    seen.push(path);
    return envelope(
      [
        {
          media_item_id: "m1",
          kind: "movie",
          library_id: "lib",
          title: "A",
          year: 2024,
          poster_url: null,
          file_count: 1,
          seasons: [],
          episode_count: 0,
        },
      ],
      { next_cursor: "next-token", total: 250 },
    );
  });
  const page = await api.listLibraryItems("lib", { limit: 1, cursor: "prev-token" });
  assert.equal(page.items.length, 1);
  assert.equal(page.items[0].media_item_id, "m1");
  assert.equal(page.next_cursor, "next-token");
  assert.equal(page.total, 250);
  assert.equal(seen[0], "/libraries/lib/items?limit=1&cursor=prev-token");
  assert.ok(!seen[0].includes("offset"), "请求里不能出现 offset");
});

test("listLibraryItems 末页 next_cursor/total 为 null", async () => {
  const api = await loadLibraries(async () => envelope([]));
  const page = await api.listLibraryItems("lib");
  assert.deepEqual(page.items, []);
  assert.equal(page.next_cursor, null);
  assert.equal(page.total, null);
});

test("listLibraryGallery 同样用 cursor，并读出 next_cursor", async () => {
  const seen = [];
  const api = await loadLibraries(async (path) => {
    seen.push(path);
    return envelope([{ media_item_id: "m1", images: [] }], { next_cursor: "g-next" });
  });
  const page = await api.listLibraryGallery("lib", { limit: 2, cursor: "g-prev" });
  assert.equal(page.groups.length, 1);
  assert.equal(page.next_cursor, "g-next");
  assert.equal(seen[0], "/libraries/lib/gallery?limit=2&cursor=g-prev");
  assert.ok(!seen[0].includes("offset"));
});
