import test from "node:test";
import assert from "node:assert/strict";

import { galleryMarkTarget } from "../lib/gallery-mark-target.ts";

test("galleryMarkTarget keeps UUID string intact without converting to NaN/null", () => {
  const id = "11111111-1111-1111-1111-111111111111";
  const target = galleryMarkTarget(id);
  assert.equal(typeof target.media_item_id, "string");
  assert.equal(target.media_item_id, id);
  assert.deepEqual(JSON.parse(JSON.stringify(target)), {
    media_item_id: id,
  });
});
