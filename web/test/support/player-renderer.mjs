/* global Event, EventTarget */
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const root = fileURLToPath(new URL("../../", import.meta.url));
const emptyRanges = { length: 0, start() {}, end() {} };
class Video extends EventTarget {
  paused = true; currentTime = 0; duration = 1000; readyState = 2;
  ended = false; seeking = false; volume = 1; muted = false; playbackRate = 1;
  buffered = emptyRanges; seekable = emptyRanges;
  textTracks = Object.assign([], new EventTarget());
  constructor() {
    super();
    this.textTracks.addEventListener = () => {};
    this.textTracks.removeEventListener = () => {};
  }
  async play() { this.paused = false; this.dispatchEvent(new Event("playing")); }
  pause() { this.paused = true; this.dispatchEvent(new Event("pause")); }
  requestVideoFrameCallback(callback) { queueMicrotask(callback); return 1; }
  cancelVideoFrameCallback() {}
  querySelectorAll() { return []; }
  getBoundingClientRect() { return { width: 640, height: 360, left: 0, top: 0 }; }
}

function session(sid, startMs) {
  return {
    play_session_id: sid, session_id: null, stream_url: `/stream/${sid}`, start_ms: startMs,
    decision: { outcome: "plan", tier: 0, file_id: "file-1", container: "mp4", video: { action: "copy" }, audio: { action: "copy", track_ref: null }, subtitles: [], audio_tracks: [] },
    subtitle_urls: [], watch: { position_ms: startMs, duration_ms: 1000_000 }, source: null,
  };
}

export async function mountPlayer({ grant, progress, scope, startMsOverride } = {}) {
  const target = new EventTarget();
  const timers = new Set();
  const intervals = new Set();
  const win = Object.assign(target, {
    setTimeout(fn, delay) { const id = setTimeout(fn, delay); timers.add(id); return id; },
    clearTimeout(id) { clearTimeout(id); timers.delete(id); },
    setInterval(fn, delay) { const id = setInterval(fn, delay); intervals.add(id); return id; },
    clearInterval(id) { clearInterval(id); intervals.delete(id); },
    matchMedia: () => Object.assign(new EventTarget(), { matches: false }),
    innerWidth: 1280, innerHeight: 720,
  });
  globalThis.window = win;
  globalThis.document = Object.assign(new EventTarget(), { visibilityState: "visible" });
  const storage = { getItem: () => null, setItem() {}, removeItem() {} };
  globalThis.localStorage = storage;
  const calls = [], reports = [], metrics = [], engines = [];
  const video = new Video();
  const cache = new Map();
  const load = filename => {
    if (cache.has(filename)) return cache.get(filename).exports;
    const output = ts.transpileModule(readFileSync(filename, "utf8"), { compilerOptions: {
      module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022,
    } }).outputText;
    const mod = { exports: {} };
    cache.set(filename, mod);
    new Function("require", "module", "exports", output)(name => {
      if (name === "media-chrome/react") return { MediaController: "media-controller" };
      if (name === "@/lib/api/playback") return {
        DEFAULT_PLAYBACK_SCOPE: { base: "/playback", progress: "server", telemetry: true },
        startPlaybackSession: async body => { calls.push(body); return grant ? grant(body, calls.length) : session(`sid-${calls.length}`, body.start_ms ?? 0); },
        resolveStreamUrl: url => url,
        reportPlaybackProgress: async body => { reports.push(body); return progress ? progress(body) : {}; },
        reportPlaybackProgressOnUnload: body => reports.push({ ...body, unload: true }),
        reportPlaybackMetric: async body => { metrics.push(body); },
        reportPlaybackMetricOnUnload: body => metrics.push(body),
        reportPlaybackClientLog() {},
      };
      if (name === "@/lib/player/capability") return { getCapabilitySnapshot: async () => ({ mse: "none", native_hls: false, is_mobile: false }) };
      if (name === "@/lib/player/engine") return {
        preloadHlsEngine() {}, createEngine(options) {
          const engine = { options, attach: async () => {}, destroy() {}, stats: () => ({ engine: "direct", downlinkBps: null }) };
          engines.push(engine); return engine;
        },
      };
      if (name.startsWith("@/components/")) return new Proxy({}, { get: (_, key) => key === "useVideoContentBox" ? () => ({ width: 0, height: 0, bottomInset: 0 }) : String(key) });
      if (name.startsWith("@/")) return load(path.join(root, `${name.slice(2)}.ts`));
      if (name.startsWith(".")) return load(path.resolve(path.dirname(filename), name.endsWith(".ts") ? name : `${name}.ts`));
      return require(name);
    }, mod, mod.exports);
    return mod.exports;
  };
  const { VideoPlayer } = load(path.join(root, "components/player/video-player.tsx"));
  let renderer;
  const props = { unit: { media_item_id: "media", file_id: "file-1" }, title: "Media", episodeLabel: null, posterUrl: null, next: null, prev: null, onPlayNext() {}, onPlayPrev() {}, onExit() {}, startMsOverride, ...(scope ? { api: scope } : {}) };
  await act(async () => { renderer = create(React.createElement(VideoPlayer, props), { createNodeMock: element => element.type === "video" ? video : null }); });
  return {
    calls, reports, metrics, engines, video, renderer,
    session,
    async event(type, persisted = true) {
      const event = new Event(type); Object.defineProperty(event, "persisted", { value: persisted });
      await act(async () => target.dispatchEvent(event));
    },
    async pause() { await act(async () => renderer.root.findByType("PlayerCenterControls").props.onTogglePlay()); },
    async navigate() { await act(async () => renderer.update(React.createElement(VideoPlayer, {
      ...props, unit: { media_item_id: "next-media" },
    }))); },
    async play() { await act(async () => video.play()); },
    async seek(ms) { await act(async () => { video.currentTime = ms / 1000; video.dispatchEvent(new Event("timeupdate")); }); },
    async flush(fn) { await act(async () => { await fn(); }); },
    async unmount() {
      await act(async () => renderer.unmount());
      for (const timer of timers) clearTimeout(timer);
      for (const timer of intervals) clearInterval(timer);
    },
  };
}
