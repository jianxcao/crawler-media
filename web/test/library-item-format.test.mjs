import assert from "node:assert/strict";
import test from "node:test";
import {
  fileCodecLabel,
  fileDirectory,
  fileHdrLabel,
  fileNameOf,
  fileResolutionLabel,
  seasonLabel,
  sortHdrFormats,
  videoCodecLabel,
} from "../lib/library-item-format.ts";

const makeFile = (overrides = {}) => ({
  file_id: "f1",
  path: "/movies/a.mkv",
  units: [],
  presence: "present",
  publication: "published",
  season: null,
  episode: null,
  resolution: null,
  codec: null,
  hdr: null,
  quality_source: null,
  video_track: null,
  ...overrides,
});

test("videoCodecLabel and fileCodecLabel format known and fallback codecs", () => {
  assert.equal(videoCodecLabel("hevc"), "HEVC");
  assert.equal(videoCodecLabel("h265"), "HEVC");
  assert.equal(videoCodecLabel("h264"), "H.264");
  assert.equal(videoCodecLabel("vp8"), "VP8");
  assert.equal(videoCodecLabel(null), null);

  assert.equal(fileCodecLabel(makeFile({ codec: "av1", video_track: { codec: "h264" } })), "AV1");
  assert.equal(fileCodecLabel(makeFile({ video_track: { codec: "hevc" } })), "HEVC");
  assert.equal(fileCodecLabel(null), null);
});

test("fileResolutionLabel and fileHdrLabel derive labels from file or video_track", () => {
  assert.equal(fileResolutionLabel(makeFile({ resolution: "2160p" })), "4K");
  assert.equal(
    fileResolutionLabel(makeFile({ video_track: { width: 3840, height: 2160 } })),
    "4K",
  );
  assert.equal(
    fileResolutionLabel(makeFile({ video_track: { width: 1920, height: 1080 } })),
    "1080p",
  );
  assert.equal(
    fileResolutionLabel(makeFile({ video_track: { width: 1280, height: 720 } })),
    "720p",
  );
  assert.equal(
    fileResolutionLabel(makeFile({ video_track: { width: 640, height: 480 } })),
    "480p",
  );
  assert.equal(fileResolutionLabel(null), null);

  assert.equal(fileHdrLabel(makeFile({ hdr: "Dolby Vision" })), "Dolby Vision");
  assert.deepEqual(sortHdrFormats(["HLG", "HDR10+", "Dolby Vision", "Custom"]), [
    "Dolby Vision",
    "HDR10+",
    "HLG",
    "Custom",
  ]);
});

test("fileNameOf, fileDirectory, and seasonLabel handle edge paths and seasons", () => {
  assert.equal(fileNameOf("/a/b/c.mkv"), "c.mkv");
  assert.equal(fileNameOf("C:\\a\\b\\c.mkv"), "c.mkv");
  assert.equal(fileNameOf("plain.mkv"), "plain.mkv");

  assert.equal(fileDirectory("/a/b/c.mkv"), "/a/b");
  assert.equal(fileDirectory("/a.mkv"), "/");
  assert.equal(fileDirectory("C:\\a\\b.mkv"), "C:\\a");
  assert.equal(fileDirectory("plain.mkv"), "—");

  assert.equal(seasonLabel(0, true), "特别篇");
  assert.equal(seasonLabel(2, true), "第 2 季");
  assert.equal(seasonLabel(0, false), "特别篇 · 未入库");
  assert.equal(seasonLabel(3, false), "第 3 季 · 未入库");
});
