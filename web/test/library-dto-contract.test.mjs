import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function loadLibraries(request) {
  const source = await readFile(new URL("../lib/api/libraries.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((dependency) => {
    if (dependency === "@/lib/http") return { request, resolveRequestUrl: (p) => p };
    if (dependency.endsWith("library-filter")) return { filterQuery: () => undefined };
    return {};
  }, module, module.exports);
  return module.exports;
}

const envelope = (data) => ({ ok: true, data });

test("unidentified groups read only the backend DTO fields", async () => {
  const api = await loadLibraries(async () =>
    envelope([
      {
        key: "/m/a.mkv",
        label: "a.mkv",
        library_id: "lib",
        library_name: "Movies",
        file_count: 1,
        total_size_bytes: 5,
        files: [{ file_id: "/m/a.mkv", file_path: "/m/a.mkv" }],
      },
      { path: "/legacy.mkv", file_name: "legacy.mkv", size_bytes: 9 },
    ]),
  );
  const [current, legacy] = await api.listUnidentifiedLibraryFiles("lib");
  assert.equal(current.key, "/m/a.mkv");
  assert.equal(current.label, "a.mkv");
  assert.equal(current.total_size_bytes, 5);
  assert.equal(current.files[0].id, "/m/a.mkv");
  assert.equal(current.files[0].file_path, "/m/a.mkv");
  assert.equal(legacy.key, "", "legacy path alias must not be read");
  assert.equal(legacy.total_size_bytes, 0, "legacy size alias must not be read");
});

test("missing files read only the backend DTO fields", async () => {
  const api = await loadLibraries(async () =>
    envelope([
      {
        media_item_id: "m1",
        kind: "tv",
        title: "Show",
        year: 2020,
        files: [
          { id: "f1", file_path: "/tv/a.mkv", season_number: 1, episode_number: 2, size_bytes: 0 },
          { file_id: "f2", path: "/tv/b.mkv", season: 3, episode: 4 },
        ],
      },
    ]),
  );
  const [item] = await api.listMissingLibraryFiles("lib");
  assert.deepEqual(item.files[0], {
    id: "f1",
    file_path: "/tv/a.mkv",
    season_number: 1,
    episode_number: 2,
    size_bytes: 0,
  });
  assert.equal(item.files[1].id, "");
  assert.equal(item.files[1].file_path, "");
  assert.equal(item.files[1].season_number, 0);
});

test("non-array payloads are schema errors, not empty success", async () => {
  const api = await loadLibraries(async () => envelope({ unexpected: true }));
  await assert.rejects(api.listUnidentifiedLibraryFiles("lib"));
  await assert.rejects(api.listMissingLibraryFiles("lib"));
});

test("chapter refresh keeps the canonical object and rejects the legacy array", async () => {
  const canonicalPayload = { chapters: [{ title: "Intro", start_ms: 0 }], fingerprint_refresh_queued: true };
  const ok = await loadLibraries(async () => envelope(canonicalPayload));
  assert.deepEqual(await ok.refreshItemChapters("lib", "m", { file_id: "f" }, "reuse_valid"), canonicalPayload);
  const legacy = await loadLibraries(async () => envelope([{ title: "Intro", start_ms: 0 }]));
  await assert.rejects(legacy.refreshItemChapters("lib", "m", { file_id: "f" }, "reuse_valid"), /contract/i);
});

test("obsolete trash and duplicate adapters are gone", async () => {
  const api = await loadLibraries(async () => envelope(null));
  for (const name of [
    "listTrashedFiles", "purgeTrashedFiles", "restoreTrashedFiles", "listDuplicateFiles",
    "startDuplicateScan", "resolveDuplicates", "resolveAllDuplicates",
  ]) {
    assert.equal(api[name], undefined, name);
  }
});

test("metadata refresh returns the synchronous refreshed count, not a fake job", async () => {
  const api = await loadLibraries(async () => envelope({ refreshed: 3 }));
  const library = await api.startLibraryMetadataRefresh("lib");
  assert.deepEqual(library, { refreshed: 3 });
  const item = await api.refreshItemMetadata("lib", "m");
  assert.deepEqual(item, { refreshed: 3 });
});

test("list endpoints reject a non-array payload instead of rendering empty", async () => {
  const api = await loadLibraries(async () => envelope({ items: [] }));
  await assert.rejects(api.listLibraries(), /响应格式错误/);
  await assert.rejects(api.listLibraryItems("lib"), /响应格式错误/);
  await assert.rejects(api.listLibraryItemIndex("lib"), /响应格式错误/);
});

test('chapter clients carry selected file, identity pair, retry body and abort signal', async () => {
  const calls = [];
  const payload = { task: { task_id: 'current', generation: 'g', state: 'queued' }, empty: false, superseded: false };
  const api = await loadLibraries(async (path, init) => { calls.push({ path, init }); return envelope(path.includes('/refresh') ? { chapters: [], fingerprint_refresh_queued: false } : payload); });
  const target = { file_id: 'version-b', season: 2, episode: 3 };
  const signal = new AbortController().signal;
  await api.fetchItemChapters('lib', 'm', target, signal);
  assert.deepEqual(await api.generateItemChapters('lib', 'm', target, true, signal), payload);
  await api.refreshItemChapters('lib', 'm', target, 'reuse_valid', signal);
  await api.fetchChapterFrameStatus('lib', 'm', target, { task_id: 'current', generation: 'g' }, signal);
  await api.refreshItemChapters('lib', 'm', target, 'recapture', signal);
  for (const call of calls) {
    const query = new URL(call.path, 'http://test').searchParams;
    assert.equal(query.get('file_id'), 'version-b'); assert.equal(query.get('season'), '2'); assert.equal(query.get('episode'), '3'); assert.equal(call.init.signal, signal);
  }
  assert.equal(calls[1].init.body, JSON.stringify({ retry_failed: true }));
  assert.equal(calls[2].init.body, JSON.stringify({ capture_policy: 'reuse_valid' }));
  assert.equal(calls[4].init.body, JSON.stringify({ capture_policy: 'recapture' }));
  const status = new URL(calls[3].path, 'http://test').searchParams;
  assert.equal(status.get('task_id'), 'current'); assert.equal(status.get('generation'), 'g');
});
