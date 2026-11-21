import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";
import {
  LibraryOperationError,
  waitForLibraryOperation,
  operationLabel,
} from "../lib/library-operations.ts";
import { loadLibrariesApi } from "./support/load-libraries-api.mjs";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

const operation = (state, extra = {}) => ({
  operation_id: "op",
  kind: "roots",
  state,
  phase: state,
  cursor: null,
  error: null,
  ...extra,
});

test("accepted reconfiguration stays pending through planning and applying until success", async () => {
  const states = ["frozen", "applying", "succeeded"];
  const seen = [];
  const result = await waitForLibraryOperation(operation("planned"), async () => operation(states.shift()), {
    intervalMs: 0,
    onProgress: (value) => seen.push(value.state),
  });
  assert.equal(result.state, "succeeded");
  assert.deepEqual(seen, ["planned", "frozen", "applying", "succeeded"]);
});

test("a freshly accepted operation is polled even when its first read is failed", async () => {
  const states = ["applying", "succeeded"];
  const result = await waitForLibraryOperation(operation("failed", { error: "old timeout" }), async (id) => {
    assert.equal(id, "op", "polling must address the accepted operation id");
    return operation(states.shift());
  }, { intervalMs: 0 });
  assert.equal(result.state, "succeeded");
});

test("a later failure surfaces the backend error as LibraryOperationError with operation_id and stops polling", async () => {
  let calls = 0;
  await assert.rejects(
    waitForLibraryOperation(operation("applying"), async () => {
      calls += 1;
      throw new Error("boom");
    }, { intervalMs: 0 }),
    /boom/,
  );
  assert.equal(calls, 1);
  const failedOp = operation("failed", { operation_id: "op-fail", cursor: "batch-2", error: "迁移失败" });
  await assert.rejects(
    waitForLibraryOperation(operation("applying", { operation_id: "op-fail" }), async () => failedOp, {
      intervalMs: 0,
    }),
    (err) => {
      assert.ok(err instanceof LibraryOperationError);
      assert.equal(err.operationId, "op-fail");
      assert.deepEqual(err.operation, failedOp);
      assert.match(err.message, /迁移失败/);
      return true;
    },
  );
});

test("abort cancels the pending poll without another status request", async () => {
  const controller = new AbortController();
  let requested = false;
  const pending = waitForLibraryOperation(operation("applying"), async () => {
    requested = true;
    return operation("succeeded");
  }, { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(requested, false);
});

test("pre-aborted wait does not fetch status or publish progress", async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    waitForLibraryOperation(operation("planned"), async () => {
      assert.fail("must not fetch");
    }, { signal: controller.signal, onProgress: () => assert.fail("must not publish") }),
    { name: "AbortError" },
  );
});

test("operation states have visible copy", () => {
  assert.match(operationLabel(operation("planned")), /正在准备/);
  assert.match(operationLabel(operation("frozen")), /正在冻结媒体库/);
  assert.match(operationLabel(operation("applying")), /正在迁移文件归属/);
  assert.match(operationLabel(operation("reticulating")), /正在等待操作完成.*reticulating/);
  assert.match(operationLabel(operation("failed", { error: "busy" })), /操作失败.*busy/);
});

async function loadApi(request) {
  const removals = await import("../lib/library-removal.ts");
  const operations = await import("../lib/library-operations.ts");
  return loadLibrariesApi({
    "@/lib/http": { request, resolveRequestUrl: (path) => path },
    "@/lib/library-removal": { waitForLibraryRemoval: removals.waitForLibraryRemoval },
    "@/lib/library-operations": operations,
    "@/lib/library-file-pages": { collectAllFilePages: async (firstPage) => firstPage.files },
    "@/lib/library-filter": {},
  });
}

const library = { id: "lib", name: "电影", kind: "movie", root_paths: ["/media"] };

test("root consolidation uses the current root operation and waits for completion", async () => {
  const requests = [];
  let changed = false;
  const api = await loadApi(async (path, init = {}) => {
    const method = init.method ?? "GET";
    requests.push({ path, method });
    if (path === "/libraries?manage=1") {
      return { ok: true, data: [{ ...library, root_paths: changed ? ["/media"] : ["/media", "/duplicate"] }] };
    }
    if (method === "PATCH") {
      assert.equal(path, "/libraries/lib");
      assert.deepEqual(JSON.parse(init.body), { root_paths: ["/media"] });
      return { ok: true, data: operation("planned") };
    }
    assert.equal(path, "/libraries/lib/operations/op");
    changed = true;
    return { ok: true, data: operation("succeeded") };
  });
  const result = await api.applyRootConsolidations("lib", ["/duplicate"], { intervalMs: 0 });
  assert.deepEqual(result, { consolidated: 1, roots: ["/media"] });
  assert.deepEqual(requests.map((r) => r.path), [
    "/libraries?manage=1", "/libraries/lib", "/libraries/lib/operations/op", "/libraries?manage=1",
  ]);
});

test("PATCH acceptance returns the operation instead of a library", async () => {
  const api = await loadApi(async (path, init) => {
    assert.equal(path, "/libraries/lib");
    assert.equal(init.method, "PATCH");
    assert.deepEqual(JSON.parse(init.body), { root_paths: ["/media2"] });
    return { ok: true, data: { operation_id: "op-1", kind: "roots", state: "planned" } };
  });
  const updated = await api.updateLibrary("lib", { root_paths: ["/media2"] });
  assert.equal(updated.kind, "operation");
  assert.equal(updated.operation.operation_id, "op-1");
});

test("a synchronous field update still returns the library", async () => {
  const api = await loadApi(async (path, init) => {
    assert.equal(path, "/libraries/lib");
    assert.deepEqual(JSON.parse(init.body), { name: "新名字" });
    return { ok: true, data: { ...library, name: "新名字" } };
  });
  const updated = await api.updateLibrary("lib", { name: "新名字" });
  assert.equal(updated.kind, "library");
  assert.equal(updated.library.name, "新名字");
});

test("updateLibraryAndWait polls the operation then re-reads the library via manage=1", async () => {
  const requests = [];
  const api = await loadApi(async (path, init = {}) => {
    requests.push({ path, method: init.method ?? "GET" });
    if (init.method === "PATCH") return { ok: true, data: { operation_id: "op-1", kind: "roots", state: "planned" } };
    if (path.includes("/operations/")) return { ok: true, data: { operation_id: "op-1", kind: "roots", state: "succeeded" } };
    return { ok: true, data: [{ ...library, root_paths: ["/media2"] }] };
  });
  const updated = await api.updateLibraryAndWait("lib", { root_paths: ["/media2"] }, { intervalMs: 0 });
  assert.deepEqual(updated.root_paths, ["/media2"]);
  assert.deepEqual(requests, [
    { path: "/libraries/lib", method: "PATCH" },
    { path: "/libraries/lib/operations/op-1", method: "GET" },
    { path: "/libraries?manage=1", method: "GET" },
  ]);
});

test("updateLibraryAndWait returns a synchronous update without polling", async () => {
  const requests = [];
  const api = await loadApi(async (path) => {
    requests.push(path);
    return { ok: true, data: { ...library, name: "新名字" } };
  });
  const updated = await api.updateLibraryAndWait("lib", { name: "新名字" }, { intervalMs: 0 });
  assert.equal(updated.name, "新名字");
  assert.deepEqual(requests, ["/libraries/lib"]);
});

test("reidentify keeps its synchronous contract and is never polled", async () => {
  const requests = [];
  const api = await loadApi(async (path, init) => {
    requests.push({ path, method: init.method, body: JSON.parse(init.body) });
    return { ok: true, data: { claimed: 2 } };
  });
  const pinned = {
    media_item_id: "m1",
    source_revision: 3,
    title_ref: "tmdb:1",
    files: [
      { file_id: "f1", revision: 1, source_version: "v1" },
      { file_id: "f2", revision: 2, source_version: "v2" },
    ],
  };
  const result = await api.assignLibraryFilesToTitle(pinned);
  assert.equal(result.claimed, 2);
  // The previewed versions go out exactly; no bare file_ids list survives.
  assert.deepEqual(requests, [{ path: "/reidentify", method: "POST", body: pinned }]);
});

test("the edit dialog waits for the accepted operation and shows its phase", async () => {
  const source = await readFile(new URL("../components/library-form/edit-library-dialog.tsx", import.meta.url), "utf8");
  assert.match(source, /updateLibraryAndWait\(library\.id, payload/);
  assert.match(source, /onProgress: \(operation\) => setPending\(operationLabel\(operation\)\)/);
  assert.match(source, /busy \? \(pending \?\? "保存中…"\) : "保存"/);
  assert.doesNotMatch(source, /\bupdateLibrary\(/, "the dialog must not use the fire-and-forget update");
});

test("unidentified claim sends only the explicit identify_and_transfer action", async () => {
  const requests = [];
  const api = await loadApi(async (path, init) => {
    requests.push({ path, method: init.method, body: JSON.parse(init.body) });
    return { ok: true, data: {} };
  });
  await api.claimUnidentifiedFile({
    path: "/in/x.mkv",
    title: "X",
    kind: "movie",
    year: 2001,
    tmdb_id: "7",
  });
  assert.deepEqual(requests, [{
    path: "/unidentified/claim",
    method: "POST",
    body: {
      path: "/in/x.mkv",
      action: { type: "identify_and_transfer", title: "X", kind: "movie", year: 2001, tmdb_id: "7" },
    },
  }]);
});

test("retryLibraryOperation sends POST .../operations/{id}/retry with no body and retryLibraryOperationAndWait polls same ID", async () => {
  const requests = [];
  const controller = new AbortController();
  const states = ["applying", "succeeded"];
  const api = await loadApi(async (reqPath, init = {}) => {
    requests.push({ path: reqPath, method: init.method ?? "GET", body: init.body, signal: init.signal });
    if (init.method === "POST") {
      return { ok: true, data: { operation_id: "op/1", state: "failed" } };
    }
    if (reqPath.includes("/operations/")) {
      return { ok: true, data: { operation_id: "op/1", kind: "roots", state: states.shift(), cursor: "c2", error: null } };
    }
    if (reqPath === "/libraries?manage=1") {
      return { ok: true, data: [{ ...library, root_paths: ["/media2"] }] };
    }
    return { ok: true, data: [] };
  });

  const updated = await api.retryLibraryOperationAndWait("lib", "op/1", {
    intervalMs: 0,
    signal: controller.signal,
  });
  assert.deepEqual(updated.root_paths, ["/media2"]);
  assert.deepEqual(
    requests.map(({ path: reqPath, method, body }) => ({ path: reqPath, method, body })),
    [
      { path: "/libraries/lib/operations/op%2F1/retry", method: "POST", body: undefined },
      { path: "/libraries/lib/operations/op%2F1", method: "GET", body: undefined },
      { path: "/libraries/lib/operations/op%2F1", method: "GET", body: undefined },
      { path: "/libraries?manage=1", method: "GET", body: undefined },
    ],
  );
  assert.ok(requests.every((r) => r.signal === controller.signal));
});

test("reindexingOperationId extracts operation_id from 409 library.reindexing errors only", async () => {
  const api = await loadApi(async () => ({ ok: true, data: [] }));
  assert.equal(
    api.reindexingOperationId({
      status: 409,
      details: { ok: false, error: { code: "library.reindexing", operation_id: "op-frozen" } },
    }),
    "op-frozen",
  );
  assert.equal(
    api.reindexingOperationId({
      status: 409,
      details: { ok: false, error: { code: "library.root_conflict", operation_id: "op-x" } },
    }),
    null,
  );
  assert.equal(
    api.reindexingOperationId({
      status: 500,
      details: { ok: false, error: { code: "library.reindexing", operation_id: "op-x" } },
    }),
    null,
  );
  assert.equal(api.reindexingOperationId(new Error("plain")), null);
});

test("hidden library (admin_visible=false) works for applyRootConsolidations, updateLibraryAndWait, and retryLibraryOperationAndWait", async () => {
  const controller = new AbortController();
  let roots = ["/media", "/dup"];
  const hiddenLibrary = () => ({ ...library, id: "hidden", admin_visible: false, root_paths: roots });
  const requests = [];
  const api = await loadApi(async (reqPath, init = {}) => {
    requests.push({ path: reqPath, method: init.method ?? "GET", signal: init.signal });
    if (reqPath === "/libraries") return { ok: true, data: [] };
    if (reqPath === "/libraries?manage=1") return { ok: true, data: [hiddenLibrary()] };
    if (init.method === "PATCH") {
      const body = JSON.parse(init.body);
      roots = body.root_paths;
      return { ok: true, data: operation("frozen", { operation_id: "op-hidden" }) };
    }
    if (init.method === "POST" && reqPath.endsWith("/retry")) {
      return { ok: true, data: { operation_id: "op-hidden", state: "failed" } };
    }
    if (reqPath === "/libraries/hidden/operations/op-hidden") {
      return { ok: true, data: operation("succeeded", { operation_id: "op-hidden" }) };
    }
    throw new Error(`Unexpected request ${reqPath}`);
  });

  const consolidated = await api.applyRootConsolidations("hidden", ["/dup"], {
    intervalMs: 0,
    signal: controller.signal,
  });
  assert.deepEqual(consolidated, { consolidated: 1, roots: ["/media"] });

  const updated = await api.updateLibraryAndWait("hidden", { root_paths: ["/media2"] }, {
    intervalMs: 0,
    signal: controller.signal,
  });
  assert.equal(updated.id, "hidden");
  assert.deepEqual(updated.root_paths, ["/media2"]);

  const retried = await api.retryLibraryOperationAndWait("hidden", "op-hidden", {
    intervalMs: 0,
    signal: controller.signal,
  });
  assert.equal(retried.id, "hidden");
  assert.deepEqual(retried.root_paths, ["/media2"]);

  assert.equal(requests.some((r) => r.path === "/libraries"), false, "must never read default browse /libraries");
  assert.ok(requests.every((r) => r.signal === controller.signal), "must forward signal even with manage=1");

  await assert.rejects(
    api.applyRootConsolidations("missing-lib", ["/dup"], { intervalMs: 0 }),
    /媒体库不存在/,
  );
});

function loadEditDialog(apiOverrides) {
  const opsSource = ts.transpileModule(
    readFileSync(path.join(webRoot, "lib/library-operations.ts"), "utf8"),
    { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
  ).outputText;
  const opsMod = { exports: {} };
  new Function("require", "module", "exports", opsSource)(require, opsMod, opsMod.exports);

  const icon = () => React.createElement("span");
  const librariesStub = {
    createLibrary: async () => { throw new Error("unexpected createLibrary"); },
    listLibraryRoutingOptions: async () => ({ movie_genres: [], tv_genres: [], region_presets: [], country_names: {} }),
    reindexingOperationId: (error) => {
      if (!error || typeof error !== "object" || error.status !== 409) return null;
      const inner = error.details?.error;
      return inner?.code === "library.reindexing" && typeof inner.operation_id === "string" ? inner.operation_id : null;
    },
    ...apiOverrides,
  };
  const resolveFile = (base) => {
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = `${base}${suffix}`;
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module from ${base}`);
  };
  const cache = new Map();
  const loadModule = (filename) => {
    if (cache.has(filename)) return cache.get(filename).exports;
    const js = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    const mod = { exports: {} };
    cache.set(filename, mod);
    new Function("require", "module", "exports", js)((name) => {
      if (name === "@/components/modal") {
        return { Modal: ({ children }) => React.createElement("div", { "data-modal": true }, children) };
      }
      if (name === "@/components/directory-picker") return { DirectoryPicker: () => null };
      if (name === "@/components/tooltip") return { Tooltip: ({ children }) => children };
      if (name === "@/components/icons") {
        return { CheckIcon: icon, ChevronDownIcon: icon, FolderIcon: icon, InfoIcon: icon, PlusIcon: icon, XIcon: icon };
      }
      if (name === "@/components/library-kind-meta") {
        return { LIBRARY_KIND_META: { movie: { label: "电影", Icon: icon }, tv: { label: "剧集", Icon: icon }, video: { label: "其他", Icon: icon } } };
      }
      if (name === "@/lib/library-operations") return opsMod.exports;
      if (name === "@/lib/api/members") return { listMembers: async () => [] };
      if (name === "@/lib/api/subscriptions") return { listRuleSets: async () => [] };
      if (name === "@/lib/api/libraries") return librariesStub;
      if (name.startsWith("@/")) return loadModule(resolveFile(path.join(webRoot, name.slice(2))));
      if (name.startsWith(".")) return loadModule(resolveFile(path.resolve(path.dirname(filename), name)));
      return require(name);
    }, mod, mod.exports);
    return mod.exports;
  };
  const formDialog = loadModule(path.join(webRoot, "components/library-form-dialog.tsx"));
  return { LibraryFormDialog: formDialog.LibraryFormDialog, LibraryOperationError: opsMod.exports.LibraryOperationError };
}

test("EditLibraryDialog preserves failed operation_id and retries via POST .../operations/{id}/retry without re-sending PATCH", async () => {
  const events = [];
  let savedLib = null;
  let DialogError;
  const api = {
    updateLibraryAndWait: async (id, payload) => {
      events.push({ kind: "patch", id, payload });
      throw new DialogError({
        operation_id: "op-77",
        kind: "roots",
        state: "failed",
        cursor: "batch-3",
        error: "磁盘暂时不可用",
      });
    },
    retryLibraryOperationAndWait: async (id, opId) => {
      events.push({ kind: "retry", id, opId });
      return { ...library, root_paths: ["/media2"] };
    },
    getLibraryOperation: async () => { throw new Error("unexpected getLibraryOperation"); },
  };
  const loaded = loadEditDialog(api);
  DialogError = loaded.LibraryOperationError;

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(loaded.LibraryFormDialog, {
        state: { ...library, stats: { item_count: 0, file_count: 0, total_size_bytes: 0 }, access_mode: "everyone", admin_visible: true, is_default: false, root_missing: false, member_ids: [] },
        onClose() {},
        onSaved(saved) { savedLib = saved; },
      }),
    );
  });

  try {
    const findButton = (label) =>
      renderer.root.findAllByType("button").find((b) => b.children.join("") === label);

    await act(async () => { findButton("保存").props.onClick(); });
    assert.equal(events.length, 1);
    assert.equal(events[0].kind, "patch");
    assert.match(JSON.stringify(renderer.toJSON()), /磁盘暂时不可用/);
    assert.equal(findButton("保存"), undefined, "save button must not send a doomed PATCH while failedOperation is active");
    const retryBtn = findButton("重试操作");
    assert.ok(retryBtn, "must show retry action when operation fails");

    await act(async () => { retryBtn.props.onClick(); });
    assert.deepEqual(
      events.map((e) => ({ kind: e.kind, id: e.id, opId: e.opId })),
      [
        { kind: "patch", id: "lib", opId: undefined },
        { kind: "retry", id: "lib", opId: "op-77" },
      ],
    );
    assert.deepEqual(savedLib?.root_paths, ["/media2"]);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});

test("EditLibraryDialog recovers failed operation_id from 409 library.reindexing and retries it", async () => {
  const events = [];
  let savedLib = null;
  const loaded = loadEditDialog({
    updateLibraryAndWait: async (id) => {
      events.push({ kind: "patch", id });
      throw Object.assign(new Error("媒体库正在重建索引，请稍后重试"), {
        status: 409,
        details: { ok: false, error: { code: "library.reindexing", operation_id: "op-frozen" } },
      });
    },
    getLibraryOperation: async (id, opId) => {
      events.push({ kind: "get-op", id, opId });
      return { operation_id: opId, kind: "roots", state: "failed", cursor: "batch-1", error: "IO 写入失败" };
    },
    retryLibraryOperationAndWait: async (id, opId) => {
      events.push({ kind: "retry", id, opId });
      return { ...library, root_paths: ["/media2"] };
    },
  });

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(loaded.LibraryFormDialog, {
        state: { ...library, stats: { item_count: 0, file_count: 0, total_size_bytes: 0 }, access_mode: "everyone", admin_visible: true, is_default: false, root_missing: false, member_ids: [] },
        onClose() {},
        onSaved(saved) { savedLib = saved; },
      }),
    );
  });

  try {
    const findButton = (label) =>
      renderer.root.findAllByType("button").find((b) => b.children.join("") === label);

    await act(async () => { findButton("保存").props.onClick(); });
    assert.match(JSON.stringify(renderer.toJSON()), /IO 写入失败/);
    const retryBtn = findButton("重试操作");
    assert.ok(retryBtn);

    await act(async () => { retryBtn.props.onClick(); });
    assert.deepEqual(events, [
      { kind: "patch", id: "lib" },
      { kind: "get-op", id: "lib", opId: "op-frozen" },
      { kind: "retry", id: "lib", opId: "op-frozen" },
    ]);
    assert.deepEqual(savedLib?.root_paths, ["/media2"]);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});

test("LibraryManageView uses manage=1 in DuplicatesPanel and reloads after root consolidation", async () => {
  const duplicatesSource = await readFile(
    new URL("../components/library-manage/duplicates-panel.tsx", import.meta.url),
    "utf8",
  );
  const manageViewSource = await readFile(
    new URL("../components/library-manage-view.tsx", import.meta.url),
    "utf8",
  );
  assert.match(duplicatesSource, /function DuplicatesPanel[\s\S]*?listLibraries\(undefined,\s*\{\s*manage:\s*true\s*\}\)/);
  assert.match(manageViewSource, /onConsolidate:[\s\S]*?consolidateLibrary\([\s\S]*?reload\(\)/);
});
