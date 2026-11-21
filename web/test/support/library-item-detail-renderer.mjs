import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";
import * as libraryFilePages from "../../lib/library-file-pages.ts";
import * as chapterFrames from "../../lib/chapter-frame-task.ts";
import * as itemProbe from "../../lib/library-item-probe.ts";
import * as playLinks from "../../lib/player/play-links.ts";
import * as activeTarget from "../../lib/player/active-target.ts";
import * as format from "../../lib/format.ts";
import * as timeline from "../../lib/player/timeline.ts";

const require = createRequire(import.meta.url);
const noop = () => undefined;
const leaf = () => null;
const children = ({ children }) => children;
const fallback = new Proxy({}, { get: (_, key) => key === "__esModule" ? false : leaf });
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

export function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

export const watchState = (overrides = {}) => ({
  position_ms: 3_600_000, start_ms: 3_600_000, duration_ms: 7_200_000,
  played: false, play_count: 0, audio_track: null, subtitle_track: null,
  file_id: "file-a", resume_status: "compatible", ...overrides,
});

const file = (id, episodes) => ({
  file_id: id, path: `/media/${id}.mkv`, presence: "present", publication: "published",
  units: episodes.map((episode) => ({ season: 1, episode })),
  season: episodes.length ? 1 : null, episode: episodes[0] ?? null,
  resolution: null, codec: null, hdr: null, quality_source: null,
  audio_tracks: [], subtitle_tracks: [], video_track: null, probed: true, probe_queued: false,
});

// Real detail/episode/PlayAction components and playback API serialization.
// HTTP and browser providers are external seams; the track selector replaces
// only its visual presentation, preserving the component's controlled contract.
export function itemDetailHarness({
  tv = false,
  sharedFile = false,
  canManageLibraries = false,
  resume = () => watchState(),
  mark,
} = {}) {
  const requests = [];
  const probeStatusCalls = [];
  const deletedLedgerRows = [];
  const navigation = [];
  const errors = [];
  const files = [file("file-a", tv ? [1] : []), file("file-b", tv ? (sharedFile ? [1, 2] : [1]) : [])];
  if (tv) files.push(file("file-c", [2]));
  const detail = {
    media_item_id: "media", kind: tv ? "tv" : "movie", title: "Media", year: null,
    original_title: null, tmdb_id: null, poster_url: null, backdrop_url: null,
    overview: null, rating: null, runtime_minutes: null, genres: [], cast: [],
    seasons: tv ? [1] : [], files,
  };
  const episodes = [1, 2].map((number) => ({
    episode_number: number, name: `Episode ${number}`, overview: null, air_date: null, still_url: null,
    owned: true, file_ids: files.filter((file) => file.units.some((unit) => unit.episode === number)).map((file) => file.file_id),
    position_ms: 0, played: false, progress_percent: null,
  }));
  const feedback = { info: noop, success: noop, error: (message) => errors.push(message) };
  const favorites = new Map();
  let played = false;
  const request = async (path, options = {}) => {
    const url = new URL(path, "http://localhost");
    const query = Object.fromEntries(url.searchParams);
    const body = options.body ? JSON.parse(options.body) : undefined;
    requests.push({ path: url.pathname, query, body });
    if (url.pathname === "/playback/resume") return { data: await resume(query, played) };
    if (url.pathname === "/playback/marks") {
      if (options.method && options.method !== "GET" && body) {
        if (Object.keys(body).some((key) => !["media_item_id", "season_number", "episode_number", "played", "favorite"].includes(key))) {
          throw new Error("playback.invalid: unknown mark field");
        }
        if (mark) await mark(body);
        if (body.played != null) played = body.played;
        if (body.favorite != null) favorites.set(body.media_item_id, body.favorite);
      }
      const targetId = query.media_item_id ?? body?.media_item_id ?? "media";
      return { data: { played, is_favorite: favorites.get(targetId) ?? false, unplayed_count: null } };
    }
    throw new Error(`Unexpected request ${path}`);
  };
  globalThis.window = {
    location: { pathname: "/library/library/item/media", search: "" },
    sessionStorage: { setItem: noop },
  };
  const mocks = {
    react: React, "react/jsx-runtime": require("react/jsx-runtime"),
    "react-router-dom": { Link: "a", useNavigate: () => (href) => navigation.push(href) },
    "@/lib/http": { request },
    "@/lib/library-file-pages": libraryFilePages,
    "@/lib/chapter-frame-task": chapterFrames,
    "@/lib/library-item-probe": itemProbe,
    "@/lib/player/play-links": playLinks,
    "@/lib/player/active-target": activeTarget,
    "@/lib/format": format, "@/lib/player/timeline": timeline,
    "@/lib/api/libraries": {
      getLibraryItemDetail: async (_lib, id) => ({ ...detail, media_item_id: id ?? detail.media_item_id }),
      getLibrary: async () => ({ id: "library", name: "Library", kind: detail.kind }),
      getItemEpisodes: async () => ({ season_number: 1, episodes }),
      fetchItemChapters: async () => [],
      fetchChapterFrameStatus: async () => ({ task: null, empty: true }),
      fetchItemProbeStatus: async (libraryId, mediaItemId, season) => {
        probeStatusCalls.push({ libraryId, mediaItemId, season });
        return { active: false, job: null };
      },
    },
    "@/lib/api/transfers": {
      deleteLedgerRow: async (id, deleteFile = false) => {
        deletedLedgerRows.push({ id, deleteFile });
        return { deleted: true, path: `/media/${id}.mkv`, file_deleted: deleteFile };
      },
    },
    "@/components/modal": {
      Modal: ({ open, children: content }) =>
        open ? React.createElement("div", { "data-modal": "true" }, content) : null,
    },
    "@/components/feedback": { useConfirm: () => noop, useToast: () => feedback },
    "@/components/subscribe-entry": { useSubscribeEntry: () => ({ canSubscribe: false }) },
    "@/lib/permissions": { usePermissions: () => ({ canManageLibraries, isAdmin: canManageLibraries }) },
    "@/lib/backdrop": { useBackdrop: () => ({ setOverrideBackdrop: noop }) },
    "@/lib/ui-prefs": { useTheme: () => ({ id: "default" }) },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/components/tooltip": { Tooltip: children },
    "@/components/h-scroller": { HScroller: children },
    "@/components/media-track-rows": {
      MediaTrackRows: ({ selectedFileId, onSelectedFileIdChange }) => React.createElement("select", {
        "aria-label": "播放版本", value: selectedFileId, onChange: (event) => onSelectedFileIdChange(event.target.value),
      }),
    },
  };
  const modules = new Map();
  const real = new Set(["@/lib/library-item-format"]);
  function resolveFile(baseUrl, specifier) {
    const raw = specifier.startsWith("@/")
      ? new URL(`../../${specifier.slice(2)}`, import.meta.url)
      : new URL(specifier, baseUrl);
    for (const suffix of ["", ".tsx", ".ts", "/index.tsx", "/index.ts"]) {
      const candidate = new URL(`${raw.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module ${specifier} from ${baseUrl.href}`);
  }
  function compile(specifier, fromUrl = import.meta.url) {
    const baseUrl = typeof fromUrl === "string" ? new URL(fromUrl) : fromUrl;
    const targetUrl = resolveFile(baseUrl, specifier);
    if (modules.has(targetUrl.href)) return modules.get(targetUrl.href).exports;
    const source = readFileSync(targetUrl, "utf8");
    const js = ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText;
    const module = { exports: {} };
    modules.set(targetUrl.href, module);
    new Function("require", "module", "exports", js)((name) => {
      if (mocks[name]) return mocks[name];
      if (name.startsWith("./") || name.startsWith("../") || real.has(name)) {
        return compile(name, targetUrl);
      }
      return fallback;
    }, module, module.exports);
    return module.exports;
  }
  const api = compile("../../lib/api/playback.ts");
  mocks["@/lib/api/playback"] = api;
  const { LibraryItemDetailView } = compile("../../components/library-item-detail-view.tsx");
  let renderer;
  const playButton = () => renderer.root.findAllByType("button").find((button) => /^(播放|继续观看|重新播放)/.test(button.props["aria-label"] ?? ""));
  return {
    requests, probeStatusCalls, deletedLedgerRows, navigation, errors,
    root: () => renderer.root,
    async mount(mediaItemId = "media") {
      await act(async () => {
        renderer = create(React.createElement(LibraryItemDetailView, { libraryId: "library", mediaItemId }));
      });
    },
    async remount(mediaItemId) {
      await act(async () => {
        renderer.update(React.createElement(LibraryItemDetailView, { libraryId: "library", mediaItemId }));
      });
    },
    async unmount() { await act(async () => { renderer.unmount(); }); },
    async source(id) { await act(async () => { renderer.root.findByProps({ "aria-label": "播放版本" }).props.onChange({ target: { value: id } }); }); },
    async episode(number) { await act(async () => { renderer.root.findByProps({ "data-episode-number": number }).props.onClick(); }); },
    async markFavorite() {
      const btn = renderer.root.findAllByType("button").find((b) => /^(收藏|取消收藏)$/.test(b.props["aria-label"] ?? ""));
      await act(async () => { btn.props.onClick(); });
    },
    favoritePressed: () => {
      const btn = renderer.root.findAllByType("button").find((b) => /^(收藏|取消收藏)$/.test(b.props["aria-label"] ?? ""));
      return Boolean(btn?.props["aria-pressed"]);
    },
    favoriteDisabled: () => {
      const btn = renderer.root.findAllByType("button").find((b) => /^(收藏|取消收藏)$/.test(b.props["aria-label"] ?? ""));
      return Boolean(btn?.props.disabled);
    },
    async markPlayed() { await act(async () => { renderer.root.findByProps({ "aria-label": "标记为已看" }).props.onClick(); }); },
    async play() { await act(async () => { playButton().props.onClick(); }); },
    async resolve(pending, value) { await act(async () => { pending.resolve(value); }); },
    label: () => playButton().props["aria-label"],
    selectedFile: () => renderer.root.findByProps({ "aria-label": "播放版本" }).props.value,
  };
}
