import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import {
  historyReplayOptions,
  isMediaHistoryProvider,
  toMediaSearchItem,
} from "../lib/search-history.ts";

const readWeb = (relativePath) =>
  readFile(new URL(`../${relativePath}`, import.meta.url), "utf8");

test("manual download submission preserves selected destination and route identity", async () => {
  const [api, dialog, backend] = await Promise.all([
    readWeb("lib/api/downloaders.ts"),
    readWeb("components/download-target-dialog.tsx"),
    readFile(new URL("../../crates/api/src/http/downloaders/tasks.rs", import.meta.url), "utf8"),
  ]);

  assert.match(dialog, /save_path: option\.savePath/);
  assert.match(dialog, /option\.kind === "smart" && identity && option\.savePath/);
  assert.match(dialog, /auto_route: true/);
  assert.match(api, /save_path: payload\.save_path/);
  assert.match(api, /payload\.auto_route \? \{ auto_route: true \}/);
  assert.match(backend, /save_path: Option<String>/);
  assert.match(backend, /auto_route: bool/);
});

test("search history identifiers stay opaque strings through delete and snapshot APIs", async () => {
  const [api, command, results] = await Promise.all([
    readWeb("lib/api/search.ts"),
    readWeb("components/search-command.tsx"),
    readWeb("components/search-results.tsx"),
  ]);

  assert.match(api, /historyId: string/);
  assert.match(api, /deleteSearchHistoryEntry\(id: string\)/);
  assert.doesNotMatch(command, /Number\(item\.id\)|Number\(id\)/);
  assert.doesNotMatch(results, /getTorrentSearchHistoryResults\(Number\(/);
});

test("history replay keeps the history snapshot id and routes torrent history to torrent search", async () => {
  assert.deepEqual(
    historyReplayOptions({ id: "user:abc", provider: "titles" }),
    { vertical: "media", snapshotId: "user:abc" },
  );
  assert.deepEqual(
    historyReplayOptions({ id: "user:def", provider: "torrents" }),
    { vertical: "torrent", snapshotId: "user:def" },
  );
  assert.equal(isMediaHistoryProvider("torrents"), false);
  assert.equal(isMediaHistoryProvider("titles"), true);
  assert.equal(isMediaHistoryProvider(null), false);

  const [command] = await Promise.all([readWeb("components/search-command.tsx")]);
  assert.match(command, /onSearch\(item\.query, SCOPE_ALL, historyReplayOptions\(item\)\)/);
});

test("title snapshot DTO maps provider and snake_case fields into media search items", async () => {
  const item = toMediaSearchItem(
    {
      provider: "douban",
      external_id: "subject-42",
      kind: "movie",
      title: "Example",
      year: 2024,
      original_title: "Original Example",
      poster_url: "https://image.example/poster.jpg",
    },
    (url) => `cached:${url}`,
  );

  assert.deepEqual(item, {
    id: "subject-42",
    source: "douban",
    title: "Example",
    year: 2024,
    type: "movie",
    rating: 0,
    posterUrl: "cached:https://image.example/poster.jpg",
  });

  const api = await readWeb("lib/api/search.ts");
  assert.match(api, /items: \(snap\.items \?\? \[\]\)\.map\(titleHitToItem\)/);
});

test("media search renders every supported metadata source and keeps torrent categories", async () => {
  const [mediaTypes, mediaResults, torrentResults] = await Promise.all([
    readWeb("lib/media-types.ts"),
    readWeb("components/media-search-results.tsx"),
    readWeb("components/search-results.tsx"),
  ]);

  for (const source of ["tmdb", "douban", "tvdb", "bangumi", "anilist"]) {
    assert.ok(mediaTypes.includes(`"${source}"`), `MediaSource includes ${source}`);
    assert.ok(mediaResults.includes(`"${source}"`), `search UI renders ${source}`);
  }
  assert.match(torrentResults, /category: hit\.category/);
});

test("torrent search endpoint permits the empty keyword used by browse mode", async () => {
  const backend = await readFile(
    new URL("../../crates/api/src/http/search.rs", import.meta.url),
    "utf8",
  );
  const torrentEndpoints = backend.split("pub(crate) async fn search_torrents")[1]
    .split("pub(crate) async fn search_stream")[0] +
    backend.split("pub(crate) async fn search_stream")[1].split("struct SiteSearchResult")[0];
  assert.doesNotMatch(torrentEndpoints, /if keyword\.is_empty\(\)\s*\{\s*return err\(StatusCode::BAD_REQUEST, "search\.keyword"/);
});
