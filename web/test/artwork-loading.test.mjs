/* global EventTarget, Event */
import assert from "node:assert/strict";
import test from "node:test";
import { createArtworkImage, manageArtworkImage } from "../lib/artwork-loading.ts";
import * as freezeFrame from "../lib/player/freeze-frame.ts";

class ImageFixture extends EventTarget {
  source = "";
  attributes = new Map();
  complete = true;
  naturalWidth = 0;
  naturalHeight = 0;
  set src(value) { this.complete = false; this.source = new URL(value, "https://app.invalid").href; this.attributes.set("src", value); }
  get src() { return this.source; }
  setAttribute(name, value) { if (name === "src") this.src = value; else this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); if (name === "src") { this.source = ""; this.complete = true; this.naturalWidth = 0; this.naturalHeight = 0; } }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  finish(type = "load") {
    if (type === "load") { this.complete = true; this.naturalWidth = 100; this.naturalHeight = 100; }
    const event = new Event(type);
    this.dispatchEvent(event);
    if (!event.cancelBubble) this[`on${type}`]?.(event);
  }
}

function setup(t) {
  const previous = globalThis.Image;
  globalThis.Image = ImageFixture;
  const abort = new AbortController();
  t.after(() => { abort.abort(); if (previous) globalThis.Image = previous; else delete globalThis.Image; });
  const requests = [];
  function load(name) {
    const request = createArtworkImage(abort.signal);
    assert.equal(typeof request.load, "function", "preload source must enter shared admission rather than native src");
    request.load(`https://app.invalid/posters/${name}`);
    requests.push(request);
    return request;
  }
  return { load, requests, abort };
}

test("twenty artwork preloads reserve at most four connections and each completion admits one", (t) => {
  const { load, requests } = setup(t);
  for (let index = 0; index < 20; index++) load(index);
  assert.deepEqual(requests.filter(({ image }) => image.src).map(({ image }) => image.src), [
    "https://app.invalid/posters/0", "https://app.invalid/posters/1",
    "https://app.invalid/posters/2", "https://app.invalid/posters/3",
  ]);
  requests[0].image.finish();
  assert.equal(requests[4].image.src, "https://app.invalid/posters/4");
  assert.equal(requests[5].image.src, "");
});

test("aborted queued and active preloads never start or retry after navigation", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const { load, requests, abort } = setup(t);
  for (let index = 0; index < 8; index++) load(index);
  requests[0].image.finish("error");
  abort.abort();
  t.mock.timers.tick(100_000);
  assert.ok(requests.every(({ image }) => image.src === ""));
});

test("a retry waits for the same four slots instead of bypassing queued work", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const { load, requests } = setup(t);
  for (let index = 0; index < 5; index++) load(index);
  requests[0].image.finish("error");
  assert.equal(requests[4].image.src, "https://app.invalid/posters/4");
  t.mock.timers.tick(1_000);
  assert.ok(!requests[0].image.src.includes("_retry"), "retry cannot steal a fifth connection");
  requests[1].image.finish();
  assert.equal(new URL(requests[0].image.src).searchParams.get("_retry"), "1");
});

test("changing a queued source discards its abandoned URL", (t) => {
  const { load, requests } = setup(t);
  for (let index = 0; index < 5; index++) load(index);
  requests[4].load("https://app.invalid/posters/replacement");
  requests[0].image.finish();
  assert.equal(requests[4].image.src, "https://app.invalid/posters/replacement");
});

test("viewport artwork shares preload slots, visible work beats background and offscreen cancels", async (t) => {
  const { load, requests } = setup(t);
  const loading = await import("../lib/artwork-loading.ts");
  assert.equal(typeof loading.observeArtworkImage, "function", "viewport observation must drive shared admission");
  const observers = [];
  const previous = globalThis.IntersectionObserver;
  globalThis.IntersectionObserver = class {
    constructor(callback, options) { this.callback = callback; this.options = options; observers.push(this); }
    observe() {}
    unobserve() {}
  };
  t.after(() => { if (previous) globalThis.IntersectionObserver = previous; else delete globalThis.IntersectionObserver; });
  for (let index = 0; index < 5; index++) load(index);
  const image = new ImageFixture();
  const managed = loading.manageArtworkImage(image);
  managed.setVisible(false);
  managed.load({ src: "https://app.invalid/posters/visible", srcSet: "https://app.invalid/posters/visible-2x 2x" });
  const stop = loading.observeArtworkImage(image, (near, priority) => managed.setVisible(near, priority));
  t.after(() => { stop(); managed.dispose(); });
  assert.equal(image.src, "");
  observers.find(({ options }) => options.rootMargin === "0px").callback([{ target: image, isIntersecting: true }]);
  requests[0].image.finish();
  assert.equal(image.src, "https://app.invalid/posters/visible", "visible image wins queued background preload");
  assert.equal(requests[4].image.src, "");
  observers.find(({ options }) => options.rootMargin === "0px").callback([{ target: image, isIntersecting: false }]);
  assert.equal(image.src, "");
  assert.equal(image.getAttribute("srcset"), null, "responsive source cannot bypass cancellation");
  assert.equal(requests[4].image.src, "https://app.invalid/posters/4");
  image.finish("error");
  assert.equal(image.src, "", "cancelled request cannot resurrect itself");
  let staleLoads = 0;
  image.addEventListener("load", () => { staleLoads++; });
  image.finish();
  assert.equal(staleLoads, 0, "abandoned completion cannot reveal a fallback as loaded");
});

test("completed artwork stays usable for canvas caches when its owner is disposed", (t) => {
  const { load, abort } = setup(t);
  const request = load("cached");
  request.image.finish();
  abort.abort();
  assert.equal(request.image.src, "https://app.invalid/posters/cached");
  assert.equal(request.image.naturalWidth, 100);
});


test("a queued empty trickplay sheet paints its target after eventual admission and load", (t) => {
  const { load, requests } = setup(t);
  for (let index = 0; index < 5; index++) load(index);
  const sheet = requests[4].image;
  assert.equal(sheet.complete, true, "native empty image is complete while queue has no src");
  assert.equal(sheet.naturalWidth, 0);
  const draws = [];
  const canvas = { getContext: () => ({ drawImage: (...args) => draws.push(args.slice(1)) }) };
  assert.equal(typeof freezeFrame.whenTileReady, "function");
  freezeFrame.whenTileReady(sheet, () => freezeFrame.drawTile(canvas, sheet, {
    width: 160, height: 90, offsetX: -320, offsetY: -90,
  }));
  assert.deepEqual(draws, []);
  requests[0].image.finish();
  assert.equal(sheet.src, "https://app.invalid/posters/4");
  sheet.finish();
  assert.deepEqual(draws, [[320, 90, 160, 90, 0, 0, 160, 90]], "the requested tile must actually reach the canvas");
});

for (const [name, source] of [
  ["no source", {}], ["undefined src", { src: undefined }],
  ["empty src", { src: "" }], ["empty srcset", { srcSet: "" }],
]) {
  test(`clearing a completed artwork removes its old image with ${name}`, (t) => {
    setup(t);
    const image = new ImageFixture();
    const managed = manageArtworkImage(image);
    t.after(() => managed.dispose());
    managed.load({ src: "https://app.invalid/posters/old", srcSet: "https://app.invalid/posters/old-2x 2x" });
    image.finish();
    managed.load(source);
    assert.equal(image.src, "");
    assert.equal(image.getAttribute("srcset"), null);
  });
}
