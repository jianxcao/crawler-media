import assert from "node:assert/strict";
import test from "node:test";

import {
  MOBILE_HERO_HEIGHT,
  buildDetailPeopleList,
  computeHeroRecedeProgress,
  resolveImmersiveBackdropUrl,
  resolveMediaDetailOwnership,
  resolveMobileHeroSrc,
} from "../lib/media-detail-helpers.ts";

test("resolveMediaDetailOwnership hides buy/subscribe actions appropriately for owned movies vs TV", () => {
  assert.deepEqual(
    resolveMediaDetailOwnership({
      isMovie: true,
      libraryLinksCount: 1,
      hasSubscription: false,
      canSubscribe: true,
      canSearch: true,
    }),
    {
      ownedMovie: true,
      showSubscribeButton: false,
      showSearchButton: false,
    },
  );

  // Already-subscribed owned movie keeps manage subscription button, hides search
  assert.deepEqual(
    resolveMediaDetailOwnership({
      isMovie: true,
      libraryLinksCount: 2,
      hasSubscription: true,
      canSubscribe: true,
      canSearch: true,
    }),
    {
      ownedMovie: true,
      showSubscribeButton: true,
      showSearchButton: false,
    },
  );

  // Owned TV series keeps both buttons since episodes/future seasons may be tracked
  assert.deepEqual(
    resolveMediaDetailOwnership({
      isMovie: false,
      libraryLinksCount: 1,
      hasSubscription: false,
      canSubscribe: true,
      canSearch: true,
    }),
    {
      ownedMovie: false,
      showSubscribeButton: true,
      showSearchButton: true,
    },
  );

  // Unowned movie respects permissions
  assert.deepEqual(
    resolveMediaDetailOwnership({
      isMovie: true,
      libraryLinksCount: 0,
      hasSubscription: false,
      canSubscribe: true,
      canSearch: false,
    }),
    {
      ownedMovie: false,
      showSubscribeButton: true,
      showSearchButton: false,
    },
  );
});

test("buildDetailPeopleList merges directorCredits or deduplicated directors with cast", () => {
  assert.deepEqual(
    buildDetailPeopleList({
      isMovie: true,
      directorCredits: [{ name: "Christopher Nolan", role: "Director", tmdbPersonId: 525 }],
      directors: ["Ignored Fallback"],
      cast: [{ name: "Cillian Murphy", role: "Oppenheimer", tmdbPersonId: 2037 }],
    }),
    [
      { name: "Christopher Nolan", role: "Director", tmdbPersonId: 525, credit: "导演" },
      { name: "Cillian Murphy", role: "Oppenheimer", tmdbPersonId: 2037 },
    ],
  );

  assert.deepEqual(
    buildDetailPeopleList({
      isMovie: false,
      directorCredits: [],
      directors: ["Vince Gilligan", "Vince Gilligan", "Peter Gould"],
      cast: [{ name: "Bob Odenkirk" }],
    }),
    [
      { name: "Vince Gilligan", credit: "主创" },
      { name: "Peter Gould", credit: "主创" },
      { name: "Bob Odenkirk" },
    ],
  );
});

test("computeHeroRecedeProgress clamps to [0, 1] and applies min 320px / 75% range", () => {
  assert.equal(MOBILE_HERO_HEIGHT, "min(115vw, 62svh)");
  assert.equal(computeHeroRecedeProgress(-20, 1000), "0.000");
  assert.equal(computeHeroRecedeProgress(0, 1000), "0.000");
  // clientHeight = 200 -> 200 * 0.75 = 150 -> floored to 320px range
  assert.equal(computeHeroRecedeProgress(160, 200), "0.500");
  // clientHeight = 1000 -> 1000 * 0.75 = 750px range
  assert.equal(computeHeroRecedeProgress(375, 1000), "0.500");
  assert.equal(computeHeroRecedeProgress(900, 1000), "1.000");
});

test("resolveImmersiveBackdropUrl and resolveMobileHeroSrc follow douban and HD decode policy", () => {
  assert.equal(
    resolveImmersiveBackdropUrl({
      source: "douban",
      hdState: "ok",
      hdUrl: "https://example.com/hd.jpg",
      fallbackBackdrop: "https://example.com/poster.jpg",
    }),
    "",
  );
  assert.equal(
    resolveMobileHeroSrc({
      hdState: "failed",
      hdUrl: undefined,
      fallbackBackdrop: "https://example.com/poster.jpg",
    }),
    "https://example.com/poster.jpg",
  );

  // TMDB stays black while pending, uses hdUrl on ok, falls back on failed
  assert.equal(
    resolveImmersiveBackdropUrl({
      source: "tmdb",
      hdState: "pending",
      hdUrl: "https://example.com/original.jpg",
      fallbackBackdrop: "https://example.com/w1280.jpg",
    }),
    "",
  );
  assert.equal(
    resolveImmersiveBackdropUrl({
      source: "tmdb",
      hdState: "ok",
      hdUrl: "https://example.com/original.jpg",
      fallbackBackdrop: "https://example.com/w1280.jpg",
    }),
    "https://example.com/original.jpg",
  );
  assert.equal(
    resolveImmersiveBackdropUrl({
      source: "tmdb",
      hdState: "failed",
      hdUrl: "https://example.com/original.jpg",
      fallbackBackdrop: "https://example.com/w1280.jpg",
    }),
    "https://example.com/w1280.jpg",
  );
  assert.equal(
    resolveMobileHeroSrc({
      hdState: "ok",
      hdUrl: "https://example.com/original.jpg",
      fallbackBackdrop: "https://example.com/w1280.jpg",
    }),
    "https://example.com/original.jpg",
  );
});
