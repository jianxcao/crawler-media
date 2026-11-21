/* global AbortSignal */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

import {
  buildOrganizeRenames,
  canOpenOrganizeDialog,
  organizeTargets,
} from "../lib/library-organize.ts";
import { loadLibrariesApi as loadLibrariesApiModule } from "./support/load-libraries-api.mjs";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

const lib = (id, kind, roots, extra = {}) => ({ id, kind, name: id, root_paths: roots, ...extra });

async function loadLibrariesApi(request) {
  return loadLibrariesApiModule({
    "@/lib/http": { request, resolveRequestUrl: (p) => p },
    "@/lib/library-removal": { waitForLibraryRemoval: async (op) => op },
    "@/lib/library-operations": { waitForLibraryOperation: async (op) => op },
    "@/lib/library-file-pages": { collectAllFilePages: async (p) => p.files },
    "@/lib/library-filter": {},
  });
}

function loadOrganizeDialog(previewLibraryOrganize) {
  const filename = path.join(webRoot, "components/library-organize-dialog.tsx");
  const js = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const organizeHelpers = ts.transpileModule(
    readFileSync(path.join(webRoot, "lib/library-organize.ts"), "utf8"),
    { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
  ).outputText;
  const helpersMod = { exports: {} };
  new Function("require", "module", "exports", organizeHelpers)(require, helpersMod, helpersMod.exports);

  const mod = { exports: {} };
  new Function("require", "module", "exports", js)((name) => {
    if (name === "@/components/modal") {
      return { Modal: ({ children }) => React.createElement("div", { "data-modal": true }, children) };
    }
    if (name === "@/lib/api/libraries") return { previewLibraryOrganize };
    if (name === "@/lib/library-organize") return helpersMod.exports;
    return require(name);
  }, mod, mod.exports);
  return mod.exports.LibraryOrganizeDialog;
}

test("targets are the current Library plus other Libraries of the same kind", () => {
  const current = lib("a", "movie", ["/m/a"]);
  const all = [current, lib("b", "movie", ["/m/b"]), lib("t", "tv", ["/t"]), lib("c", "movie", [])];
  assert.deepEqual(
    organizeTargets(current, all).map((l) => l.id),
    ["a", "b"],
    "same kind only, never a rootless Library",
  );
});

test("same-Library organize sends plain from/to without to_library_id", () => {
  const current = lib("a", "movie", ["/m/a"]);
  const renames = [{ from: "/m/a/x.mkv", to: "/m/a/X (2000)/X (2000).mkv" }];
  assert.deepEqual(buildOrganizeRenames(renames, current, current), renames);
});

test("cross-Library organize preserves backend-previewed target paths and sets to_library_id", () => {
  const current = lib("a", "movie", ["/m/a", "/m/a2"]);
  const target = lib("b", "movie", ["/m/b1", "/m/b2"]);
  const renames = [
    { file_id: "f1", from: "/m/a/X (2000)/X (2000).mkv", to: "/m/b2/X (2000)/X (2000).mkv" },
    { file_id: "f2", from: "/m/a2/y.mkv", to: "/m/b2/Y (2001)/Y (2001).mkv" },
  ];
  assert.deepEqual(buildOrganizeRenames(renames, current, target), [
    { from: "/m/a/X (2000)/X (2000).mkv", to: "/m/b2/X (2000)/X (2000).mkv", to_library_id: "b" },
    { from: "/m/a2/y.mkv", to: "/m/b2/Y (2001)/Y (2001).mkv", to_library_id: "b" },
  ]);
});

test("all-already_ok source library can still open organize dialog when cross-library target or extra root exists", () => {
  const singleRoot = lib("a", "movie", ["/m/a"]);
  const multiRoot = lib("a", "movie", ["/m/a1", "/m/a2"]);
  const otherSameKind = lib("b", "movie", ["/m/b"]);
  const otherKind = lib("t", "tv", ["/t"]);

  assert.equal(canOpenOrganizeDialog(singleRoot, [singleRoot, otherSameKind], { renames: [], already_ok: 2 }), true);
  assert.equal(canOpenOrganizeDialog(multiRoot, [multiRoot], { renames: [], already_ok: 2 }), true);
  assert.equal(canOpenOrganizeDialog(singleRoot, [singleRoot, otherKind], { renames: [], already_ok: 2 }), false);
  assert.equal(canOpenOrganizeDialog(singleRoot, [singleRoot, otherSameKind], { renames: [], already_ok: 0 }), false);
  assert.equal(
    canOpenOrganizeDialog(singleRoot, [singleRoot], { renames: [{ from: "/m/a/x.mkv", to: "/m/a/X/X.mkv" }], already_ok: 0 }),
    true,
  );
});

test("previewLibraryOrganize sends optional target_library_id, target_root, and abort signal", async () => {
  const calls = [];
  const api = await loadLibrariesApi(async (reqPath, init) => {
    calls.push({ reqPath, init });
    return { ok: true, data: { library_id: "b", root: "/m/b2", renames: [], skipped: 0, already_ok: 0 } };
  });
  const controller = new AbortController();
  await api.previewLibraryOrganize("a");
  await api.previewLibraryOrganize("a", {
    targetLibraryId: "b",
    targetRoot: "/m/b2",
    signal: controller.signal,
  });
  assert.equal(calls[0].reqPath, "/libraries/a/organize-preview");
  const url = new URL(calls[1].reqPath, "http://test");
  assert.equal(url.pathname, "/libraries/a/organize-preview");
  assert.equal(url.searchParams.get("target_library_id"), "b");
  assert.equal(url.searchParams.get("target_root"), "/m/b2");
  assert.equal(calls[1].init.signal, controller.signal);
});

test("dialog opens for all-already_ok source, re-previews target library and root, and submits full manifest", async () => {
  const source = lib("a", "movie", ["/m/a"], { name: "源电影库" });
  const target = lib("b", "movie", ["/m/b1", "/m/b2"], { name: "目标电影库" });
  const initialPreview = { library_id: "a", root: "/m/a", renames: [], skipped: 0, already_ok: 2 };
  const previewCalls = [];
  const Dialog = loadOrganizeDialog(async (id, options) => {
    previewCalls.push({ id, targetLibraryId: options?.targetLibraryId, targetRoot: options?.targetRoot, signal: options?.signal });
    const destRoot = options.targetRoot ?? "/m/b1";
    return {
      library_id: options.targetLibraryId,
      root: destRoot,
      already_ok: 0,
      skipped: 0,
      renames: [
        { file_id: "f1", from: "/m/a/Movie A (2020)/Movie A (2020).mkv", to: `${destRoot}/Movie A (2020)/Movie A (2020).mkv` },
        { file_id: "f2", from: "/m/a/Movie B (2021)/Movie B (2021).mkv", to: `${destRoot}/Movie B (2021)/Movie B (2021).mkv` },
      ],
    };
  });

  let submitted = null;
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(Dialog, {
        library: source,
        libraries: [source, target],
        preview: initialPreview,
        onClose() {},
        onSubmit(renames) { submitted = renames; },
      }),
    );
  });

  try {
    const submitBtn = () => renderer.root.findAllByType("button").find((b) => b.children.join("") === "开始整理");
    assert.equal(submitBtn().props.disabled, true, "0 renames in source library must disable submit");
    assert.match(JSON.stringify(renderer.toJSON()), /0 个文件/);

    // Switch to target library b
    const libSelect = renderer.root.findByProps({ id: "organize-target" });
    await act(async () => { libSelect.props.onChange({ target: { value: "b" } }); });
    assert.equal(previewCalls.length, 1);
    assert.deepEqual(
      { id: previewCalls[0].id, targetLibraryId: previewCalls[0].targetLibraryId, targetRoot: previewCalls[0].targetRoot },
      { id: "a", targetLibraryId: "b", targetRoot: "/m/b1" },
    );
    assert.ok(previewCalls[0].signal instanceof AbortSignal);
    assert.match(JSON.stringify(renderer.toJSON()), /2 个文件/);
    assert.equal(submitBtn().props.disabled, false);

    // Switch target root to /m/b2
    const rootSelect = renderer.root.findByProps({ id: "organize-target-root" });
    await act(async () => { rootSelect.props.onChange({ target: { value: "/m/b2" } }); });
    assert.equal(previewCalls.length, 2);
    assert.equal(previewCalls[1].targetRoot, "/m/b2");

    // Submit cross-library manifest
    await act(async () => { submitBtn().props.onClick(); });
    assert.deepEqual(submitted, [
      { from: "/m/a/Movie A (2020)/Movie A (2020).mkv", to: "/m/b2/Movie A (2020)/Movie A (2020).mkv", to_library_id: "b" },
      { from: "/m/a/Movie B (2021)/Movie B (2021).mkv", to: "/m/b2/Movie B (2021)/Movie B (2021).mkv", to_library_id: "b" },
    ]);

    // Switch back to initial source library — restores initial preview without refetching
    await act(async () => { libSelect.props.onChange({ target: { value: "a" } }); });
    assert.equal(previewCalls.length, 2, "returning to initial (library, root) must restore initial preview without refetching");
    assert.equal(submitBtn().props.disabled, true);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});

test("partially-conforming source library re-previews target library including previously already_ok files", async () => {
  const source = lib("a", "movie", ["/m/a"], { name: "源库" });
  const target = lib("b", "movie", ["/m/b"], { name: "目标库" });
  const initialPreview = {
    library_id: "a",
    root: "/m/a",
    already_ok: 1,
    skipped: 0,
    renames: [
      { file_id: "f2", from: "/m/a/raw-b.mkv", to: "/m/a/Movie B (2021)/Movie B (2021).mkv" },
    ],
  };
  const Dialog = loadOrganizeDialog(async (_id, options) => ({
    library_id: options.targetLibraryId,
    root: options.targetRoot,
    already_ok: 0,
    skipped: 0,
    renames: [
      { file_id: "f1", from: "/m/a/Movie A (2020)/Movie A (2020).mkv", to: "/m/b/Movie A (2020)/Movie A (2020).mkv" },
      { file_id: "f2", from: "/m/a/raw-b.mkv", to: "/m/b/Movie B (2021)/Movie B (2021).mkv" },
    ],
  }));

  let submitted = null;
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(Dialog, {
        library: source,
        libraries: [source, target],
        preview: initialPreview,
        onClose() {},
        onSubmit(renames) { submitted = renames; },
      }),
    );
  });

  try {
    assert.match(JSON.stringify(renderer.toJSON()), /1 个文件/);
    const libSelect = renderer.root.findByProps({ id: "organize-target" });
    await act(async () => { libSelect.props.onChange({ target: { value: "b" } }); });
    assert.match(JSON.stringify(renderer.toJSON()), /2 个文件/);
    const submitBtn = renderer.root.findAllByType("button").find((b) => b.children.join("") === "开始整理");
    await act(async () => { submitBtn.props.onClick(); });
    assert.deepEqual(submitted, [
      { from: "/m/a/Movie A (2020)/Movie A (2020).mkv", to: "/m/b/Movie A (2020)/Movie A (2020).mkv", to_library_id: "b" },
      { from: "/m/a/raw-b.mkv", to: "/m/b/Movie B (2021)/Movie B (2021).mkv", to_library_id: "b" },
    ]);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});
