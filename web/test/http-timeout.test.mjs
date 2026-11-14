import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { setImmediate } from "node:timers/promises";
import ts from "typescript";
import * as requestSignal from "../lib/request-signal.mjs";

async function loadModule(path, imports) {
  const source = await readFile(new URL(path, import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((name) => {
    if (name in imports) return imports[name];
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return module.exports;
}

const [backdropCache, uiPrefsCache, removals, operations, filePages, libraryFilter] =
  await Promise.all([
    "../lib/backdrop-cache.ts",
    "../lib/ui-prefs-cache.ts",
    "../lib/library-removal.ts",
    "../lib/library-operations.ts",
    "../lib/library-file-pages.ts",
    "../lib/library-filter.ts",
  ].map((path) => loadModule(path, {})));

const http = await loadModule("../lib/http.ts", {
  "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
  "@/lib/backdrop-cache": backdropCache,
  "@/lib/ui-prefs-cache": uiPrefsCache,
  "@/lib/request-signal.mjs": requestSignal,
});
const libraries = await loadModule("../lib/api/libraries.ts", {
  "@/lib/http": http,
  "@/lib/library-removal": removals,
  "@/lib/library-operations": operations,
  "@/lib/library-file-pages": filePages,
  "@/lib/library-filter": libraryFilter,
});

function pendingFetch(t) {
  let signal;
  let respond;
  t.mock.method(globalThis, "fetch", (_url, init) => new Promise((resolve, reject) => {
    signal = init.signal;
    const abort = () => reject(signal.reason);
    if (signal.aborted) {
      abort();
      return;
    }
    signal.addEventListener("abort", abort, { once: true });
    respond = (data) => {
      signal.removeEventListener("abort", abort);
      resolve(new Response(JSON.stringify({ ok: true, data }), {
        headers: { "Content-Type": "application/json" },
      }));
    };
  }));
  return {
    respond: (data) => respond(data),
    get signal() { return signal; },
  };
}

function observe(promise) {
  const result = { state: "pending" };
  promise.then(
    () => { result.state = "fulfilled"; },
    () => { result.state = "rejected"; },
  );
  return result;
}

test("cover generation can finish after the ordinary deadline and releases its timer", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const transport = pendingFetch(t);
  const pending = libraries.generateLibraryCover("library", { preview_only: true });
  const outcome = observe(pending);

  t.mock.timers.tick(15_000);
  await setImmediate();
  assert.equal(outcome.state, "pending");
  transport.respond({ data_url: "data:image/png;base64,cover", preview: true });
  assert.deepEqual(await pending, { data_url: "data:image/png;base64,cover", preview: true });

  t.mock.timers.tick(180_000);
  assert.equal(transport.signal.aborted, false, "completed requests release their deadline");
});

test("cover generation times out at its full three minute deadline", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  pendingFetch(t);
  const pending = libraries.generateLibraryCover("library", { preview_only: true });
  const outcome = observe(pending);

  t.mock.timers.tick(179_999);
  await setImmediate();
  assert.equal(outcome.state, "pending");
  t.mock.timers.tick(1);
  await assert.rejects(pending, (error) => error instanceof http.HttpError && error.status === 0);
});

test("ordinary requests retain their deadline when the caller supplies a signal", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  pendingFetch(t);
  const controller = new AbortController();
  const pending = http.request("/libraries", { signal: controller.signal });
  const outcome = observe(pending);

  t.mock.timers.tick(14_999);
  await setImmediate();
  assert.equal(outcome.state, "pending");
  t.mock.timers.tick(1);
  await assert.rejects(pending, (error) => error instanceof http.HttpError && error.status === 0);
  assert.equal(controller.signal.aborted, false);
});

test("an explicit request deadline preserves caller cancellation and its reason", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  pendingFetch(t);
  const controller = new AbortController();
  const pending = http.request("/libraries", { signal: controller.signal }, { timeoutMs: 180_000 });
  const outcome = observe(pending);

  t.mock.timers.tick(15_000);
  await setImmediate();
  assert.equal(outcome.state, "pending");
  const reason = new Error("caller cancelled");
  controller.abort(reason);
  await assert.rejects(pending, (error) => error === reason);
});
