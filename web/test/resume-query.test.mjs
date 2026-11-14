import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

/**
 * 续播查询的序列化：季集是**一对**坐标。
 *
 * 只有季号或只有集号都不是一个合法的播放单元——旧实现把缺的一半序列化成
 * 字符串 "undefined" 发给后端，等于告诉服务端「第 undefined 集」。
 */

async function loadPlayback() {
  const source = await readFile(new URL("../lib/api/playback.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((dependency) => {
    if (dependency === "@/lib/http") {
      return {
        getAuthToken: () => null,
        request: async () => ({ ok: true, data: {} }),
        resolveRequestUrl: (path) => path,
      };
    }
    if (dependency === "@/lib/env") return { publicEnv: { apiBaseUrl: "/api/v1" } };
    if (dependency === "@/lib/player/device") return { getPlayerDeviceId: () => "device" };
    throw new Error(`Unexpected import ${dependency}`);
  }, module, module.exports);
  return module.exports;
}

test("只有季号时不带集号，也不出现 undefined 字面量", async () => {
  const { buildResumeQuery } = await loadPlayback();
  const query = buildResumeQuery({ media_item_id: "m1", season_number: 1 });
  assert.equal(query.get("media_item_id"), "m1");
  assert.equal(query.has("episode_number"), false);
  assert.equal(query.has("season_number"), false);
  assert.equal(query.toString().includes("undefined"), false);
});

test("季集成对才有坐标，显式源偏好照常带上", async () => {
  const { buildResumeQuery } = await loadPlayback();
  const query = buildResumeQuery({
    media_item_id: "m1",
    season_number: 2,
    episode_number: 5,
    file_id: "f1",
  });
  assert.equal(query.get("season_number"), "2");
  assert.equal(query.get("episode_number"), "5");
  assert.equal(query.get("file_id"), "f1");
});
