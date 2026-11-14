import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

function fixture() {
  const calls = [];
  const module = { exports: {} };
  const filename = new URL("../lib/api/transfer-settings.ts", import.meta.url);
  const code = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
    fileName: filename.pathname,
  }).outputText;
  vm.runInNewContext(code, {
    module, exports: module.exports,
    require: () => ({ request: async (path, init) => {
      calls.push({ path, method: init?.method ?? "GET", body: init?.body });
      return { data: { revision: 8, transfer_mode: "copy", watch_intake: null } };
    } }),
  }, { filename: filename.pathname });
  return { api: module.exports, calls };
}

test("clearing Watch sends an explicit null and the observed revision", async () => {
  const { api, calls } = fixture();
  const saved = await api.patchTransferSettings(7, { watch_intake: null });
  assert.equal(saved.watch_intake, null);
  assert.deepEqual(calls, [{ path: "/settings/transfer", method: "PATCH",
    body: JSON.stringify({ expected_revision: 7, watch_intake: null }) }]);
});

test("changing Transfer mode writes only that field without reading a stale aggregate", async () => {
  const { api, calls } = fixture();
  await api.patchTransferSettings(7, { transfer_mode: "copy" });
  assert.deepEqual(calls, [{ path: "/settings/transfer", method: "PATCH",
    body: JSON.stringify({ expected_revision: 7, transfer_mode: "copy" }) }]);
});

test("Transfer settings load from the dedicated current endpoint", async () => {
  const { api, calls } = fixture();
  assert.equal((await api.getTransferSettings()).revision, 8);
  assert.deepEqual(calls, [{ path: "/settings/transfer", method: "GET", body: undefined }]);
});
