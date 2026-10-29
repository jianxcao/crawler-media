/* global EventTarget, Event */
import assert from "node:assert/strict";
import test from "node:test";
import { artworkRetry } from "../lib/artwork-retry.ts";
import { createArtworkImage, manageArtworkImage } from "../lib/artwork-loading.ts";

class Artwork extends EventTarget {
  constructor(src) { super(); this.src = src; }
  removeAttribute(name) { if (name === "src") this.src = ""; }
}

function error() { return new Event("error", { cancelable: true }); }

test("proxy cache miss retries without showing the broken-image fallback, then gives up within a bound", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/api/v1/images/proxy?url=https%3A%2F%2Fcatalog.invalid%2Fposter.jpg&variant=poster-card");
  const retry = artworkRetry(image, (source) => { image.src = source; });
  for (let i = 1; i <= 20; i++) {
    assert.equal(retry.onError(error()), true);
    t.mock.timers.tick(Math.min(5_000, 1_000 * 2 ** (i - 1)));
    const url = new URL(image.src);
    assert.equal(url.searchParams.get("_retry"), String(i));
    assert.equal(url.searchParams.get("variant"), "poster-card");
  }
  assert.equal(retry.onError(error()), false, "permanent upstream failure reaches the existing fallback");
  retry.dispose();
});

test("replacing a pending image does not overwrite its new source", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/images/proxy?url=first");
  const retry = artworkRetry(image, (source) => { image.src = source; });
  retry.onError(error());
  image.src = "https://app.invalid/images/proxy?url=second";
  t.mock.timers.tick(1_000);
  assert.equal(image.src, "https://app.invalid/images/proxy?url=second");
  retry.dispose();
});

test("unmount cancels retry timers and local asset failures pass through immediately", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/images/proxy?url=poster");
  const retry = artworkRetry(image, (source) => { image.src = source; });
  retry.onError(error());
  retry.dispose();
  t.mock.timers.tick(5_000);
  assert.equal(image.src, "https://app.invalid/images/proxy?url=poster");
  image.src = "https://app.invalid/images/assets/fixture";
  assert.equal(retry.onError(error()), false);
});

test("native preloads suppress only transient cache misses and preserve their existing error handler", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/images/proxy?url=poster");
  const managed = manageArtworkImage(image);
  managed.load({ src: "https://app.invalid/images/proxy?url=poster" });
  let failures = 0;
  image.addEventListener("error", () => { failures++; });
  for (let i = 0; i < 20; i++) { image.dispatchEvent(error()); t.mock.timers.tick(Math.min(5_000, 1_000 * 2 ** i)); }
  assert.equal(failures, 0);
  image.dispatchEvent(error());
  assert.equal(failures, 1);
  managed.dispose();
});


test("cold artwork remains retryable while a full background queue drains for 64 seconds", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/images/proxy?url=queued-poster");
  const retry = artworkRetry(image, (source) => { image.src = source; });
  let elapsed = 0;
  let attempt = 0;
  while (elapsed < 64_000) {
    assert.equal(retry.onError(error()), true, `image must remain pending at ${elapsed}ms`);
    const delay = Math.min(5_000, 1_000 * 2 ** attempt++);
    t.mock.timers.tick(delay);
    elapsed += delay;
  }
  assert.equal(retry.onError(error()), true, "queue delay must not trigger a permanent broken-image fallback");
  retry.onLoad();
  retry.dispose();
});

test("canceling a detached preload clears pending retries and prevents traffic after navigation", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  class Preload extends Artwork {
    removeAttribute(name) { if (name === "src") this.src = ""; }
  }
  const previous = globalThis.Image;
  globalThis.Image = function () { return new Preload(""); };
  t.after(() => { if (previous) globalThis.Image = previous; else delete globalThis.Image; });
  const controller = new AbortController();
  const { image, load } = createArtworkImage(controller.signal);
  load("https://app.invalid/images/proxy?url=abandoned-poster");
  image.dispatchEvent(error());
  controller.abort();
  t.mock.timers.tick(100_000);
  assert.equal(image.src.includes("_retry"), false, "abandoned preload must issue zero retries");
  assert.equal(image.src, "", "the pending source is released on navigation");
});


for (const path of ["/posters/file", "/fanart/file", "/stills/file", "/chapters/file/frame", "/libraries/fixture/cover"]) {
  test(`native artwork ${path} recovers transient failures with a short bounded retry window`, (t) => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const image = new Artwork(`https://app.invalid/api/v1${path}?version=fixture`);
    const retry = artworkRetry(image, (source) => { image.src = source; });
    for (let i = 1; i <= 4; i++) {
      assert.equal(retry.onError(error()), true);
      t.mock.timers.tick(Math.min(5_000, 1_000 * 2 ** (i - 1)));
      const next = new URL(image.src);
      assert.equal(next.searchParams.get("_retry"), String(i));
      assert.equal(next.searchParams.get("version"), "fixture");
    }
    assert.equal(retry.onError(error()), false, "missing native artwork eventually reaches its fallback");
    retry.dispose();
  });
}

test("external images and local assets retain immediate fallback", () => {
  for (const source of ["", "data:image/png;base64,fixture", "https://catalog.invalid/poster.jpg", "https://app.invalid/api/v1/images/assets/poster"]) {
    const image = new Artwork(source);
    const retry = artworkRetry(image, (next) => { image.src = next; });
    assert.equal(retry.onError(error()), false);
    retry.dispose();
  }
});

test("navigation cancels native artwork retries", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const source = "https://app.invalid/api/v1/libraries/fixture/cover";
  const image = new Artwork(source);
  const retry = artworkRetry(image, (source) => { image.src = source; });
  assert.equal(retry.onError(error()), true);
  retry.dispose();
  t.mock.timers.tick(100_000);
  assert.equal(image.src, source);
});

test("Media poster cache warmup retains the long queue window and preserves its query", (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const image = new Artwork("https://app.invalid/api/v1/media/fixture/poster?version=fixture");
  const retry = artworkRetry(image, (source) => { image.src = source; });
  let elapsed = 0;
  for (let i = 1; i <= 20; i++) {
    assert.equal(retry.onError(error()), true, `Media poster should remain pending at ${elapsed}ms`);
    const delay = Math.min(5_000, 1_000 * 2 ** (i - 1));
    t.mock.timers.tick(delay);
    elapsed += delay;
    const next = new URL(image.src);
    assert.equal(next.searchParams.get("_retry"), String(i));
    assert.equal(next.searchParams.get("version"), "fixture");
  }
  assert.equal(elapsed, 92_000);
  assert.equal(retry.onError(error()), false);
  retry.dispose();
});
