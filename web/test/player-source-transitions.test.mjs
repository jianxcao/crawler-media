import test from "node:test";
import assert from "node:assert/strict";

import {
  initialTarget,
  navigateTarget,
  sessionUnit,
  playLinkFileId,
} from "../lib/player/active-target.ts";

const MEDIA = "m1";
const A = "file-a";

test("next/prev replace the whole target and drop the URL's physical source", () => {
  let target = initialTarget({ season: 1, episode: 1, fileId: A });
  assert.equal(sessionUnit(MEDIA, target).file_id, A, "the explicit initial source is honoured");
  target = navigateTarget(target, { season: 1, episode: 2 });
  const nextSessionRequest = sessionUnit(MEDIA, target);
  assert.equal(nextSessionRequest.episode_number, 2);
  assert.equal(nextSessionRequest.file_id, undefined);
  target = navigateTarget(target, { season: 1, episode: 1 });
  const prevSessionRequest = sessionUnit(MEDIA, target);
  assert.equal(prevSessionRequest.episode_number, 1);
  assert.equal(prevSessionRequest.file_id, undefined);
});

test("a merged file's next episode is a fresh logical target", () => {
  let target = initialTarget({ season: 1, episode: 2, fileId: A });
  target = navigateTarget(target, { season: 1, episode: 3 });
  assert.equal(sessionUnit(MEDIA, target).file_id, undefined);
});

test("the default displayed file is not an explicit choice", () => {
  const untouchedContinue = { file_id: playLinkFileId(null) };
  assert.equal(untouchedContinue.file_id, undefined);
  assert.equal(playLinkFileId(A), A);
});

test("a movie target carries no coordinates", () => {
  const unit = sessionUnit(MEDIA, initialTarget({}));
  assert.equal(unit.season_number, undefined);
  assert.equal(unit.episode_number, undefined);
  assert.equal(unit.file_id, undefined);
});
