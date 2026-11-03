import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function fixture(relative, api = {}) {
  const state = [], effects = [], deps = [];
  let index = 0;
  const jsx = (type, props) => ({ type, props });
  const react = {
    useState(initial) {
      const i = index++;
      if (!(i in state)) state[i] = typeof initial === "function" ? initial() : initial;
      return [state[i], (next) => { state[i] = typeof next === "function" ? next(state[i]) : next; }];
    },
    useMemo: (fn) => fn(),
    useEffect(fn, next) {
      const i = index++;
      if (!deps[i] || next.some((value, j) => value !== deps[i][j])) effects.push(fn);
      deps[i] = next;
    },
  };
  const messages = [];
  const toast = Object.fromEntries(["success", "info", "error"].map((kind) => [kind, (text) => messages.push({ kind, text })]));
  const mocks = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/components/modal": { Modal: "modal" },
    "@/components/season-cleanup-dialog": { SeasonCleanupDialog: "season-cleanup" },
    "@/components/subscribe-dialog": { SeasonRow: "season" },
    "@/components/feedback": { useToast: () => toast },
    "@/lib/permissions": { usePermissions: () => ({ canManageSubscriptions: true }) },
    "@/lib/subscription-form": { isLibraryDirty: (a, b) => a !== b, libraryPatch: () => ({}) },
    "@/lib/api/libraries": { listLibraries: async () => [] },
    "@/lib/format": { formatBytes: (value) => `${value} B` },
    "@/lib/api/subscriptions": api,
  };
  const filename = new URL(`../components/${relative}.tsx`, import.meta.url);
  const code = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(code, { module, exports: module.exports, require: (name) => {
    assert.ok(name in mocks, name); return mocks[name];
  } });
  const component = Object.values(module.exports)[0];
  return {
    messages,
    render(props) {
      index = 0;
      const tree = component(props);
      effects.splice(0).forEach((fn) => fn());
      return tree;
    },
  };
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  if (typeof tree.type === "function") return nodes(tree.type(tree.props));
  return [tree, ...nodes(tree.props?.children)];
}
function text(tree) {
  if (Array.isArray(tree)) return tree.map(text).join("");
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (!tree || typeof tree !== "object") return "";
  return text(typeof tree.type === "function" ? tree.type(tree.props) : tree.props?.children);
}
const settle = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
const preview = { library_file_count: 99, library_bytes: 999, torrent_count: 88,
  torrent_count_inferred: true, torrent_titles: [], retained_cross_season: [], hit_and_run_count: 0 };

test("season cleanup choices are unchecked, available and describe whole shared files without broad counts", async () => {
  const app = fixture("season-cleanup-dialog");
  let submitted;
  const props = { open: true, title: "Fixture", seasons: [2], preview,
    onClose() {}, onConfirm: async (options) => { submitted = options; } };
  const tree = app.render(props);
  const content = text(tree);
  assert.ok(!content.includes("99") && !content.includes("88"), "work-wide counts must not be presented as season counts");
  assert.match(content, /跨季.*(?:整个|整份).*文件/);
  assert.match(content, /物理删除/);
  assert.doesNotMatch(content, /不会删除|后台|任务中心/);
  const toggles = nodes(tree).filter((node) => node.type === "input");
  assert.equal(toggles.length, 2);
  for (const toggle of toggles) {
    assert.equal(toggle.props.checked, false);
    assert.ok(!toggle.props.disabled);
  }
  toggles[1].props.onChange({ target: { checked: true } });
  const updated = app.render(props);
  const confirm = nodes(updated).find((node) => node.type === "button" && text(node).includes("清理第"));
  await confirm.props.onClick();
  await settle();
  assert.equal(submitted.deleteTorrents, false);
  assert.equal(submitted.deleteLibraryFiles, true);
});

for (const total of [0, 99]) {
  test(`switching seasons offers explicit cleanup even with work-wide count ${total}`, async () => {
    const app = fixture("subscription-adjust-dialog", {
      previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }, { season_number: 2 }] }),
      previewSubscriptionDownloadRouting: async () => ({ ok: true }),
      updateSubscription: async () => ({}),
      getSubscriptionRemovalPreview: async () => ({ ...preview, torrent_count: total, library_file_count: total }),
      cleanupSubscriptionSeasons: async () => ({}),
    });
    let saved = false;
    const props = { detail: { id: "s1", media: { kind: "tv", tmdb_id: 1, title: "Fixture" },
      coverage: { kind: "tv", season: 1 }, wanted: [], library_id: null },
      onClose() {}, onSaved() { saved = true; } };
    app.render(props); await settle();
    let tree = app.render(props);
    nodes(tree).find((node) => node.type === "season" && node.props.season.season_number === 2).props.onToggle();
    tree = app.render(props);
    await nodes(tree).find((node) => node.type === "button" && text(node).includes("保存")).props.onClick();
    await settle();
    tree = app.render(props);
    assert.equal(tree.type, "season-cleanup");
    assert.equal(saved, false);
    assert.deepEqual(Array.from(tree.props.seasons), [1]);
    await tree.props.onConfirm({ deleteTorrents: false, deleteLibraryFiles: true });
    assert.equal(saved, true);
    assert.equal(app.messages.at(-1).kind, "success");
    assert.doesNotMatch(app.messages.at(-1).text, /后台|任务中心/);
  });
}

test("cancellation marks inferred torrent counts and keeps deletion opt in", async () => {
  const app = fixture("subscription-cancel-dialog", { getSubscriptionRemovalPreview: async () => preview });
  const props = { open: true, subscriptionId: "s1", title: "Fixture", onClose() {}, onConfirm() {} };
  app.render(props); await settle();
  const tree = app.render(props);
  assert.match(text(tree), /推断|估算/);
  for (const toggle of nodes(tree).filter((node) => node.type === "input")) assert.equal(toggle.props.checked, false);
});

test("cancellation explains synchronous cleanup after deletion is selected", async () => {
  const app = fixture("subscription-cancel-dialog", { getSubscriptionRemovalPreview: async () => preview });
  const props = { open: true, subscriptionId: "s1", title: "Fixture", onClose() {}, onConfirm() {} };
  app.render(props); await settle();
  const tree = app.render(props);
  nodes(tree).find((node) => node.type === "input").props.onChange({ target: { checked: true } });
  const selected = text(app.render(props));
  assert.doesNotMatch(selected, /后台|任务中心/);
  assert.match(selected, /等待.*结果/);
});
