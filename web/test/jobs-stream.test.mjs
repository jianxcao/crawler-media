import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function fixture() {
  const streams = [];
  const effects = [];
  const timers = new Map();
  let token = "first-token";
  let nextTimer = 0;
  const react = {
    createContext: () => ({ Provider: "provider" }),
    useCallback: (fn) => fn,
    useMemo: (fn) => fn(),
    useRef: (value) => ({ current: value }),
    useState: (value) => [value, () => {}],
    useEffect: (fn) => effects.push(fn()),
  };
  const mocks = {
    react,
    "react/jsx-runtime": { jsx: () => null },
    "@microsoft/fetch-event-source": { fetchEventSource: async (url, options) => { streams.push({ url, ...options }); } },
    "@/lib/api/jobs": { ACTIVE_JOB_STATUSES: new Set(), listJobs: async () => [] },
    "@/lib/http": { getAuthToken: () => token, redirectToLoginOn401: () => {}, resolveRequestUrl: (url) => url },
    "@/lib/session": { useSession: () => ({ session: { role: "admin" } }) },
  };
  const context = {
    AbortController, console,
    setTimeout: (fn) => { timers.set(++nextTimer, fn); return nextTimer; },
    clearTimeout: (id) => timers.delete(id),
    window: { setInterval: () => 1, clearInterval() {}, addEventListener() {}, removeEventListener() {} },
  };
  context.window.setTimeout = context.setTimeout;
  context.window.clearTimeout = context.clearTimeout;
  function load(path) {
    const filename = new URL(path, import.meta.url);
    const module = { exports: {} };
    const output = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    vm.runInNewContext(output, { ...context, module, exports: module.exports, require: (name) => {
      if (mocks[name]) return mocks[name];
      const result = load(`../lib/${name.split("/").at(-1)}.ts`);
      mocks[name] = result;
      return result;
    } });
    return module.exports;
  }
  const { JobsProvider } = load("../lib/jobs.tsx");
  return {
    streams, timers,
    mount() { JobsProvider({ children: null }); return effects.pop(); },
    token(value) { token = value; },
  };
}

test("multiple job providers share one authenticated stream until the final subscriber leaves", () => {
  const app = fixture();
  const first = app.mount();
  const second = app.mount();
  assert.equal(app.streams.length, 1, "one HTTP connection per tab");
  assert.equal(app.streams[0].headers.Authorization, "Bearer first-token");
  first();
  assert.equal(app.streams[0].signal.aborted, false);
  second();
  assert.equal(app.streams[0].signal.aborted, true);
  assert.equal(app.timers.size, 0);
});

test("changing credentials replaces the existing stream and cancels stale pending events", () => {
  const app = fixture();
  const first = app.mount();
  app.streams[0].onmessage({ event: "job" });
  app.token("second-token");
  const second = app.mount();
  assert.equal(app.streams[0].signal.aborted, true);
  assert.equal(app.streams[1].headers.Authorization, "Bearer second-token");
  first(); second();
  assert.equal(app.timers.size, 0);
});

test("the shared stream coalesces event bursts and reconnects clean server disconnects", () => {
  const app = fixture();
  const first = app.mount();
  const second = app.mount();
  const stream = app.streams[0];
  stream.onmessage({ event: "ready" });
  stream.onmessage({ event: "job" });
  assert.equal(app.timers.size, 1);
  assert.throws(() => stream.onclose(), /disconnected/);
  assert.equal(stream.onerror(new Error("temporary network interruption")), 3_000);
  first(); second();
});

test("forbidden stream access stops retries and the final consumer removes pending refresh work", async () => {
  const app = fixture();
  const unmount = app.mount();
  const stream = app.streams[0];
  await assert.rejects(stream.onopen(new Response(null, { status: 403 })), /403/);
  assert.equal(stream.signal.aborted, true);
  assert.throws(() => stream.onerror(new Error("denied")), /denied/);
  unmount();
  assert.equal(app.timers.size, 0);
});
