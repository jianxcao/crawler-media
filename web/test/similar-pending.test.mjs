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
  new Function("require", "module", "exports", js)(id => dependencies[id] ?? {}, module, module.exports);
  return module.exports;
}
const ready = { state: "ready", items: [{ tmdb_id: "42", title: "Related Media", kind: "movie", year: 2026, poster_url: null }] };
const pending = { state: "pending", retry_after_ms: 250 };

test("similar client preserves explicit pending and forwards request cancellation", async () => {
  const controller = new AbortController();
  const requests = [];
  const api = await loadTs("../lib/api/libraries.ts", {
    "@/lib/http": { request: async (path, init) => { requests.push({ path, init }); return { ok: true, data: pending }; } },
  });
  assert.deepEqual(await api.fetchItemSimilar("library", "media", controller.signal), pending);
  assert.equal(requests[0].path, "/libraries/library/items/media/similar");
  assert.equal(requests[0].init?.signal, controller.signal);
});

test("similar polling returns complete ready results after explicit pending", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  const controller = new AbortController();
  const observed = [];
  let calls = 0;
  const result = await waitForSimilar(async signal => {
    assert.equal(signal, controller.signal);
    calls += 1;
    return calls === 1 ? pending : ready;
  }, { signal: controller.signal, onPending: () => observed.push("pending"), pause: async ms => observed.push(ms) });
  assert.deepEqual(result, ready.items);
  assert.deepEqual(observed, ["pending", 250]);
  assert.equal(calls, 2);
});

test("similar ready-empty remains a terminal result without polling", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  const result = await waitForSimilar(async () => ({ state: "ready", items: [] }), {
    pause: async () => assert.fail("ready-empty must not be treated as pending"),
  });
  assert.deepEqual(result, []);
});

test("similar source failures propagate without becoming an empty success or retried", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  const failure = new Error("catalog source unavailable");
  let calls = 0;
  await assert.rejects(waitForSimilar(async () => { calls += 1; throw failure; }), error => error === failure);
  assert.equal(calls, 1);
});

test("similar polling cancels an actual wait and never requests the old Media again", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  const controller = new AbortController();
  let calls = 0;
  const result = waitForSimilar(async () => { calls += 1; return pending; }, {
    signal: controller.signal,
    onPending: () => queueMicrotask(() => controller.abort()),
  });
  await assert.rejects(result, { name: "AbortError" });
  assert.equal(calls, 1);
});

test("similar cancellation rejects even if an already started response arrives late", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  const controller = new AbortController();
  let finish;
  const result = waitForSimilar(() => new Promise(resolve => { finish = resolve; }), { signal: controller.signal });
  controller.abort();
  finish(ready);
  await assert.rejects(result, { name: "AbortError" });
});

test("similar pending poll delays stay bounded without truncating the pending lifetime", async () => {
  const { waitForSimilar } = await loadTs("../lib/similar-read.ts");
  for (const [requested, expected] of [[1, 100], [60000, 5000]]) {
    let calls = 0;
    const delays = [];
    assert.deepEqual(await waitForSimilar(async () => ++calls === 1
      ? { state: "pending", retry_after_ms: requested }
      : ready, { pause: async ms => { delays.push(ms); } }), ready.items);
    assert.deepEqual(delays, [expected]);
  }
});

test("similar client rejects old arrays, unknown states, and malformed pending intervals", async () => {
  for (const data of [[], { state: "ready" }, { state: "unknown", items: [] },
    { state: "pending", retry_after_ms: 0 }, { state: "pending", retry_after_ms: NaN }]) {
    const api = await loadTs("../lib/api/libraries.ts", {
      "@/lib/http": { request: async () => ({ ok: true, data }) },
    });
    await assert.rejects(api.fetchItemSimilar("library", "media"), /响应格式错误/);
  }
});
