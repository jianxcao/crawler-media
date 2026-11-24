import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function fixture(relative, api = {}, libraries = []) {
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
    "@/lib/api/libraries": { listLibraries: async () => libraries },
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
    const app = fixture(
      "subscription-adjust-dialog",
      {
        previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }, { season_number: 2 }] }),
        previewSubscriptionDownloadRouting: async () => ({ ok: true }),
        updateSubscription: async () => ({}),
        getSubscriptionRemovalPreview: async () => ({ ...preview, torrent_count: total, library_file_count: total }),
        cleanupSubscriptionSeasons: async () => ({}),
      },
      [{ id: "lib-default", name: "默认剧集库", kind: "tv", is_default: true }],
    );
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

test("adjust dialog passes tmdb_id unconditionally, guards default-library option & save, renders staging_path, and disables save on failed preview", async () => {
  const explicitCalls = [];
  const explicitApp = fixture("subscription-adjust-dialog", {
    previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }] }),
    previewSubscriptionDownloadRouting: async (...args) => {
      explicitCalls.push(args);
      return { ok: true, mode: "inplace", entry_dir: "/tv/china/两京十五日 (2026)/Season 01" };
    },
    updateSubscription: async () => ({}),
  });
  explicitApp.render({
    detail: {
      id: "s1",
      media: { kind: "tv", tmdb_id: "203483", title: "两京十五日", year: 2026 },
      coverage: { kind: "tv", season: 1 },
      wanted: [],
      library_id: "lib-china",
    },
    onClose() {},
    onSaved() {},
  });
  await settle();
  assert.equal(explicitCalls.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(explicitCalls[0])), [
    "tv",
    "lib-china",
    "203483",
    { title: "两京十五日", year: 2026 },
  ]);

  // Default library exists -> passes tmdb_id unconditionally & renders selectable default-library option & staging_path
  const defaultCalls = [];
  const defaultApp = fixture(
    "subscription-adjust-dialog",
    {
      previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }, { season_number: 2 }] }),
      previewSubscriptionDownloadRouting: async (...args) => {
        defaultCalls.push(args);
        return {
          ok: true,
          mode: "watch",
          path: "/downloads/watch",
          staging_path: "/downloads/staging/两京十五日 (2026)",
        };
      },
      updateSubscription: async () => ({}),
    },
    [{ id: "lib-default", name: "默认剧集库", kind: "tv", is_default: true }],
  );
  const defaultProps = {
    detail: {
      id: "s2",
      media: { kind: "tv", tmdb_id: "203483", title: "两京十五日", year: 2026 },
      coverage: { kind: "tv", season: 1 },
      wanted: [],
      library_id: null,
    },
    onClose() {},
    onSaved() {},
  };
  defaultApp.render(defaultProps);
  await settle();
  const renderedDefault = defaultApp.render(defaultProps);
  assert.equal(defaultCalls.length, 1);
  assert.deepEqual(JSON.parse(JSON.stringify(defaultCalls[0])), [
    "tv",
    null,
    "203483",
    { title: "两京十五日", year: 2026 },
  ]);
  const placeholderOption = nodes(renderedDefault).find(
    (node) => node.type === "option" && node.props?.value === "",
  );
  assert.ok(placeholderOption, "must render default-library option when default exists");
  assert.equal(text(placeholderOption), "（按默认库路由）");
  assert.ok(!placeholderOption.props.disabled);
  assert.match(
    text(renderedDefault),
    /整理到.*\/downloads\/staging\/两京十五日 \(2026\).*文件进入媒体库根目录后自动入账/,
  );

  // No default library + detail.library_id === null -> disabled placeholder, save disabled until explicit library picked
  let previewOk = true;
  const noDefaultApp = fixture(
    "subscription-adjust-dialog",
    {
      previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }, { season_number: 2 }] }),
      previewSubscriptionDownloadRouting: async () =>
        previewOk ? { ok: true, mode: "inplace", entry_dir: "/tv/anime" } : { ok: false, warning: "下载器未连接" },
      updateSubscription: async () => ({}),
    },
    [{ id: "lib-anime", name: "动漫库", kind: "tv", is_default: false }],
  );
  const noDefaultProps = {
    detail: {
      id: "s3",
      media: { kind: "tv", tmdb_id: "203483", title: "两京十五日", year: 2026 },
      coverage: { kind: "tv", season: 1 },
      wanted: [],
      library_id: null,
    },
    onClose() {},
    onSaved() {},
  };
  noDefaultApp.render(noDefaultProps);
  await settle();
  let noDefaultTree = noDefaultApp.render(noDefaultProps);
  const disabledPlaceholder = nodes(noDefaultTree).find(
    (node) => node.type === "option" && node.props?.value === "",
  );
  assert.ok(disabledPlaceholder, "must render disabled placeholder when library_id is null and no default exists");
  assert.equal(disabledPlaceholder.props.disabled, true);
  assert.equal(text(disabledPlaceholder), "（当前默认库不可用，请选择目标媒体库）");

  // Even after changing season (dirty=true), save stays disabled while libraryId === null and no default exists
  nodes(noDefaultTree)
    .find((node) => node.type === "season" && node.props.season.season_number === 2)
    .props.onToggle();
  noDefaultTree = noDefaultApp.render(noDefaultProps);
  const saveBeforePick = nodes(noDefaultTree).find(
    (node) => node.type === "button" && text(node).includes("保存调整"),
  );
  assert.equal(saveBeforePick.props.disabled, true, "must disable saving null libraryId when no default exists");

  // Selecting an explicit library enables save when preview.ok === true
  nodes(noDefaultTree)
    .find((node) => node.type === "select")
    .props.onChange({ target: { value: "lib-anime" } });
  noDefaultApp.render(noDefaultProps);
  await settle();
  noDefaultTree = noDefaultApp.render(noDefaultProps);
  const saveAfterPick = nodes(noDefaultTree).find(
    (node) => node.type === "button" && text(node).includes("保存调整"),
  );
  assert.equal(saveAfterPick.props.disabled, false);

  // When preview.ok === false, save button is disabled
  previewOk = false;
  const badPreviewApp = fixture(
    "subscription-adjust-dialog",
    {
      previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }, { season_number: 2 }] }),
      previewSubscriptionDownloadRouting: async () => ({ ok: false, warning: "没有可用下载器" }),
      updateSubscription: async () => ({}),
    },
    [{ id: "lib-default", name: "默认库", kind: "tv", is_default: true }],
  );
  badPreviewApp.render(defaultProps);
  await settle();
  let badTree = badPreviewApp.render(defaultProps);
  nodes(badTree)
    .find((node) => node.type === "season" && node.props.season.season_number === 2)
    .props.onToggle();
  badTree = badPreviewApp.render(defaultProps);
  const saveOnFailedPreview = nodes(badTree).find(
    (node) => node.type === "button" && text(node).includes("保存调整"),
  );
  assert.equal(saveOnFailedPreview.props.disabled, true, "must disable save when preview.ok === false");

  // Explicit library + no default library -> must NOT offer <option value="">
  const explicitNoDefaultApp = fixture(
    "subscription-adjust-dialog",
    {
      previewSubscriptionTitle: async () => ({ status: "ready", seasons: [{ season_number: 1 }] }),
      previewSubscriptionDownloadRouting: async () => ({ ok: true, mode: "inplace", entry_dir: "/tv/anime" }),
      updateSubscription: async () => ({}),
    },
    [{ id: "lib-anime", name: "动漫库", kind: "tv", is_default: false }],
  );
  explicitNoDefaultApp.render({
    detail: {
      id: "s4",
      media: { kind: "tv", tmdb_id: "203483", title: "两京十五日", year: 2026 },
      coverage: { kind: "tv", season: 1 },
      wanted: [],
      library_id: "lib-anime",
    },
    onClose() {},
    onSaved() {},
  });
  await settle();
  const explicitNoDefaultTree = explicitNoDefaultApp.render({
    detail: {
      id: "s4",
      media: { kind: "tv", tmdb_id: "203483", title: "两京十五日", year: 2026 },
      coverage: { kind: "tv", season: 1 },
      wanted: [],
      library_id: "lib-anime",
    },
    onClose() {},
    onSaved() {},
  });
  const emptyOpt = nodes(explicitNoDefaultTree).find(
    (node) => node.type === "option" && node.props?.value === "",
  );
  assert.equal(emptyOpt, undefined, "must not offer empty default-library option when no default library exists and library_id is non-null");
});

test("SubscriptionCancelDialog intercepts Escape in capture phase when open and raised so parent SubscribeDialog stays open", async () => {
  const captureListeners = [];
  const bubbleListeners = [];
  const prevWindow = globalThis.window;
  const fakeWindow = {
    addEventListener(type, fn, opts) {
      const capture = typeof opts === "boolean" ? opts : Boolean(opts?.capture);
      if (type === "keydown") {
        (capture ? captureListeners : bubbleListeners).push(fn);
      }
    },
    removeEventListener(type, fn, opts) {
      const capture = typeof opts === "boolean" ? opts : Boolean(opts?.capture);
      if (type === "keydown") {
        const list = capture ? captureListeners : bubbleListeners;
        const idx = list.indexOf(fn);
        if (idx >= 0) list.splice(idx, 1);
      }
    },
  };
  globalThis.window = fakeWindow;

  try {
    let parentClosed = false;
    let cancelClosed = false;
    bubbleListeners.push((e) => {
      if (e.key === "Escape") parentClosed = true;
    });

    // Inject globalThis.window into fixture via read/transpile
    const source = readFileSync(new URL("../components/subscription-cancel-dialog.tsx", import.meta.url), "utf8");
    const code = ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    const cleanups = [];
    const jsx = (type, props) => ({ type, props });
    const mod = { exports: {} };
    vm.runInNewContext(code, {
      window: fakeWindow,
      globalThis: { window: fakeWindow },
      module: mod,
      exports: mod.exports,
      require: (name) => {
        if (name === "react") {
          return {
            useState: (init) => [typeof init === "function" ? init() : init, () => {}],
            useEffect: (fn) => {
              const cleanup = fn();
              if (typeof cleanup === "function") cleanups.push(cleanup);
            },
          };
        }
        if (name === "react/jsx-runtime") return { jsx, jsxs: jsx };
        if (name === "@/components/modal") return { Modal: "modal" };
        if (name === "@/lib/format") return { formatBytes: (v) => `${v} B` };
        if (name === "@/lib/api/subscriptions") {
          return { getSubscriptionRemovalPreview: async () => preview };
        }
        throw new Error(`unexpected import ${name}`);
      },
    });

    mod.exports.SubscriptionCancelDialog({
      open: true,
      raised: true,
      subscriptionId: "s1",
      title: "Fixture",
      onClose: () => {
        cancelClosed = true;
      },
      onConfirm: () => {},
    });

    assert.ok(captureListeners.length > 0, "raised SubscriptionCancelDialog must register capture-phase keydown listener");

    let stopped = false;
    const escEvent = {
      key: "Escape",
      preventDefault() {},
      stopPropagation() {
        stopped = true;
      },
    };
    for (const fn of [...captureListeners]) fn(escEvent);
    if (!stopped) {
      for (const fn of [...bubbleListeners]) fn(escEvent);
    }

    assert.equal(stopped, true, "Escape in raised SubscriptionCancelDialog must stopPropagation");
    assert.equal(cancelClosed, true, "Escape must close SubscriptionCancelDialog");
    assert.equal(parentClosed, false, "Parent SubscribeDialog bubbling Escape listener must not fire");

    cleanups.forEach((fn) => fn());
    assert.equal(captureListeners.length, 0, "cleanup must remove capture listener");
  } finally {
    if (prevWindow === undefined) delete globalThis.window;
    else globalThis.window = prevWindow;
  }
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

test("SeasonCleanupDialog and SubscriptionCancelDialog use theme-safe .btn-danger for destructive confirm buttons", async () => {
  const seasonApp = fixture("season-cleanup-dialog");
  const seasonTree = seasonApp.render({
    open: true,
    title: "Fixture",
    seasons: [2],
    onClose() {},
    onConfirm() {},
  });
  const seasonConfirm = nodes(seasonTree).find(
    (node) => node.type === "button" && text(node).includes("清理第"),
  );
  assert.ok(seasonConfirm, "SeasonCleanupDialog confirm button must exist");
  assert.match(seasonConfirm.props.className, /\bbtn-danger\b/);
  assert.doesNotMatch(seasonConfirm.props.className, /bg-red-500\/85/);

  const cancelApp = fixture("subscription-cancel-dialog", {
    getSubscriptionRemovalPreview: async () => preview,
  });
  const cancelProps = {
    open: true,
    subscriptionId: "s1",
    title: "Fixture",
    onClose() {},
    onConfirm() {},
  };
  cancelApp.render(cancelProps);
  await settle();
  const cancelTree = cancelApp.render(cancelProps);
  const cancelConfirm = nodes(cancelTree).find(
    (node) => node.type === "button" && text(node).includes("取消订阅"),
  );
  assert.ok(cancelConfirm, "SubscriptionCancelDialog confirm button must exist");
  assert.match(cancelConfirm.props.className, /\bbtn-danger\b/);
  assert.doesNotMatch(cancelConfirm.props.className, /bg-red-500\/85/);
});
