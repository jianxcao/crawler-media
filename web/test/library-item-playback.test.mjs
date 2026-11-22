import assert from "node:assert/strict";
import test from "node:test";
import { deferred, itemDetailHarness, watchState } from "./support/library-item-detail-renderer.mjs";

for (const tv of [false, true]) {
  test(`${tv ? "episode" : "movie"} can be marked watched after selecting a version`, async () => {
    const view = itemDetailHarness({ tv, resume: (_query, played) => watchState({ played }) });
    await view.mount();
    try {
      await view.source("file-b");
      await view.markPlayed();
      assert.deepEqual(view.errors, []);
      assert.equal(view.label(), "重新播放");
      assert.equal(view.selectedFile(), "file-b", "refreshing marks preserves the current episode's explicit version");
      assert.deepEqual(view.requests.find((request) => request.body)?.body, {
        media_item_id: "media", ...(tv ? { season_number: 1, episode_number: 1 } : {}), played: true,
      });
    } finally { await view.unmount(); }
  });
}

test("switching versions refreshes the resume preview for the explicit file", async () => {
  const pending = deferred();
  const view = itemDetailHarness({ resume: (query) => query.file_id === "file-b" ? pending.promise : watchState() });
  await view.mount();
  try {
    assert.match(view.label(), /^继续观看/);
    await view.source("file-b");
    assert.deepEqual(view.requests.filter((request) => request.path === "/playback/resume").at(-1).query, {
      media_item_id: "media", file_id: "file-b",
    });
    assert.equal(view.label(), "播放", "old resume is cleared while the new source is resolving");
    await view.resolve(pending, watchState({ file_id: "file-b", position_ms: 120_000, start_ms: 120_000 }));
    assert.match(view.label(), /看到 2:00/);
    await view.play();
    assert.equal(view.navigation.at(-1), "/play/media?file_id=file-b");
  } finally { await view.unmount(); }
});

test("an incompatible resume source displays the server's start position", async () => {
  const view = itemDetailHarness({ resume: () => watchState({ start_ms: 0, resume_status: "source_changed" }) });
  await view.mount();
  try { assert.equal(view.label(), "播放"); }
  finally { await view.unmount(); }
});

for (const sharedFile of [false, true]) {
  test(`changing episodes clears a ${sharedFile ? "shared" : "different"} file preference from resume and play`, async () => {
    const view = itemDetailHarness({ tv: true, sharedFile });
    await view.mount();
    try {
      await view.source("file-b");
      await view.episode(2);
      assert.deepEqual(view.requests.filter((request) => request.path === "/playback/resume").at(-1).query, {
        media_item_id: "media", season_number: "1", episode_number: "2",
      });
      await view.play();
      assert.equal(view.navigation.at(-1), "/play/media/s01e02");
    } finally { await view.unmount(); }
  });
}

test("a previous source's late resume cannot replace the selected version's preview", async () => {
  const old = deferred();
  const view = itemDetailHarness({ resume: (query) => query.file_id === "file-b"
    ? watchState({ file_id: "file-b", position_ms: 0, start_ms: 0, resume_status: "no_progress" }) : old.promise });
  await view.mount();
  try {
    await view.source("file-b");
    await view.resolve(old, watchState());
    assert.equal(view.label(), "播放");
  } finally { await view.unmount(); }
});

test("a watched mark completed after changing episodes cannot replace the new episode's preview", async () => {
  const marking = deferred();
  const view = itemDetailHarness({ tv: true, mark: () => marking.promise,
    resume: (query, played) => watchState({
      played: query.episode_number === "1" && played,
      start_ms: query.episode_number === "2" ? 120_000 : 3_600_000,
      position_ms: query.episode_number === "2" ? 120_000 : 3_600_000,
    }),
  });
  await view.mount();
  try {
    await view.markPlayed();
    await view.episode(2);
    assert.match(view.label(), /看到 2:00/);
    await view.resolve(marking);
    assert.match(view.label(), /看到 2:00/);
  } finally { await view.unmount(); }
});

test("a late watched-mark resume cannot replace a newly selected episode's preview", async () => {
  const markedResume = deferred();
  const view = itemDetailHarness({ tv: true, resume: (query, played) => {
    if (query.episode_number === "2") return watchState({ file_id: "file-c", position_ms: 120_000, start_ms: 120_000 });
    return played ? markedResume.promise : watchState();
  } });
  await view.mount();
  try {
    await view.markPlayed();
    await view.episode(2);
    assert.match(view.label(), /看到 2:00/);
    await view.resolve(markedResume, watchState({ played: true, start_ms: 0 }));
    assert.match(view.label(), /看到 2:00/);
  } finally { await view.unmount(); }
});

test("clearing playback history resets watched state and triggers episode & resume reload", async () => {
  let cleared = false;
  const view = itemDetailHarness({
    tv: true,
    libraryName: "",
    resume: () => (cleared
      ? watchState({ position_ms: 0, start_ms: 0, played: false, resume_status: "no_progress" })
      : watchState({ position_ms: 120_000, start_ms: 120_000, played: false })),
    onClearHistory: () => { cleared = true; },
  });
  await view.mount();
  try {
    assert.equal(view.fallbackLabel(), "库存", "empty library name must fall back to 库存");
    assert.match(view.label(), /看到 2:00/);
    const fetchesBefore = view.episodeFetches();
    const resumeCallsBefore = view.requests.filter((r) => r.path === "/playback/resume").length;

    await view.clearHistory();

    assert.equal(view.requests.some((r) => r.path === "/playback/history"), true);
    assert.equal(view.episodeFetches(), fetchesBefore + 1, "clear history must refresh season episodes");
    assert.equal(
      view.requests.filter((r) => r.path === "/playback/resume").length,
      resumeCallsBefore + 1,
      "clear history must re-fetch resume state",
    );
    assert.equal(view.label(), "播放", "watched progress label must clear after history deletion");
  } finally {
    await view.unmount();
  }
});
