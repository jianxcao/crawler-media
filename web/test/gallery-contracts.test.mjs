import test from "node:test";
import assert from "node:assert/strict";

import { galleryDetailHref } from "../lib/gallery-detail-link.ts";

test("galleryDetailHref generates clean item link when season/episode are null or undefined", () => {
  assert.equal(
    galleryDetailHref(
      { library_id: "lib-A", media_item_id: "media-A" },
      { season: null, episode: null },
    ),
    "/library/lib-A/item/media-A",
  );
  assert.equal(
    galleryDetailHref(
      { library_id: "lib-A", media_item_id: "media-A" },
      { season: undefined, episode: undefined },
    ),
    "/library/lib-A/item/media-A",
  );
});

test("galleryDetailHref generates scoped item link only when both season and episode are numbers", () => {
  assert.equal(
    galleryDetailHref(
      { library_id: "lib-A", media_item_id: "media-A" },
      { season: 2, episode: 3 },
    ),
    "/library/lib-A/item/media-A?season=2&episode=3",
  );
  assert.equal(
    galleryDetailHref(
      { library_id: "lib-A", media_item_id: "media-A" },
      { season: 2, episode: null },
    ),
    "/library/lib-A/item/media-A",
  );
});
