import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function loadTs(path, dependencies = {}) {
  const source = await readFile(new URL(path, import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((id) => dependencies[id] ?? {}, module, module.exports);
  return module.exports;
}

const item = (id) => ({ media_item_id: id, library_id: "library", kind: "movie", title: id, year: null, poster_url: null });
const page = (ids, total, next_cursor = null) => ({ items: ids.map(item), total, next_cursor });
const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

async function pager(load) {
  const { CollectionPager } = await loadTs("../lib/collection-pager.ts");
  return new CollectionPager(load);
}

test("collection client sends bounded keyset parameters and reads exact total", async () => {
  const requests = [];
  const expected = page(["m2"], 250, "next");
  const api = await loadTs("../lib/api/collections.ts", {
    "@/lib/http": { request: async (url) => { requests.push(url); return { ok: true, data: expected }; } },
  });
  const actual = await api.listCollectionItems("collection", { limit: 40, cursor: "a+/=" });
  assert.deepEqual(actual, expected);
  const url = new URL(requests[0], "http://local");
  assert.equal(url.pathname, "/collections/collection/items");
  assert.equal(url.searchParams.get("limit"), "40");
  assert.equal(url.searchParams.get("cursor"), "a+/=");
  assert.equal(url.searchParams.has("offset"), false);
});

test("collection pager appends pages once and retains server total", async () => {
  const requests = [];
  const next = deferred();
  const model = await pager(async (id, options) => {
    requests.push({ id, ...options });
    return options.cursor ? next.promise : page(["a", "b"], 3, "cursor-b");
  });
  await model.reset("collection");
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["a", "b"]);
  assert.equal(model.state.total, 3);
  const first = model.loadMore();
  const second = model.loadMore();
  assert.equal(requests.length, 2, "parallel clicks share one outstanding page");
  assert.equal(requests[1].cursor, "cursor-b");
  next.resolve(page(["c"], 3));
  await Promise.all([first, second]);
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["a", "b", "c"]);
  assert.equal(model.state.nextCursor, null);
  await model.loadMore();
  assert.equal(requests.length, 2, "completed collections do not request again");
});

test("collection pager rejects stale head and append responses after route change", async () => {
  const oldHead = deferred();
  const oldMore = deferred();
  const model = await pager(async (id, options) => {
    if (id === "old-head") return oldHead.promise;
    if (id === "old-more") return options.cursor ? oldMore.promise : page(["old"], 2, "next");
    return page(["new"], 1);
  });
  const pendingHead = model.reset("old-head");
  await model.reset("new");
  oldHead.resolve(page(["stale-head"], 1));
  await pendingHead;
  assert.equal(model.state.items[0].media_item_id, "new");
  await model.reset("old-more");
  const pendingMore = model.loadMore();
  await model.reset("new");
  oldMore.resolve(page(["stale-more"], 2));
  await pendingMore;
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["new"]);
  assert.equal(model.state.total, 1);
});

test("collection pager preserves loaded page and cursor after failure for retry", async () => {
  let attempts = 0;
  const model = await pager(async (_id, options) => {
    if (!options.cursor) return page(["a"], 2, "next");
    attempts += 1;
    if (attempts === 1) throw new Error("page unavailable");
    return page(["b"], 2);
  });
  await model.reset("collection");
  await model.loadMore();
  assert.equal(model.state.error, "page unavailable");
  assert.equal(model.state.items.length, 1);
  assert.equal(model.state.nextCursor, "next");
  await model.loadMore();
  assert.equal(model.state.error, null);
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["a", "b"]);
});

test("collection refresh after removal invalidates outstanding append and resets total", async () => {
  const append = deferred();
  let heads = 0;
  const model = await pager(async (_id, options) => {
    if (options.cursor) return append.promise;
    heads += 1;
    return heads === 1 ? page(["a", "b"], 3, "next") : page(["b", "c"], 2);
  });
  await model.reset("collection");
  const pending = model.loadMore();
  await model.refresh();
  append.resolve(page(["c"], 3));
  await pending;
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["b", "c"]);
  assert.equal(model.state.total, 2);
  assert.equal(model.state.nextCursor, null);
});

test("initial collection failure remains a retryable error rather than an empty success", async () => {
  let fail = true;
  const model = await pager(async () => {
    if (fail) throw new Error("collection unavailable");
    return page([], 0);
  });
  await model.reset("collection");
  assert.equal(model.state.items, null);
  assert.equal(model.state.loading, false);
  assert.equal(model.state.error, "collection unavailable");
  fail = false;
  await model.refresh();
  assert.deepEqual(model.state.items, []);
  assert.equal(model.state.total, 0);
  assert.equal(model.state.error, null);
});

test("collection pager coalesces overlapping media after concurrent collection reorder", async () => {
  const model = await pager(async (_id, options) => options.cursor
    ? page(["b", "c"], 3, "last")
    : page(["a", "b"], 3, "next"));
  await model.reset("collection");
  await model.loadMore();
  assert.deepEqual(model.state.items.map((row) => row.media_item_id), ["a", "b", "c"]);
  assert.equal(model.state.total, 3);
  assert.equal(model.state.nextCursor, "last", "cursor position belongs to server selection");
});
