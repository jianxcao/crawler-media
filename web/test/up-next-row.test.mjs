import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

function loadUpNextRow(overrides = {}) {
  const cache = new Map();
  const rootUrl = new URL("../", import.meta.url);

  function resolveFile(baseUrl, specifier) {
    const raw = specifier.startsWith("@/")
      ? new URL(specifier.slice(2), rootUrl)
      : new URL(specifier, baseUrl);
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = new URL(`${raw.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve ${specifier} from ${baseUrl.href}`);
  }

  function load(specifier, fromUrl = rootUrl) {
    if (specifier in overrides) return overrides[specifier];
    const targetUrl = resolveFile(fromUrl, specifier);
    if (cache.has(targetUrl.href)) return cache.get(targetUrl.href).exports;
    const mod = { exports: {} };
    cache.set(targetUrl.href, mod);
    const source = readFileSync(targetUrl, "utf8");
    const js = ts.transpileModule(source, {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
        jsx: ts.JsxEmit.ReactJSX,
        esModuleInterop: true,
      },
    }).outputText;
    const passthrough = ({ children }) => React.createElement(React.Fragment, null, children);
    new Function("require", "module", "exports", js)(
      (dep) => {
        if (dep in overrides) return overrides[dep];
        if (dep === "react") return React;
        if (dep === "react/jsx-runtime") return require("react/jsx-runtime");
        if (dep.startsWith("./") || dep.startsWith("../") || dep.startsWith("@/")) {
          return load(dep, targetUrl);
        }
        try {
          return require(dep);
        } catch {
          return passthrough;
        }
      },
      mod,
      mod.exports,
    );
    return mod.exports;
  }

  return load("@/components/up-next-row");
}

test("UpNextRow renders card action trigger outside <a> and keeps it accessible on touch devices", () => {
  const passthrough = ({ children }) => React.createElement(React.Fragment, null, children);
  const { UpNextRow } = loadUpNextRow({
    "next/link": ({ href, to, children, ...rest }) =>
      React.createElement("a", { href: String(to ?? href ?? ""), ...rest }, children),
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: String(to), ...rest }, children),
    },
    "@/components/h-scroller": {
      HScroller: ({ children, className }) => React.createElement("div", { className }, children),
    },
    "@/components/watch-history-menu": {
      WatchHistoryMenu: () => React.createElement("div", { "data-testid": "history-menu" }),
    },
    "@/components/feedback": {
      useToast: () => ({ success: () => {}, error: () => {} }),
    },
    "@/components/poster-image": {
      PosterImage: ({ alt }) => React.createElement("img", { alt }),
    },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
    "@/lib/http": {
      request: async () => ({ data: {} }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
    "@/lib/api/playback": {
      clearPlaybackHistory: async () => ({ deleted_states: 0, deleted_metrics: 0 }),
    },
    "@radix-ui/react-dropdown-menu": {
      Root: passthrough,
      Trigger: passthrough,
      Portal: passthrough,
      Content: passthrough,
      Item: ({ children }) => React.createElement("div", { role: "menuitem" }, children),
    },
  });

  const html = renderToStaticMarkup(
    React.createElement(UpNextRow, {
      items: [
        {
          media_item_id: "media-1",
          library_id: "lib-1",
          kind: "tv",
          title: "测试剧集",
          year: 2025,
          season_number: 1,
          episode_number: 2,
          episode_title: "第二集",
          poster_url: null,
          backdrop_url: null,
          episode_still_url: null,
          poster_aspect: 0.667,
          position_ms: 120000,
          duration_ms: 2400000,
          progress_percent: 5,
          last_played_at: null,
          advanced: false,
          unwatched_ahead_count: 3,
        },
      ],
      libraries: [{ id: "lib-1", name: "剧集", kind: "tv" }],
      onCleared: () => {},
    }),
  );

  assert.match(html, /aria-label="操作《测试剧集》"/);
  assert.match(
    html,
    /\[@media\(hover:none\)\]:opacity-100/,
    "Card menu button must stay visible on touch devices without hover",
  );
  // Ensure the button is NOT inside an <a> element
  const anchors = [...html.matchAll(/<a\b[^>]*>([\s\S]*?)<\/a>/g)].map((m) => m[0]);
  assert.ok(anchors.length >= 1);
  for (const anchor of anchors) {
    assert.equal(
      anchor.includes('aria-label="操作《测试剧集》"'),
      false,
      "Dropdown trigger <button> must not be nested inside <a>",
    );
  }
});

test("UpNextRow guards remove-from-continue-watching against rapid duplicate clicks and resets after completion or error", async () => {
  const calls = [];
  const successToasts = [];
  const errorToasts = [];
  let clearedCount = 0;
  let pendingResolve = null;
  let pendingReject = null;

  const { UpNextRow } = loadUpNextRow({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: String(to), ...rest }, children),
    },
    "@/components/h-scroller": {
      HScroller: ({ children, className }) => React.createElement("div", { className }, children),
    },
    "@/components/watch-history-menu": {
      WatchHistoryMenu: () => React.createElement("div", { "data-testid": "history-menu" }),
    },
    "@/components/feedback": {
      useToast: () => ({
        success: (msg) => successToasts.push(msg),
        error: (msg) => errorToasts.push(msg),
      }),
    },
    "@/components/poster-image": {
      PosterImage: ({ alt }) => React.createElement("img", { alt }),
    },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
    "@/lib/http": {
      request: async () => ({ data: {} }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
    "@/lib/api/playback": {
      clearPlaybackHistory: (...args) => {
        calls.push(args);
        return new Promise((resolve, reject) => {
          pendingResolve = resolve;
          pendingReject = reject;
        });
      },
    },
    "@radix-ui/react-dropdown-menu": {
      Root: ({ children }) => React.createElement("div", null, children),
      Trigger: ({ children, disabled }) =>
        React.createElement("div", { "data-testid": "menu-trigger", "data-disabled": Boolean(disabled) }, children),
      Portal: ({ children }) => React.createElement("div", null, children),
      Content: ({ children }) => React.createElement("div", null, children),
      Item: ({ children, onSelect, disabled, className }) =>
        React.createElement(
          "button",
          {
            type: "button",
            "data-testid": "remove-item",
            disabled: Boolean(disabled),
            className,
            onSelect,
          },
          children,
        ),
    },
  });

  const sampleItem = {
    media_item_id: "media-1",
    library_id: "lib-1",
    kind: "tv",
    title: "测试剧集",
    year: 2025,
    season_number: 1,
    episode_number: 2,
    episode_title: "第二集",
    poster_url: null,
    backdrop_url: null,
    episode_still_url: null,
    poster_aspect: 0.667,
    position_ms: 120000,
    duration_ms: 2400000,
    progress_percent: 5,
    last_played_at: null,
    advanced: false,
    unwatched_ahead_count: 3,
  };

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(UpNextRow, {
        items: [sampleItem],
        libraries: [{ id: "lib-1", name: "剧集", kind: "tv" }],
        onCleared: () => {
          clearedCount += 1;
        },
      }),
    );
  });

  const getTriggerWrapper = () =>
    renderer.root.find((n) => n.props?.["data-testid"] === "menu-trigger");
  const getRemoveItem = () =>
    renderer.root.find((n) => n.props?.["data-testid"] === "remove-item");

  assert.equal(getTriggerWrapper().props["data-disabled"], false);
  assert.equal(getRemoveItem().props.disabled, false);

  // Rapid duplicate invocations within the same tick while clearPlaybackHistory is in flight
  let prevented = 0;
  const makeEvent = () => ({
    preventDefault() {
      prevented += 1;
    },
  });

  await act(async () => {
    const onSelect = getRemoveItem().props.onSelect;
    onSelect(makeEvent());
    onSelect(makeEvent());
  });

  assert.equal(calls.length, 1, "clearPlaybackHistory must only be called once while in flight");
  assert.deepEqual(calls[0], ["item", { mediaItemId: "media-1" }]);
  assert.equal(prevented, 1, "duplicate in-flight onSelect must call preventDefault()");
  assert.equal(getTriggerWrapper().props["data-disabled"], true);
  assert.equal(getRemoveItem().props.disabled, true);

  // Even a click after re-render while still in flight is blocked
  await act(async () => {
    getRemoveItem().props.onSelect(makeEvent());
  });
  assert.equal(calls.length, 1);
  assert.equal(prevented, 2);

  // Resolve the in-flight request -> state resets cleanly
  await act(async () => {
    pendingResolve({ deleted_states: 1, deleted_metrics: 0 });
    await Promise.resolve();
  });

  assert.deepEqual(successToasts, ["已移出《测试剧集》"]);
  assert.equal(clearedCount, 1);
  assert.equal(getTriggerWrapper().props["data-disabled"], false);
  assert.equal(getRemoveItem().props.disabled, false);

  // Error path: rapid duplicate invocations during failing request also reset cleanly
  await act(async () => {
    const onSelect = getRemoveItem().props.onSelect;
    onSelect(makeEvent());
    onSelect(makeEvent());
  });
  assert.equal(calls.length, 2, "must allow new call after previous completed, but block its duplicate");
  assert.equal(getRemoveItem().props.disabled, true);

  await act(async () => {
    pendingReject(new Error("network boom"));
    await Promise.resolve();
  });

  assert.deepEqual(errorToasts, ["network boom"]);
  assert.equal(getTriggerWrapper().props["data-disabled"], false);
  assert.equal(getRemoveItem().props.disabled, false);

  await act(async () => renderer.unmount());
});
