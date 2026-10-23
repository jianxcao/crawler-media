import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";
import { waitForLibraryOperation, operationLabel } from "../lib/library-operations.ts";

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

test("a later failure surfaces the backend error and stops polling", async () => {
  let calls = 0;
  await assert.rejects(
    waitForLibraryOperation(operation("applying"), async () => {
      calls += 1;
      throw new Error("boom");
    }, { intervalMs: 0 }),
    /boom/,
  );
  assert.equal(calls, 1);
  await assert.rejects(
    waitForLibraryOperation(operation("applying"), async () => operation("failed", { error: "迁移失败" }), {
      intervalMs: 0,
    }),
    /迁移失败/,
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
  const source = await readFile(new URL("../lib/api/libraries.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  const removals = await import("../lib/library-removal.ts");
  const operations = await import("../lib/library-operations.ts");
  new Function("require", "module", "exports", js)((name) => {
    if (name === "@/lib/http") return { request, resolveRequestUrl: (path) => path };
    if (name === "@/lib/library-removal") return { waitForLibraryRemoval: removals.waitForLibraryRemoval };
    if (name === "@/lib/library-operations") return operations;
    if (name === "@/lib/library-file-pages") {
      return { collectAllFilePages: async (firstPage) => firstPage.files };
    }
    if (name === "@/lib/library-filter") return {};
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return module.exports;
}

const library = { id: "lib", name: "电影", kind: "movie", root_paths: ["/media"] };

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

test("updateLibraryAndWait polls the operation then re-reads the library", async () => {
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
    { path: "/libraries", method: "GET" },
  ]);
});

test("updateLibraryAndWait returns a synchronous update without polling", async () => {
  const requests = [];
  const api = await loadApi(async (path, init) => {
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
  const source = await readFile(new URL("../components/library-form-dialog.tsx", import.meta.url), "utf8");
  assert.match(source, /updateLibraryAndWait\(library\.id, payload/);
  assert.match(source, /onProgress: \(operation\) => setPending\(operationLabel\(operation\)\)/);
  assert.match(source, /busy \? \(pending \?\? "保存中…"\) : "保存"/);
  assert.doesNotMatch(source, /\bupdateLibrary\(/, "the dialog must not use the fire-and-forget update");
});
