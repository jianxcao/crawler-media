import assert from "node:assert/strict";
import test from "node:test";
import { deferred, item, libraryDetailHarness, page } from "./support/library-detail-renderer.mjs";

test("gallery records an absolute recall position after jumping into a later window", async () => {
  const view = libraryDetailHarness({
    gallery: true,
    realRecall: true,
    load: async (_mode, { cursor }) => {
      if (cursor === "title-M") return page("M movie", "title-M-next");
      if (cursor === "title-M-next") return page("N movie");
      return page("A movie", "title-A-next");
    },
  });
  await view.mount();
  try {
    await view.click("recall");
    await view.click("more");
    assert.equal(view.rows(), "M movie,N movie");
    // The recall hook ignores navigation's scroll events for the first 500 ms.
    await new Promise((resolve) => setTimeout(resolve, 510));
    await view.scrollToItem("N movie");
    assert.equal(view.savedRecall().offset, 501);
  } finally {
    await view.unmount();
  }
});

for (const gallery of [false, true]) {
  const mode = gallery ? "gallery" : "poster wall";
  for (const change of ["insert", "remove"]) {
    test(`${mode} refreshes contiguous pages after an early ${change}`, async () => {
      let ids = Array.from({ length: 450 }, (_, index) => String(index + 1).padStart(3, "0"));
      const view = libraryDetailHarness({
        gallery,
        load: async (_mode, { cursor, limit }) => {
          const start = cursor ? ids.findIndex((id) => id > cursor) : 0;
          const rows = start < 0 ? [] : ids.slice(start, start + limit);
          return {
            items: rows.map(item),
            groups: rows.map((id) => ({ ...item(id), images: [] })),
            next_cursor: start >= 0 && start + limit < ids.length ? rows.at(-1) : null,
            total: ids.length,
          };
        },
      });
      await view.mount();
      await view.click("more");
      const windowSize = gallery ? 120 : 400;
      assert.deepEqual(view.rows().split(","), ids.slice(0, windowSize));
      await view.unmount();
      ids = change === "insert" ? ["000", ...ids] : ids.slice(1);
      await view.mount();
      try {
        assert.deepEqual(view.rows().split(","), ids.slice(0, windowSize));
        await view.click("more");
        assert.deepEqual(view.rows().split(","), ids.slice(0, gallery ? 180 : 600));
      } finally {
        await view.unmount();
      }
    });
  }

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

test("cold default sort uses title for movies and release_date for videos without added_at", async () => {
  const movieSorts = [];
  const movieView = libraryDetailHarness({
    initialSortPref: null,
    libraryKind: "movie",
    load: async (mode, { sort }) => {
      if (mode === "wall") movieSorts.push(sort);
      return page("Alpha");
    },
  });
  await movieView.mount();
  try {
    assert.ok(movieSorts.length > 0);
    assert.ok(
      movieSorts.every((s) => s === "title"),
      `movie default sort must be title, saw ${JSON.stringify(movieSorts)}`,
    );
  } finally {
    await movieView.unmount();
  }

  const videoSorts = [];
  const videoView = libraryDetailHarness({
    initialSortPref: null,
    libraryKind: "video",
    load: async (mode, { sort }) => {
      if (mode === "wall") videoSorts.push(sort);
      return page("Clip");
    },
  });
  await videoView.mount();
  try {
    assert.equal(
      videoSorts.at(-1),
      "release_date",
      `video default sort must be release_date, saw ${JSON.stringify(videoSorts)}`,
    );
  } finally {
    await videoView.unmount();
  }
});

test("gallery passes gallery=true to item-index and aborts multi-page jump on sort change", async () => {
  const secondPage = deferred();
  let pageCount = 0;
  const view = libraryDetailHarness({
    gallery: true,
    load: async (mode, { sort, cursor }) => {
      if (mode === "gallery" && sort === "title" && cursor === "title-M") {
        pageCount += 1;
        if (pageCount === 1) return { groups: [item("M-1")], next_cursor: "title-M-2", total: 1000 };
        return secondPage.promise;
      }
      if (sort === "added_at") {
        return page("Added first", "added-next");
      }
      return page("A movie", "title-A-next");
    },
  });
  await view.mount();
  try {
    assert.ok(
      view.indexCalls().some((c) => c.options?.gallery === true),
      "gallery mode must request item-index with { gallery: true }",
    );
    await view.click("recall");
    await view.sort("added_at");
    assert.equal(view.rows(), "Added first");
    await view.resolve(secondPage, { groups: [item("M-2")], next_cursor: null, total: 1000 });
    assert.equal(view.rows(), "Added first");
  } finally {
    await view.unmount();
  }
});
