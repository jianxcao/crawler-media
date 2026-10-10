import assert from "node:assert/strict";
import test from "node:test";
import { createDraftSaver } from "../lib/draft-saver.ts";
import { createInitialUnitStart } from "../lib/player/initial-unit-start.ts";

function fixture() {
  let timer = null;
  const writes = [];
  const saver = createDraftSaver({
    save: async (value) => { writes.push(value); },
    delayMs: 400,
    schedule: (callback) => { timer = callback; return 1; },
    cancel: () => { timer = null; },
  });
  return { saver, writes, tick: () => timer?.() };
}

test("restore defaults discards pending draft before cleanup flush", async () => {
  const { saver, writes, tick } = fixture();
  saver.schedule(["old draft"]);
  await saver.reset([]);
  await saver.flush();
  tick();
  await saver.flush();
  assert.deepEqual(writes, [[]]);
});

test("leaving before debounce saves the latest draft exactly once", async () => {
  const { saver, writes, tick } = fixture();
  saver.schedule(["first"]);
  saver.schedule(["latest"]);
  await saver.flush();
  await saver.flush();
  tick();
  assert.deepEqual(writes, [["latest"]]);
});

test("defaults are saved after an already in-flight draft", async () => {
  let finish;
  const writes = [];
  const saver = createDraftSaver({
    save: async (value) => {
      if (value === "draft") await new Promise((resolve) => { finish = resolve; });
      writes.push(value);
    },
    delayMs: 400,
  });
  saver.schedule("draft");
  const draft = saver.flush();
  await Promise.resolve();
  const defaults = saver.reset("defaults");
  finish();
  await Promise.all([draft, defaults]);
  assert.deepEqual(writes, ["draft", "defaults"]);
});

test("initial timestamp survives StrictMode replay and never applies after switching unit", () => {
  const start = createInitialUnitStart("media/1/1");
  assert.equal(start("media/1/1", 120000), 120000);
  assert.equal(start("media/1/1", 120000), 120000);
  assert.equal(start("media/1/2", 120000), undefined);
  assert.equal(start("media/1/1", 120000), undefined);
});

test("initial unit can receive a changed timestamp without consuming it on effect calls", () => {
  const start = createInitialUnitStart("movie/0/0");
  assert.equal(start("movie/0/0", undefined), undefined);
  assert.equal(start("movie/0/0", 5000), 5000);
  assert.equal(start("movie/0/0", 5000), 5000);
});
