import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function load(relative, overrides = {}) {
  const jsx = (type, props) => ({ type, props });
  const noop = () => null;
  const fallback = new Proxy({}, { get: (_target, key) => key === "__esModule" ? false : noop });
  const mocks = {
    react: { useState: (value) => [typeof value === "function" ? value() : value, noop],
      useMemo: (fn) => fn(), useRef: (current) => ({ current }), useEffect: noop },
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "react-router-dom": { Link: "a" },
    "@/components/modal": { Modal: ({ children }) => children },
    "@/components/overflow-text": { OverflowText: ({ children }) => children },
    "@/lib/jobs": { useJobs: () => ({ refresh: noop, upsert: noop }) },
    "@/components/feedback": { useToast: () => ({ success: noop, error: noop }) },
    "@/lib/format": { formatBytes: (bytes) => `${bytes} B` },
    "@/lib/time": fallback,
    "@/lib/player/bandwidth": { formatBandwidth: () => null },
    "@/lib/language-labels": { languageLabel: (value) => value },
    ...overrides,
  };
  const filename = new URL(relative, import.meta.url);
  const module = { exports: {} };
  const code = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  vm.runInNewContext(code, { module, exports: module.exports, require: (name) => mocks[name] ?? fallback });
  return module.exports;
}
function expand(tree) {
  if (Array.isArray(tree)) return tree.flatMap(expand);
  if (!tree || typeof tree !== "object") return [tree];
  if (typeof tree.type === "function") return expand(tree.type(tree.props));
  return [tree, ...expand(tree.props?.children)];
}
const text = (tree) => expand(tree).filter((node) => typeof node === "string" || typeof node === "number").join("");

test("task center renders actual task snapshots without sources, units or placeholder errors", () => {
  const task = { id: "s1:fixture", info_hash: null, name: "Fixture task", downloader_id: null,
    downloader_name: null, progress: null, size_bytes: 100, dlspeed_bytes: null, upspeed_bytes: null,
    uploaded_bytes: null, completed_bytes: null, state: "error", site_id: "site1",
    media_item_id: "media1", media_title: "Fixture Media", media_kind: "movie", score: 80,
    subscriptions: [{ id: "s1", media_item_id: "media1", media_title: "Fixture Media", media_kind: "movie" }] };
  const group = { key: "media:media1", mediaItemId: "media1", title: "Fixture Media", kind: "movie", tasks: [task] };
  const { TaskCenterView } = load("../components/task-center-view.tsx", {
    "@/lib/download-tasks": { useDownloadTasks: () => ({ tasks: [task], loading: false, error: null, refreshedAt: null, refresh() {} }) },
    "@/lib/download-task-actions": { shouldOfferInlineReplacement: () => true },
    "@/lib/task-activity": { useTaskActivity: () => ({ ingestJobsByHash: new Map(),
      attentionDownloadGroups: [group], activeDownloadGroups: [], boostTasks: [], standaloneAttentionJobs: [],
      standaloneActiveJobs: [], standaloneHistoricalJobs: [], attentionTotal: 1, activeTotal: 0 }),
      taskSourceOf: () => "subscription", isSubscriptionTask: () => true, downloadTaskNeedsAttention: () => true },
  });
  let rendered;
  assert.doesNotThrow(() => { rendered = TaskCenterView({ view: "all", onViewChange() {} }); });
  const content = text(rendered);
  assert.match(content, /Fixture Media/);
  assert.match(content, /下载器报告该任务出错/);
  assert.match(content, /换种/);
});

test("browser diagnostics retain session decisions and a working close control without server diagnostics", () => {
  const { DiagnosticsPanel } = load("../components/player/diagnostics-panel.tsx");
  let closed = false;
  const panel = DiagnosticsPanel({ session: { session_id: "session-fixture", hw_backend: null,
    source: { container: "mkv", video_codec: "h264", resolution: "1080p" },
    decision: { tier: 1, container: "mp4", audio_tracks: [], video: { action: "copy" }, reason: "Fixture decision" } },
    engine: null, qoe: () => ({}), landscape: false, onClose() { closed = true; } });
  const content = text(panel);
  assert.match(content, /播放诊断/);
  assert.match(content, /session-fixture/);
  assert.match(content, /1080p/);
  assert.match(content, /Fixture decision/);
  assert.match(content, /掉帧/);
  const close = expand(panel).find((node) => node?.props?.["aria-label"] === "关闭播放诊断");
  close.props.onClick();
  assert.equal(closed, true);
});
