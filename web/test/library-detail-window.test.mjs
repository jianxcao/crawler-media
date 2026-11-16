import assert from "node:assert/strict";
import test from "node:test";
import { deferred, libraryDetailHarness, page } from "./support/library-detail-renderer.mjs";

for (const gallery of [false, true]) {
  const mode = gallery ? "gallery" : "poster wall";
  test(`${mode} returns from details to the same letter window and can append`, async () => {
    const view = libraryDetailHarness({
      gallery,
      load: async (_mode, { cursor }) => {
        if (cursor === "title-M") return page("M movie", "title-M-next");
        if (cursor === "title-M-next") return page("N movie");
        return page("A movie", "title-A-next");
      },
    });
    await view.mount();
    await view.click("recall");
    assert.equal(view.rows(), "M movie");
    assert.equal(view.start(), 500);
    await view.unmount();
    await view.mount();
    try {
      assert.equal(view.rows(), "M movie", "refresh must retain the jumped window");
      assert.equal(view.start(), 500);
      await view.click("more");
      assert.equal(view.rows(), "M movie,N movie");
      assert.equal(view.hasMore(), false);
    } finally {
      await view.unmount();
    }
  });

  test(`${mode} ignores an old refresh after sorting before appending`, async () => {
    const old = deferred();
    let hold = false;
    const view = libraryDetailHarness({
      gallery,
      load: async (requestedMode, { sort, cursor }) => {
        if (requestedMode === (gallery ? "gallery" : "wall") && hold && sort === "title") return old.promise;
        if (sort === "added_at") {
          if (cursor && cursor !== "added-next") throw new Error("cursor scope mismatch");
          return cursor ? page("Added second") : page("Added first", "added-next");
        }
        return page("Title movie", "title-next");
      },
    });
    await view.mount();
    hold = true;
    if (gallery) {
      await view.unmount();
      await view.mount();
    } else {
      await view.poll();
    }
    await view.sort("added_at");
    assert.equal(view.rows(), "Added first");
    await view.resolve(old, page("Stale title", "stale-title-next"));
    try {
      assert.equal(view.rows(), "Added first");
      await view.click("more");
      assert.equal(view.rows(), "Added first,Added second");
    } finally {
      await view.unmount();
    }
  });

  test(`${mode} ignores a letter jump completed after changing sort`, async () => {
    const jump = deferred();
    const view = libraryDetailHarness({
      gallery,
      load: async (_mode, { sort, cursor }) => {
        if (sort === "title" && cursor === "title-M") return jump.promise;
        if (sort === "added_at") {
          if (cursor && cursor !== "added-next") throw new Error("cursor scope mismatch");
          return cursor ? page("Added second") : page("Added first", "added-next");
        }
        return page("Title movie", "title-next");
      },
    });
    await view.mount();
    await view.click("recall");
    await view.sort("added_at");
    assert.equal(view.rows(), "Added first");
    await view.resolve(jump, page("Stale M movie", "title-M-next"));
    try {
      assert.equal(view.rows(), "Added first");
      assert.equal(view.start(), 0);
      await view.click("more");
      assert.equal(view.rows(), "Added first,Added second");
    } finally {
      await view.unmount();
    }
  });
}

test("gallery restored after sorting in poster mode uses the current sort", async () => {
  const view = libraryDetailHarness({
    gallery: true,
    load: async (_mode, { sort, cursor }) => {
      if (sort === "added_at") {
        if (cursor && cursor !== "added-next") throw new Error("cursor scope mismatch");
        return cursor ? page("Added second") : page("Added first", "added-next");
      }
      return cursor === "title-M" ? page("M movie", "title-M-next") : page("A movie", "title-A-next");
    },
  });
  await view.mount();
  await view.click("recall");
  assert.equal(view.rows(), "M movie");
  await view.gallery(false);
  await view.sort("added_at");
  await view.unmount();
  await view.mount();
  await view.gallery(true);
  try {
    assert.equal(view.rows(), "Added first");
    assert.equal(view.start(), 0);
    await view.click("more");
    assert.equal(view.rows(), "Added first,Added second");
  } finally {
    await view.unmount();
  }
});

test("background polling preserves a pending user letter jump", async () => {
  const jump = deferred();
  const view = libraryDetailHarness({
    load: async (_mode, { cursor }) => cursor === "title-M"
      ? jump.promise : page("A movie", "title-A-next"),
  });
  await view.mount();
  await view.click("recall");
  await view.poll();
  await view.resolve(jump, page("M movie", "title-M-next"));
  try {
    assert.equal(view.rows(), "M movie");
    assert.equal(view.start(), 500);
  } finally {
    await view.unmount();
  }
});
