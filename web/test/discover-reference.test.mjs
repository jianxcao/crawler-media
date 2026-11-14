import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";

async function apiFixture() {
  const source = await readFile(new URL("../lib/api/discover.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const paths = [], module = { exports: {} };
  new Function("require", "module", "exports", js)(name => {
    if (name === "@/lib/http") return { request: async path => { paths.push(path); throw new Error("request observed"); } };
    if (name === "@/lib/image-proxy") return { cachedImageUrl: value => value };
    if (name === "@/lib/discovery-section-state" || name === "@/lib/discovery-collection-cache") return {};
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return { api: module.exports, paths };
}

test("details reject retired and unsupported references before issuing HTTP", async () => {
  const { api, paths } = await apiFixture();
  for (const reference of ["603", "tvdb:tv:603", "tmdb:bad:603", "tmdb:movie:", "douban:movie:1291843", "tmdb:movie:603/extra"]) {
    await assert.rejects(api.fetchDiscoveredTitleDetails(reference), /reference/i);
  }
  assert.deepEqual(paths, []);
});

test("supported references preserve their exact current endpoint", async () => {
  const { api, paths } = await apiFixture();
  for (const reference of ["tmdb:movie:603", "tmdb:tv:1399", "douban:1291843"]) {
    await assert.rejects(api.fetchDiscoveredTitleDetails(reference), /request observed/);
  }
  assert.deepEqual(paths, ["/media/movie/603", "/media/tv/1399", "/media/douban/1291843"]);
});
