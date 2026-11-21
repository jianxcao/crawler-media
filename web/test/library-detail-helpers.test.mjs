import assert from "node:assert/strict";
import test from "node:test";
import {
  hasMorePages,
  keepOnError,
  readFilterFromUrl,
  sortOptions,
  writeFilterToUrl,
} from "../lib/library-detail-helpers.ts";

test("hasMorePages detects whether cursor chain ends in a non-null cursor", () => {
  assert.equal(hasMorePages([null]), false);
  assert.equal(hasMorePages([null, null]), false);
  assert.equal(hasMorePages([null, "c1"]), true);
  assert.equal(hasMorePages(["c0", "c1"]), true);
});

test("keepOnError resolves array on fulfillment and null on rejection", async () => {
  assert.deepEqual(await keepOnError(Promise.resolve([1, 2])), [1, 2]);
  assert.equal(await keepOnError(Promise.reject(new Error("boom"))), null);
});

test("sortOptions omits release_date for timeline libraries and uses defaultLabel", () => {
  const scraped = sortOptions("按标题", false);
  assert.deepEqual(scraped[0], ["default", "按标题"]);
  assert.ok(scraped.some(([k]) => k === "release_date"));

  const timeline = sortOptions("按时间", true);
  assert.deepEqual(timeline[0], ["default", "按时间"]);
  assert.ok(!timeline.some(([k]) => k === "release_date"));
});

test("readFilterFromUrl and writeFilterToUrl round-trip filters without dropping extra query params", () => {
  const prevWindow = globalThis.window;
  let currentUrl = "/library/lib1?pending=1";
  const fakeWindow = {
    location: {
      pathname: "/library/lib1",
      get search() {
        const idx = currentUrl.indexOf("?");
        return idx >= 0 ? currentUrl.slice(idx) : "";
      },
    },
    history: {
      replaceState(_state, _title, url) {
        currentUrl = url;
      },
    },
  };
  globalThis.window = fakeWindow;
  try {
    writeFilterToUrl({
      genres: ["动作", "科幻"],
      countries: ["JP"],
      decades: ["2020s"],
      watch: "unwatched",
      ratingGte: 8,
      runtimes: ["medium"],
      languages: ["ja"],
      resolutions: ["4k"],
      hdr: true,
      stock: ["strm"],
    });
    assert.ok(currentUrl.includes("pending=1"));
    const parsed = readFilterFromUrl();
    assert.deepEqual(parsed, {
      genres: ["动作", "科幻"],
      countries: ["JP"],
      decades: ["2020s"],
      watch: "unwatched",
      ratingGte: 8,
      runtimes: ["medium"],
      languages: ["ja"],
      resolutions: ["4k"],
      hdr: true,
      stock: ["strm"],
    });
  } finally {
    globalThis.window = prevWindow;
  }
});
