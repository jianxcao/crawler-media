import test from "node:test";
import assert from "node:assert/strict";

import {
  createSubscriptionBody,
  libraryPatch,
  isLibraryDirty,
} from "../lib/subscription-form.ts";

test("createSubscriptionBody preserves string library_id", () => {
  const body = createSubscriptionBody({
    title_ref: "tmdb:movie:603",
    library_id: "library-B",
  });
  assert.equal(body.library_id, "library-B");
});

test("createSubscriptionBody omits library_id when null or undefined", () => {
  const bodyNull = createSubscriptionBody({
    title_ref: "tmdb:movie:603",
    library_id: null,
  });
  assert.equal(bodyNull.library_id, undefined);

  const bodyUndef = createSubscriptionBody({
    title_ref: "tmdb:movie:603",
  });
  assert.equal(bodyUndef.library_id, undefined);
});

test("libraryPatch emits null when clearing library to default", () => {
  assert.deepEqual(libraryPatch("library-B", null), { library_id: null });
});

test("libraryPatch emits empty object when library is unchanged", () => {
  assert.deepEqual(libraryPatch("library-B", "library-B"), {});
  assert.deepEqual(libraryPatch(null, null), {});
});

test("libraryPatch emits library_id when setting new library", () => {
  assert.deepEqual(libraryPatch(null, "library-B"), { library_id: "library-B" });
  assert.deepEqual(libraryPatch("library-A", "library-B"), { library_id: "library-B" });
});

test("isLibraryDirty correctly detects changes", () => {
  assert.equal(isLibraryDirty("library-B", null), true);
  assert.equal(isLibraryDirty("library-B", "library-B"), false);
  assert.equal(isLibraryDirty(null, null), false);
  assert.equal(isLibraryDirty(null, "library-B"), true);
});
