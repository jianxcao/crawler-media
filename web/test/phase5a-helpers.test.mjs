import assert from "node:assert/strict";
import test from "node:test";

import {
  TrackTone,
  groupByLanguage,
  subtitleEntries,
  topAudioSpec,
  videoTrackEntries,
} from "../lib/media-track-format.ts";
import {
  bucketize,
  computeDeltaChip,
  niceMax,
  rankChange,
} from "../lib/watch-stats-helpers.ts";
import {
  detailHref,
  deviceLabel,
  formatPlayClock,
  unitLabel,
} from "../lib/media-activity-helpers.ts";

test("topAudioSpec returns null for <2 streams and picks highest codec tier with informative profile", () => {
  assert.equal(topAudioSpec([]), null);
  assert.equal(
    topAudioSpec([{ codec: "aac", channels: 2, language: "eng" }]),
    null,
  );
  assert.equal(
    topAudioSpec([
      { codec: "aac", channels: 2, language: "eng", profile: "LC" },
      { codec: "truehd", channels: 8, language: "eng" },
    ]),
    "Dolby TrueHD 7.1",
  );
  assert.equal(
    topAudioSpec([
      { codec: "ac3", channels: 6, language: "eng" },
      { codec: "dts", channels: 8, language: "eng", profile: "DTS-HD MA" },
    ]),
    "DTS-HD MA 7.1",
  );
});

test("subtitleEntries and groupByLanguage number embedded tracks sequentially, strip video stem, and order languages", () => {
  const entries = subtitleEntries(
    [
      {
        language: "eng",
        codec: "subrip",
        external: false,
        default: false,
        forced: false,
        title: null,
        file_name: null,
      },
      {
        language: "chs",
        codec: "ass",
        external: true,
        default: false,
        forced: false,
        title: null,
        file_name: "Movie.2024.chs.ass",
      },
      {
        language: "chs",
        codec: "subrip",
        external: false,
        default: true,
        forced: false,
        title: null,
        file_name: null,
      },
      {
        language: "chs",
        codec: "ass",
        external: true,
        default: false,
        forced: false,
        title: "ai-chs",
        file_name: "Movie.2024.ai-chs.ass",
      },
    ],
    "Movie.2024",
  );

  // Embedded tracks are numbered 1, 2 skipping the external subtitle in between
  assert.equal(entries[0].primary, "内封轨 1");
  assert.equal(entries[1].primary, "chs.ass");
  assert.equal(entries[1].deletable, "Movie.2024.chs.ass");
  assert.equal(entries[2].primary, "内封轨 2");
  assert.equal(entries[2].deletable, null);
  assert.equal(entries[3].tone, TrackTone.Ai);
  assert.equal(entries[3].secondary, "AI 外挂");

  const groups = groupByLanguage(entries);
  assert.deepEqual(
    groups.map((g) => g.language),
    ["简体中文", "英语"],
  );
  // Default -> external -> AI within group
  assert.deepEqual(
    groups[0].entries.map((e) => e.primary),
    ["内封轨 2", "chs.ass", "ai-chs.ass"],
  );
});

test("videoTrackEntries emits pending row when unprobed and formats probed stream summary", () => {
  const pending = videoTrackEntries({
    file_id: "f1",
    path: "/media/movie.mkv",
    presence: "present",
    publication: "published",
    season: null,
    episode: null,
    resolution: null,
    codec: null,
    hdr: null,
    quality_source: null,
    audio_tracks: [],
    subtitle_tracks: [],
    video_track: null,
    probed: false,
    probe_queued: false,
  });
  assert.equal(pending.length, 1);
  assert.equal(pending[0].key, "video:pending");

  const probed = videoTrackEntries({
    file_id: "f2",
    path: "/media/movie.mkv",
    presence: "present",
    publication: "published",
    season: null,
    episode: null,
    resolution: "2160p",
    codec: "hevc",
    hdr: "DoVi",
    quality_source: "WEB-DL",
    audio_tracks: [],
    subtitle_tracks: [],
    video_track: {
      codec: "hevc",
      width: 3840,
      height: 2160,
      frame_rate: 23.976,
      bit_rate: 15_000_000,
      duration_secs: 7200,
    },
    probed: true,
    probe_queued: false,
  });
  assert.equal(probed.length, 1);
  assert.equal(probed[0].format, "H.265");
  assert.equal(probed[0].primary, "H.265 · 4K · 23.98fps · 15.0 Mbps · 120 分钟");
  assert.deepEqual(probed[0].flags, ["DoVi"]);
});

test("watch stats helpers handle tail-aligned buckets, niceMax, delta chip, and rank shifts", () => {
  const days = Array.from({ length: 10 }, (_, i) => ({
    date: `2026-11-0${i + 1}`,
    watched_ms: i * 1000,
    plays: i,
    completed: 0,
    members: 1,
  }));
  const buckets = bucketize(days, 7);
  assert.equal(buckets.length, 2);
  assert.equal(buckets[0].length, 3);
  assert.equal(buckets[1].length, 7);

  assert.equal(niceMax(0), 1);
  assert.equal(niceMax(12), 20);
  assert.equal(niceMax(42), 50);
  assert.equal(niceMax(55), 100);

  assert.equal(computeDeltaChip({ current: 0, previous: 0, available: false }), null);
  assert.equal(computeDeltaChip({ current: 0, previous: 0, available: true }), null);
  assert.equal(
    computeDeltaChip({ current: 5, previous: 0, available: true })?.text,
    "新增",
  );
  assert.equal(
    computeDeltaChip({ current: 0.75, previous: 0.5, available: true, inPoints: true })?.text,
    "25 个百分点",
  );
  assert.equal(
    computeDeltaChip({ current: 150, previous: 100, available: true })?.text,
    "50%",
  );

  const row = (id) => ({
    media: {
      media_item_id: id,
      library_id: "lib1",
      title: id,
      kind: "movie",
      year: 2025,
      poster_url: null,
      browsable: true,
    },
    watched_ms: 1000,
    plays: 1,
    members: 1,
  });
  assert.equal(rankChange(row("a"), 0, [row("a"), row("b")])?.text, "蝉联");
  assert.equal(rankChange(row("c"), 0, [row("a"), row("b")])?.text, "新上榜");
  assert.equal(rankChange(row("b"), 0, [row("a"), row("b")])?.text, "▲ 上期第 2");
  assert.equal(rankChange(row("a"), 1, [row("a"), row("b")])?.text, "▼ 上期第 1");
});

test("media activity helpers format clocks, episodes, devices, and browsable links", () => {
  assert.equal(formatPlayClock(65_000), "1:05");
  assert.equal(formatPlayClock(3_665_000), "1:01:05");

  assert.equal(unitLabel({ kind: "movie", season_number: 1, episode_number: 1 }), null);
  assert.equal(unitLabel({ kind: "tv", season_number: 1, episode_number: 9 }), "S01E09");

  assert.equal(deviceLabel("Infuse", "Infuse"), "Infuse");
  assert.equal(deviceLabel("Infuse", "Living Room Apple TV"), "Infuse · Living Room Apple TV");
  assert.equal(deviceLabel("", ""), "未知设备");

  assert.equal(
    detailHref({ library_id: "l1", media_item_id: "m1", browsable: true }),
    "/library/l1/item/m1",
  );
  assert.equal(
    detailHref({ library_id: "l1", media_item_id: "m1", browsable: false }),
    null,
  );
  assert.equal(
    detailHref({ library_id: null, media_item_id: "m1", browsable: true }),
    null,
  );
});
