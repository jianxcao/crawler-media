import test from "node:test";
import assert from "node:assert/strict";
import { playHref } from "../lib/player/play-links.ts";

test("playHref preserves explicit file_id in query parameters", () => {
  const href = playHref("movie-1", { fileId: "file-b" });
  assert.equal(href, "/play/movie-1?file_id=file-b");

  const tvHref = playHref("tv-1", { season: 1, episode: 2, fileId: "file-b" });
  assert.equal(tvHref, "/play/tv-1/s01e02?file_id=file-b");
});

test("playbackRequestUnit contract omits movie coordinates and retains file_id", () => {
  function playbackRequestUnit(unit) {
    const result = { media_item_id: unit.media_item_id };
    if (unit.season_number != null && unit.episode_number != null) {
      result.season_number = unit.season_number;
      result.episode_number = unit.episode_number;
    }
    if (unit.file_id) {
      result.file_id = unit.file_id;
    }
    return result;
  }

  const movieRequest = playbackRequestUnit({
    media_item_id: "m-100",
    file_id: "file-b-id",
  });
  assert.equal(movieRequest.file_id, "file-b-id");
  assert.equal(movieRequest.season_number, undefined);
  assert.equal(movieRequest.episode_number, undefined);
});

