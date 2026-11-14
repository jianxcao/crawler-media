import test from "node:test";
import assert from "node:assert/strict";
import { parseUnitSegment, playHref } from "../lib/player/play-links.ts";

// 真实生产 seam：`web/lib/player/play-links.ts` 的 `playHref`（签发播放页地址）与
// `parseUnitSegment`（`web/app/play/[mediaItemId]/[[...unit]]/page.tsx:37` 用它在服务端
// 段里还原季集）。plan:294-295 的 `movieRequest.season_number === undefined` 在 web 面的
// 真实等价物 = `parseUnitSegment(undefined) === null` ⇒ 播放页 `parsed?.season` 为
// undefined ⇒ 播放请求不带季集。

test("playHref keeps an explicit file_id and omits movie coordinates", () => {
  const href = playHref("movie-1", { fileId: "file-b" });
  assert.equal(href, "/play/movie-1?file_id=file-b");
  assert.equal(href.includes("/s"), false);

  const tvHref = playHref("tv-1", { season: 1, episode: 2, fileId: "file-b" });
  assert.equal(tvHref, "/play/tv-1/s01e02?file_id=file-b");
});

test("parseUnitSegment reports no coordinates for a movie address", () => {
  // 电影地址没有季集段 ⇒ null，播放页据此发不带 season/episode 的请求。
  assert.equal(parseUnitSegment(undefined), null);
  assert.equal(parseUnitSegment(""), null);
  assert.equal(parseUnitSegment("movie"), null);
  assert.equal(parseUnitSegment("s01"), null);
  assert.equal(parseUnitSegment("e02"), null);
});

test("parseUnitSegment restores TV coordinates case-insensitively", () => {
  assert.deepEqual(parseUnitSegment("s01e02"), { season: 1, episode: 2 });
  assert.deepEqual(parseUnitSegment("S1E2"), { season: 1, episode: 2 });
  assert.deepEqual(parseUnitSegment("s100e1000"), { season: 100, episode: 1000 });
});
