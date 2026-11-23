/* global EventTarget, Event */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { getEventListeners } from "node:events";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";
import * as artworkLoading from "../lib/artwork-loading.ts";
import { createArtworkImage, manageArtworkImage } from "../lib/artwork-loading.ts";
import * as freezeFrame from "../lib/player/freeze-frame.ts";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

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

test("settled preloads release their cancellation listeners on a shared signal", (t) => {
  const { load, abort } = setup(t);
  const loaded = load("ready");
  const failed = load("failed");
  loaded.image.finish();
  failed.load("https://cdn.invalid/failure.jpg");
  failed.image.finish("error");

  assert.equal(getEventListeners(abort.signal, "abort").length, 0);
  loaded.load("https://app.invalid/posters/replacement");
  assert.equal(getEventListeners(abort.signal, "abort").length, 1);
  abort.abort();
  assert.equal(loaded.image.src, "");
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

test("ArtworkImage with loading='eager' starts immediately and skips IntersectionObserver", async () => {
  let observeCalls = 0;
  const source = readFileSync(new URL("../components/artwork-image.tsx", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "@/lib/artwork-loading") {
        return {
          manageArtworkImage: artworkLoading.manageArtworkImage,
          observeArtworkImage: (...args) => {
            observeCalls += 1;
            return artworkLoading.observeArtworkImage(...args);
          },
        };
      }
      return require(name);
    },
    module,
    module.exports,
  );
  const { ArtworkImage } = module.exports;
  const img = new ImageFixture();
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(ArtworkImage, { src: "https://app.invalid/posters/hero.jpg", loading: "eager" }),
      { createNodeMock: () => img },
    );
  });
  try {
    assert.equal(img.src, "https://app.invalid/posters/hero.jpg");
    assert.equal(observeCalls, 0, "eager ArtworkImage must not attach offscreen IntersectionObserver");
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});

test("ArtworkImage hides native broken-icon/alt flash while queued or loading and reveals on load", async () => {
  const source = readFileSync(new URL("../components/artwork-image.tsx", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)(
    (name) => (name === "@/lib/artwork-loading" ? artworkLoading : require(name)),
    module,
    module.exports,
  );
  const { ArtworkImage } = module.exports;
  const img = new ImageFixture();
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(ArtworkImage, {
        src: "https://app.invalid/posters/slow.jpg",
        alt: "天堂旗帜下",
        loading: "eager",
      }),
      { createNodeMock: () => img },
    );
  });
  try {
    // Before load finishes: must have opacity 0 so browser never flashes broken icon + alt text!
    assert.equal(renderer.toJSON().props.style?.opacity, 0);

    // Finish loading -> opacity 0 is removed!
    await act(async () => {
      img.finish("load");
    });
    assert.notEqual(renderer.toJSON().props.style?.opacity, 0);

    // Switch to another URL that fails -> hides broken state instead of showing broken icon!
    await act(async () => {
      renderer.update(
        React.createElement(ArtworkImage, {
          src: "https://app.invalid/posters/missing.jpg",
          alt: "天堂旗帜下",
          loading: "eager",
        }),
      );
    });
    await act(async () => {
      img.finish("error");
    });
    assert.equal(renderer.toJSON().props.style?.opacity, 0);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});

test("synchronous load fired inside set src during enqueue neither swallows load nor leaks a queue slot", (t) => {
  const { load, requests } = setup(t);

  class SyncCachedImageFixture extends ImageFixture {
    set src(value) {
      super.src = value;
      if (value) {
        this.finish("load");
      }
    }
    get src() {
      return super.src;
    }
  }

  const syncImg = new SyncCachedImageFixture();
  let settledCount = 0;
  const bitmapStates = [];
  const managed = manageArtworkImage(
    syncImg,
    0,
    () => {
      settledCount += 1;
    },
    (hasBitmap) => {
      bitmapStates.push(hasBitmap);
    },
  );
  t.after(() => managed.dispose());

  managed.load({ src: "https://app.invalid/posters/sync-cached.jpg" });
  assert.equal(settledCount, 1, "synchronous load inside enqueue must not be swallowed");
  assert.equal(bitmapStates.at(-1), true, "synchronous load must mark bitmap ready");

  // All 4 shared queue slots must remain available (0 slots leaked)!
  for (let i = 0; i < 4; i += 1) load(`after-sync-${i}`);
  assert.deepEqual(
    requests.map(({ image }) => image.src),
    [
      "https://app.invalid/posters/after-sync-0",
      "https://app.invalid/posters/after-sync-1",
      "https://app.invalid/posters/after-sync-2",
      "https://app.invalid/posters/after-sync-3",
    ],
    "synchronously settled artwork must release its queue slot immediately",
  );
});

test("calling load with an unchanged source while active or after completion is a no-op that consumes 0 queue slots", (t) => {
  const { load, requests } = setup(t);

  class CountingImageFixture extends ImageFixture {
    srcAssignments = 0;
    set src(value) {
      this.srcAssignments += 1;
      super.src = value;
    }
    get src() {
      return super.src;
    }
  }

  const image = new CountingImageFixture();
  const managed = manageArtworkImage(image, 0);
  t.after(() => managed.dispose());

  const targetSource = {
    src: "https://app.invalid/posters/unchanged.jpg",
    srcSet: "https://app.invalid/posters/unchanged-2x.jpg 2x",
    sizes: "200px",
  };

  managed.load(targetSource);
  assert.equal(image.srcAssignments, 1);

  // Calling load again with identical source while still in-flight must be a no-op
  managed.load({ ...targetSource });
  assert.equal(image.srcAssignments, 1, "re-render while active must not re-assign src");

  image.finish("load");

  // Fill all 4 queue slots with other preloads
  for (let i = 0; i < 4; i += 1) load(`busy-${i}`);
  assert.equal(requests.filter(({ image: img }) => img.src).length, 4);

  // Calling load again with identical source after completion must neither clear/reassign src nor enqueue
  managed.load({ ...targetSource });
  assert.equal(image.src, "https://app.invalid/posters/unchanged.jpg");
  assert.equal(image.srcAssignments, 1, "completed artwork must not re-assign unchanged src");

  // Verify 0 queue slots were queued behind the 4 busy slots
  load("fifth");
  requests[0].image.finish("load");
  assert.equal(
    requests[4].image.src,
    "https://app.invalid/posters/fifth",
    "unchanged load() must consume 0 queue slots",
  );
});

test("DetailBackdropSlideshow passes loading='eager' to its ArtworkImage layers", () => {
  const slideshowSource = readFileSync(
    new URL("../components/detail-backdrop-slideshow.tsx", import.meta.url),
    "utf8",
  );
  const matches = [...slideshowSource.matchAll(/<ArtworkImage\b[\s\S]*?\/>/g)];
  assert.equal(matches.length, 2, "DetailBackdropSlideshow should render bottom and top ArtworkImage");
  for (const [snippet] of matches) {
    assert.match(snippet, /loading="eager"/, `Expected loading="eager" on ArtworkImage:\n${snippet}`);
  }
});

test("ArtworkImage keeps stable attachment across inline callback refs and disconnects observer on lazy -> eager", async () => {
  let observeStops = 0;
  let manageCalls = 0;
  const activeSubscriptions = new Set();

  const source = readFileSync(new URL("../components/artwork-image.tsx", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "@/lib/artwork-loading") {
        return {
          manageArtworkImage: (...args) => {
            manageCalls += 1;
            return artworkLoading.manageArtworkImage(...args);
          },
          observeArtworkImage: (_image, change) => {
            const sub = { stopped: false, change };
            activeSubscriptions.add(sub);
            return () => {
              if (!sub.stopped) {
                sub.stopped = true;
                observeStops += 1;
                activeSubscriptions.delete(sub);
              }
            };
          },
        };
      }
      return require(name);
    },
    mod,
    mod.exports,
  );
  const { ArtworkImage } = mod.exports;

  const img = new ImageFixture();
  const seenRefs = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(ArtworkImage, {
        src: "https://app.invalid/posters/hero-lazy.jpg",
        loading: "lazy",
        ref: (el) => {
          seenRefs.push(el);
        },
      }),
      { createNodeMock: () => img },
    );
  });

  try {
    assert.equal(manageCalls, 1);
    assert.equal(activeSubscriptions.size, 1);
    assert.equal(img.src, "", "lazy offscreen ArtworkImage waits for observer or eager upgrade");

    // Parent re-renders with a brand-new inline callback ref function
    await act(async () => {
      renderer.update(
        React.createElement(ArtworkImage, {
          src: "https://app.invalid/posters/hero-lazy.jpg",
          loading: "lazy",
          ref: (el) => {
            seenRefs.push(el);
          },
        }),
      );
    });
    assert.equal(
      manageCalls,
      1,
      "Inline callback ref must not recreate manageArtworkImage on parent re-renders",
    );

    // Upgrade loading="lazy" -> loading="eager"
    await act(async () => {
      renderer.update(
        React.createElement(ArtworkImage, {
          src: "https://app.invalid/posters/hero-lazy.jpg",
          loading: "eager",
          ref: (el) => {
            seenRefs.push(el);
          },
        }),
      );
    });
    assert.equal(img.src, "https://app.invalid/posters/hero-lazy.jpg");
    assert.equal(observeStops, 1, "Switching loading to eager must immediately disconnect active observer subscription");
    assert.equal(activeSubscriptions.size, 0);

    // Any still-active observer callback (none should remain!) firing offscreen must NOT abort the eager preload!
    for (const sub of activeSubscriptions) {
      sub.change(false, 1);
    }
    assert.equal(
      img.src,
      "https://app.invalid/posters/hero-lazy.jpg",
      "Late offscreen observer callback must never clear an eager ArtworkImage src",
    );
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("PosterImage resets broken fallback state automatically when src updates after metadata refresh", async () => {
  const artworkSource = readFileSync(new URL("../components/artwork-image.tsx", import.meta.url), "utf8");
  const posterSource = readFileSync(new URL("../components/poster-image.tsx", import.meta.url), "utf8");
  const compile = (code) =>
    ts.transpileModule(code, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;

  const artMod = { exports: {} };
  new Function("require", "module", "exports", compile(artworkSource))(
    (name) => (name === "@/lib/artwork-loading" ? artworkLoading : require(name)),
    artMod,
    artMod.exports,
  );
  const posterMod = { exports: {} };
  new Function("require", "module", "exports", compile(posterSource))(
    (name) => (name === "@/components/artwork-image" ? artMod.exports : require(name)),
    posterMod,
    posterMod.exports,
  );
  const { PosterImage } = posterMod.exports;

  const img = new ImageFixture();
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(PosterImage, {
        src: "https://app.invalid/posters/broken-v1.jpg",
        alt: "海报",
        preload: true,
        fallback: React.createElement("div", { "data-testid": "poster-fallback" }, "FALLBACK"),
      }),
      { createNodeMock: (el) => (el.type === "img" ? img : null) },
    );
  });

  try {
    // Exhaust retries or trigger error handler on PosterImage's <img>
    const renderedImg = renderer.root.findByType("img");
    await act(async () => {
      renderedImg.props.onError();
    });
    assert.equal(renderer.root.findAllByType("img").length, 0, "broken src must show fallback");

    // Metadata refresh supplies a new valid poster URL -> must immediately render <img> again!
    await act(async () => {
      renderer.update(
        React.createElement(PosterImage, {
          src: "https://app.invalid/posters/refreshed-v2.jpg",
          alt: "海报",
          preload: true,
          fallback: React.createElement("div", { "data-testid": "poster-fallback" }, "FALLBACK"),
        }),
      );
    });
    assert.equal(
      renderer.root.findAllByType("img").length,
      1,
      "Updating src on PosterImage must reset broken state and mount ArtworkImage for the new URL",
    );
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("Above-the-fold brand logos and detail/library hero ArtworkImage elements pass loading='eager'", () => {
  for (const rel of [
    "../components/brand-loader.tsx",
    "../components/sidebar.tsx",
    "../components/app-shell.tsx",
    "../components/library-item-detail-view.tsx",
    "../components/media-detail/media-detail-scaffold.tsx",
    "../components/netflix/library-hero.tsx",
  ]) {
    const src = readFileSync(new URL(rel, import.meta.url), "utf8");
    const matches = [...src.matchAll(/<ArtworkImage\b[\s\S]*?\/>/g)];
    assert.ok(matches.length > 0, `Expected at least one <ArtworkImage /> in ${rel}`);
    for (const [snippet] of matches) {
      assert.match(snippet, /loading="eager"/, `Expected loading="eager" in ${rel}:\n${snippet}`);
    }
  }
});
