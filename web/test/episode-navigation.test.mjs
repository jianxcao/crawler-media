import test from "node:test";
import assert from "node:assert/strict";

import {
  nextOwnedEpisode,
  prevOwnedEpisode,
} from "../lib/player/episode-navigation.ts";

test("nextOwnedEpisode does not leak episodes from another season", () => {
  const episodes = [
    { season_number: 1, episode_number: 1, owned: true },
    { season_number: 2, episode_number: 2, owned: true },
  ];
  assert.equal(nextOwnedEpisode(episodes, 1, 1), null);
});

test("nextOwnedEpisode finds next owned episode in the same season", () => {
  const episodes = [
    { season_number: 1, episode_number: 1, owned: true },
    { season_number: 1, episode_number: 2, owned: false },
    { season_number: 1, episode_number: 3, owned: true },
    { season_number: 2, episode_number: 2, owned: true },
  ];
  const next = nextOwnedEpisode(episodes, 1, 1);
  assert.notEqual(next, null);
  assert.equal(next?.episode_number, 3);
});

test("prevOwnedEpisode does not leak episodes from another season", () => {
  const episodes = [
    { season_number: 2, episode_number: 1, owned: true },
    { season_number: 1, episode_number: 2, owned: true },
  ];
  assert.equal(prevOwnedEpisode(episodes, 1, 2), null);
});

test("prevOwnedEpisode finds previous owned episode in the same season", () => {
  const episodes = [
    { season_number: 1, episode_number: 1, owned: true },
    { season_number: 1, episode_number: 2, owned: false },
    { season_number: 1, episode_number: 3, owned: true },
  ];
  const prev = prevOwnedEpisode(episodes, 1, 3);
  assert.notEqual(prev, null);
  assert.equal(prev?.episode_number, 1);
});
