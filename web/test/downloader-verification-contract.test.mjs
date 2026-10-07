import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const require = createRequire(import.meta.url);
function load(relative, mocks = {}) {
  const filename = new URL(relative, import.meta.url);
  const code = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    fileName: filename.pathname,
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(code, {
    module, exports: module.exports,
    require: (name) => name in mocks ? mocks[name] : require(name),
    window: { location: { search: "" } }, URLSearchParams, console,
  }, { filename: filename.pathname });
  return module.exports;
}
const verification = load("../lib/downloader-verification.ts");
const row = (extra = {}) => ({
  id: "fixture", name: "Fixture Downloader", kind: "qbittorrent",
  url: "http://fixture.invalid", path_maps: [], is_default: false,
  enabled: true, status: "pending", last_error: null, ...extra,
});
function apiFixture(respond) {
  const calls = [];
  const api = load("../lib/api/downloaders.ts", {
    "@/lib/http": { request: async (path, init) => {
      calls.push({ path, method: init?.method ?? "GET", body: init?.body });
      return { data: await respond(path, init) };
    } },
    "../downloader-verification": verification,
  });
  return { api, calls };
}

for (const ok of [true, false]) {
  test(`manual verify ${ok ? "success" : "failure"} uses POST result without GET polling`, async () => {
    const snapshot = row({ enabled: false, status: "disabled" });
    const { api, calls } = apiFixture((path) => path.endsWith("/verify")
      ? { ok, error: ok ? null : "bad credentials" } : row());
    const result = await api.reverifyDownloader(snapshot.id, snapshot);
    assert.equal(result.status, ok ? "active" : "failed");
    assert.equal(result.last_error, ok ? null : "bad credentials");
    assert.equal(result.enabled, false, "enabled remains independent of verification");
    assert.deepEqual(calls, [{ path: "/downloaders/fixture/verify", method: "POST", body: undefined }]);
  });
}
test("create saves an untested configuration without automatic verify requests", async () => {
  const { api, calls } = apiFixture(() => row());
  const result = await api.createDownloader({
    name: "Fixture", client_type: "transmission", url: "http://fixture.invalid",
  });
  assert.equal(result.status, "pending");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "/downloaders");
  assert.equal(calls[0].method, "POST");
});

test("network rejection replaces stale active with a readable failed result", async () => {
  const { api } = apiFixture(() => { throw new TypeError("Failed to fetch"); });
  const result = await api.reverifyDownloader("fixture", row({ status: "active" }));
  assert.equal(result.status, "failed");
  assert.match(result.last_error, /Failed to fetch/);
});
test("saving configuration returns untested state and sends no automatic verification", async () => {
  const { api, calls } = apiFixture(() => row({ url: "http://changed.invalid" }));
  const result = await api.updateDownloader("fixture", {
    name: "Changed", client_type: "qbittorrent", url: "http://changed.invalid",
  });
  assert.equal(result.status, "pending");
  assert.equal(result.last_error, null);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].method, "PATCH");
});

// A small hook/JSX fixture executes the actual component without DOM or browser.
function componentFixture(api) {
  let current;
  const jsx = (type, props) => ({ type, props });
  const react = {
    useState(initial) {
      const index = current.index++;
      const owner = current;
      if (!(index in owner.states)) owner.states[index] = typeof initial === "function" ? initial() : initial;
      return [owner.states[index], (next) => {
        owner.states[index] = typeof next === "function" ? next(owner.states[index]) : next;
      }];
    },
    useRef(value) { return react.useState(() => ({ current: value }))[0]; },
    useCallback(fn) { return fn; },
    useEffect(fn, deps) {
      const index = current.index++;
      const previous = current.deps[index];
      if (!previous || deps.some((dep, i) => dep !== previous[i])) current.effects.push(fn);
      current.deps[index] = deps;
    },
  };
  const noop = () => null;
  const mocks = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "@radix-ui/react-dropdown-menu": Object.fromEntries(["Root", "Trigger", "Portal", "Content", "Item"].map((key) => [key, key])),
    "@/lib/api/downloaders": api,
    "@/lib/use-visible-polling": { useVisiblePolling: () => { throw new Error("Untested configurations must not start polling"); } },
    "@/components/feedback": { useConfirm: () => async () => true },
    "@/lib/backdrop": { useBackdrop: () => ({}) },
    "@/lib/downloader-limits-capabilities": { DOWNLOADER_LIMITS_CAPABILITIES: {}, editableLimitFields: () => [] },
    "@/components/icons": new Proxy({}, { get: () => noop }),
    "@/components/directory-picker": { DirectoryPicker: noop },
    "@/components/modal": { Modal: noop },
    "@/components/transfer-mode-card": { TransferModeCard: noop },
    "@/vendor/liquid-glass": { LiquidGlassButton: noop },
  };
  const { DownloaderConfigSection } = load("../components/downloader-config-section.tsx", mocks);
  function instance(fn, props = {}) {
    const owner = { states: [], deps: [], effects: [], index: 0 };
    return { render(next = props) {
      props = next; current = owner; owner.index = 0;
      const tree = fn(props);
      const effects = owner.effects.splice(0); effects.forEach((effect) => effect());
      return tree;
    } };
  }
  return { root: instance(DownloaderConfigSection), instance };
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  return [tree, ...nodes(tree.props?.children)];
}
const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };

test("untested rows do not poll, allow manual test, and show local verifying", async () => {
  let finish;
  const fixture = componentFixture({
    listDownloaders: async () => [row(), row({ id: "other", status: "unverified" })],
    reverifyDownloader: () => new Promise((resolve) => { finish = resolve; }),
  });
  fixture.root.render(); await settle();
  const rowElement = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  const instance = fixture.instance(rowElement.type, rowElement.props);
  let tree = instance.render();
  let menu = nodes(tree).find((node) => node.type?.name === "DownloaderActionsMenu");
  const menuTree = fixture.instance(menu.type, menu.props).render();
  const verifyItem = nodes(menuTree).find((node) => node.props?.children === "重新测试连接");
  assert.equal(verifyItem.props.disabled, false);
  menu.props.onReverify();
  tree = instance.render();
  assert.ok(nodes(tree).some((node) => node.props?.children?.includes?.("测试中")));
  finish(row({ status: "active" })); await settle();
  const updated = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  assert.equal(updated.props.downloader.status, "active");
});

test("late verification cannot restore active after configuration update", async () => {
  let finish;
  const fixture = componentFixture({
    listDownloaders: async () => [row({ status: "active" })],
    reverifyDownloader: () => new Promise((resolve) => { finish = resolve; }),
    updateDownloader: async () => row({ url: "http://changed.invalid" }),
  });
  fixture.root.render(); await settle();
  const rowElement = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  const instance = fixture.instance(rowElement.type, { ...rowElement.props, expanded: true, editing: true });
  const tree = instance.render();
  const menu = nodes(tree).find((node) => node.type?.name === "DownloaderActionsMenu");
  menu.props.onReverify();
  const form = nodes(tree).find((node) => node.type?.name === "DownloaderForm");
  await form.props.onSubmit({ name: "Changed", client_type: "qbittorrent", url: "http://changed.invalid" });
  finish(row({ status: "active" })); await settle();
  const updated = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  assert.equal(updated.props.downloader.status, "pending");
  assert.equal(updated.props.downloader.url, "http://changed.invalid");
});

test("saving sets busy and disables concurrent reverify in actions menu", async () => {
  let finishSave;
  const fixture = componentFixture({
    listDownloaders: async () => [row({ status: "active" })],
    updateDownloader: () => new Promise((resolve) => { finishSave = resolve; }),
  });
  fixture.root.render(); await settle();
  const rowElement = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  const instance = fixture.instance(rowElement.type, { ...rowElement.props, expanded: true, editing: true });
  let tree = instance.render();
  const form = nodes(tree).find((node) => node.type?.name === "DownloaderForm");
  const savePromise = form.props.onSubmit({ name: "Changed", client_type: "qbittorrent", url: "http://changed.invalid" });
  tree = instance.render();
  const menu = nodes(tree).find((node) => node.type?.name === "DownloaderActionsMenu");
  assert.equal(menu.props.busy, true, "Menu must be busy while save is pending");
  finishSave(row({ url: "http://changed.invalid" }));
  await savePromise;
  await settle();
});

test("old verification finishing cannot unlock an overlapping save or admit another verification", async () => {
  let finishVerify, finishSave;
  let verifyCalls = 0;
  const fixture = componentFixture({
    listDownloaders: async () => [row({ status: "active" })],
    reverifyDownloader: () => {
      verifyCalls++;
      return new Promise((resolve) => { finishVerify = resolve; });
    },
    updateDownloader: () => new Promise((resolve) => { finishSave = resolve; }),
  });
  fixture.root.render(); await settle();
  const element = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  const instance = fixture.instance(element.type, { ...element.props, expanded: true, editing: true });
  let tree = instance.render();
  const staleMenu = nodes(tree).find((node) => node.type?.name === "DownloaderActionsMenu");
  staleMenu.props.onReverify();
  const form = nodes(tree).find((node) => node.type?.name === "DownloaderForm");
  const save = form.props.onSubmit({ name: "Changed", client_type: "qbittorrent", url: "http://changed.invalid" });
  finishVerify(row({ status: "active" })); await settle();
  tree = instance.render();
  const menu = nodes(tree).find((node) => node.type?.name === "DownloaderActionsMenu");
  assert.equal(menu.props.busy, true);
  const menuTree = fixture.instance(menu.type, menu.props).render();
  assert.equal(nodes(menuTree).find((node) => node.props?.children === "重新测试连接").props.disabled, true);
  // Even a callback retained before React rerenders must not start a request.
  staleMenu.props.onReverify(); await settle();
  assert.equal(verifyCalls, 1);
  finishSave(row({ url: "http://changed.invalid" })); await save; await settle();
  const updated = nodes(fixture.root.render()).find((node) => node.type?.name === "DownloaderRow");
  assert.equal(updated.props.downloader.url, "http://changed.invalid");
  assert.equal(updated.props.downloader.status, "pending");
});
