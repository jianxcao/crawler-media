import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";

/** 加载真实 TS 适配器，只伪造 HTTP request。 */
async function load(file, request, extra = {}) {
  const source = await readFile(new URL(`../lib/api/${file}`, import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((name) => {
    if (name === "@/lib/http") return { request, resolveRequestUrl: (p) => p };
    if (name in extra) return extra[name];
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return module.exports;
}

const accepted = { operation_id: "op-1", user_id: "u1", phase: "cleaning", state: "pending", error: null };

test("删除适配器返回受理的操作 DTO，而不是 void/deleted", async () => {
  const calls = [];
  const request = async (path, init) => {
    calls.push([path, init?.method]);
    return { ok: true, data: accepted };
  };
  const members = await load("members.ts", request);
  assert.deepEqual(await members.deleteMember("u1"), accepted);
  assert.deepEqual(calls, [["/users/u1", "DELETE"]]);
});

test("轮询状态 pending → succeeded", async () => {
  const states = ["pending", "succeeded"];
  const paths = [];
  const request = async (path) => {
    paths.push(path);
    return { ok: true, data: { ...accepted, state: states.shift(), phase: "finalizing" } };
  };
  const members = await load("members.ts", request);
  const done = await members.waitForUserRemoval(accepted, { sleep: async () => {} });
  assert.equal(done.state, "succeeded");
  assert.deepEqual(paths, ["/users/removals/op-1", "/users/removals/op-1"]);
});

test("失败可见：failed 立即返回并带错误，不假报完成", async () => {
  const request = async () => ({ ok: true, data: { ...accepted, state: "failed", error: "locked" } });
  const members = await load("members.ts", request);
  const done = await members.waitForUserRemoval(accepted, { sleep: async () => {} });
  assert.equal(done.state, "failed");
  assert.equal(done.error, "locked");
});

test("一直 pending 时在尝试次数内停下并返回 pending", async () => {
  let n = 0;
  const request = async () => { n += 1; return { ok: true, data: accepted }; };
  const members = await load("members.ts", request);
  const done = await members.waitForUserRemoval(accepted, { attempts: 3, sleep: async () => {} });
  assert.equal(done.state, "pending");
  assert.equal(n, 3);
});
