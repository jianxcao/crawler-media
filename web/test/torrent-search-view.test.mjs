import assert from "node:assert/strict";
import test from "node:test";

import {
  aggregateFacets,
  collectEntities,
  computeSmartSortKeys,
  emptyFilters,
  ENTITY_UNPARSED,
  entityKeyOf,
  episodesOf,
  groupHitsByEntity,
  initialView,
  matchesFilters,
  maxResolution,
  mediaTypeOf,
  readSortFromUrl,
  seasonEpLabel,
  seasonEpisodeChip,
  sortHits,
  specSummary,
  toggleIn,
  torrentIdOf,
  torrentPromoBadges,
  torrentSizeLabel,
} from "../lib/torrent-search-view.ts";

function makeRelease(overrides = {}) {
  return {
    title: "沙丘2",
    year: 2024,
    season: null,
    episode: null,
    episode_to: null,
    resolution: "2160p",
    source: "WEB-DL",
    codec: "HEVC",
    hdr: "Dolby Vision",
    group: "FRDS",
    confidence: "high",
    ...overrides,
  };
}

function makeHit(overrides = {}) {
  const { release, ...rest } = overrides;
  return {
    site_id: "site-a",
    site_name: "Site A",
    id: "tid-1",
    title: "Dune.Part.Two.2024.2160p.WEB-DL.HEVC-FRDS",
    enclosure: "https://example.com/dl/1",
    size_bytes: 10 * 1024 ** 3,
    seeders: 50,
    leechers: 2,
    snatched: 120,
    upload_time: "2026-10-01T12:00:00Z",
    poster_url: null,
    free: false,
    hr: null,
    imdb_id: null,
    detail_url: null,
    category: "movie",
    release: makeRelease(release),
    ...rest,
  };
}

test("torrentIdOf prefers id and falls back to enclosure", () => {
  assert.equal(torrentIdOf(makeHit({ id: "t-9", enclosure: "https://e/9" })), "t-9");
  assert.equal(torrentIdOf(makeHit({ id: null, enclosure: "https://e/fallback" })), "https://e/fallback");
});

test("mediaTypeOf and episodesOf interpret release season and episode ranges", () => {
  assert.equal(mediaTypeOf(makeRelease({ season: null, episode: null })), "movie");
  assert.equal(mediaTypeOf(makeRelease({ season: 1, episode: null })), "tv");
  assert.equal(mediaTypeOf(makeRelease({ season: null, episode: 5 })), "tv");

  assert.deepEqual(episodesOf(makeRelease({ episode: null, episode_to: null })), []);
  assert.deepEqual(episodesOf(makeRelease({ episode: 4, episode_to: null })), [4]);
  assert.deepEqual(episodesOf(makeRelease({ episode: 2, episode_to: 5 })), [2, 3, 4, 5]);
});

test("torrentSizeLabel delegates to formatBytes for positive bytes and returns null otherwise", () => {
  assert.equal(torrentSizeLabel(null), null);
  assert.equal(torrentSizeLabel(0), null);
  assert.equal(torrentSizeLabel(1024 ** 3), "1.00 GB");
});

test("sortHits orders by seeders, size, time (missing last), snatched, complete, quality, and free", () => {
  const missingTime = makeHit({ id: "no-time", upload_time: null, seeders: 999 });
  const older = makeHit({ id: "older", upload_time: "2026-09-01T00:00:00Z", seeders: 10 });
  const newer = makeHit({ id: "newer", upload_time: "2026-10-15T00:00:00Z", seeders: 5 });

  assert.deepEqual(
    sortHits([missingTime, older, newer], { key: "time", dir: "desc" }).map((h) => h.id),
    ["newer", "older", "no-time"],
  );
  assert.deepEqual(
    sortHits([missingTime, older, newer], { key: "seeders", dir: "desc" }).map((h) => h.id),
    ["no-time", "older", "newer"],
  );

  const small = makeHit({ id: "small", size_bytes: 100 });
  const big = makeHit({ id: "big", size_bytes: 5000 });
  assert.deepEqual(
    sortHits([small, big], { key: "size", dir: "desc" }).map((h) => h.id),
    ["big", "small"],
  );

  const lessSnatched = makeHit({ id: "s1", snatched: 10 });
  const moreSnatched = makeHit({ id: "s2", snatched: 90 });
  assert.deepEqual(
    sortHits([lessSnatched, moreSnatched], { key: "snatched", dir: "desc" }).map((h) => h.id),
    ["s2", "s1"],
  );

  const movieHit = makeHit({ id: "mv", release: { season: null, episode: null, episode_to: null } });
  const singleEp = makeHit({ id: "ep1", release: { season: 1, episode: 1, episode_to: null } });
  const multiEp = makeHit({ id: "ep1-3", release: { season: 1, episode: 1, episode_to: 3 } });
  const seasonPack = makeHit({ id: "pack", release: { season: 1, episode: null, episode_to: null } });
  assert.deepEqual(
    sortHits([movieHit, singleEp, multiEp, seasonPack], { key: "complete", dir: "desc" }).map((h) => h.id),
    ["pack", "ep1-3", "ep1", "mv"],
  );

  const hdWeb = makeHit({ id: "hd-web", release: { resolution: "1080p", source: "WEB-DL" }, seeders: 80 });
  const uhdWeb = makeHit({ id: "uhd-web", release: { resolution: "2160p", source: "WEB-DL" }, seeders: 20 });
  const uhdBluray = makeHit({ id: "uhd-bd", release: { resolution: "2160p", source: "UHD Blu-ray" }, seeders: 10 });
  assert.deepEqual(
    sortHits([hdWeb, uhdWeb, uhdBluray], { key: "quality", dir: "desc" }).map((h) => h.id),
    ["uhd-bd", "uhd-web", "hd-web"],
  );

  const paid = makeHit({ id: "paid", free: false, hr: false, seeders: 100 });
  const freeHr = makeHit({ id: "free-hr", free: true, hr: true, seeders: 50 });
  const freeNoHr = makeHit({ id: "free-clean", free: true, hr: false, seeders: 10 });
  assert.deepEqual(
    sortHits([paid, freeHr, freeNoHr], { key: "free", dir: "desc" }).map((h) => h.id),
    ["free-clean", "free-hr", "paid"],
  );
});

test("computeSmartSortKeys emits complete when tv >= 50%, quality when movie > 50%, always free", () => {
  const tv1 = makeHit({ release: { season: 1, episode: 1 } });
  const tv2 = makeHit({ release: { season: 1, episode: 2 } });
  const mv1 = makeHit({ release: { season: null, episode: null } });
  const mv2 = makeHit({ release: { season: null, episode: null } });

  assert.deepEqual(computeSmartSortKeys([tv1, mv1]), ["complete", "free"]);
  assert.deepEqual(computeSmartSortKeys([tv1, tv2, mv1]), ["complete", "free"]);
  assert.deepEqual(computeSmartSortKeys([tv1, mv1, mv2]), ["quality", "free"]);
  assert.deepEqual(computeSmartSortKeys([]), ["free"]);
});

test("matchesFilters and aggregateFacets support range episodes and self-excluding facet counts", () => {
  const hits = [
    makeHit({
      id: "h1",
      site_id: "s1",
      site_name: "Site1",
      release: {
        resolution: "2160p",
        year: 2024,
        season: 1,
        episode: 1,
        episode_to: 3,
        source: "WEB-DL",
        codec: "HEVC",
        hdr: "Dolby Vision",
        group: "FRDS",
      },
    }),
    makeHit({
      id: "h2",
      site_id: "s2",
      site_name: "Site2",
      release: {
        resolution: "1080p",
        year: 2024,
        season: 1,
        episode: 2,
        episode_to: null,
        source: "Blu-ray",
        codec: "x264",
        hdr: null,
        group: "WiKi",
      },
    }),
    makeHit({
      id: "h3",
      site_id: "s1",
      site_name: "Site1",
      release: {
        resolution: "1080p",
        year: 2023,
        season: 2,
        episode: 5,
        episode_to: null,
        source: "HDTV",
        codec: "x264",
        hdr: null,
        group: "WiKi",
      },
    }),
  ];

  // Episode 2 matches h1 (range 1-3) and h2 (single 2)
  const ep2Filters = { ...emptyFilters(), episode: new Set([2]) };
  assert.deepEqual(
    hits.filter((h) => matchesFilters(h, ep2Filters)).map((h) => h.id),
    ["h1", "h2"],
  );

  // Selecting resolution = 2160p:
  // - other dimensions (e.g. site, codec) reflect only 2160p items
  // - resolution facet itself is self-excluded so 1080p still displays count=2
  const resFilters = { ...emptyFilters(), resolution: new Set(["2160p"]) };
  const facets = aggregateFacets(hits, resFilters);
  assert.deepEqual(facets.resolution, [
    { value: "1080p", count: 2 },
    { value: "2160p", count: 1 },
  ]);
  assert.equal(facets.sites.get("s1"), 1);
  assert.equal(facets.sites.get("s2") ?? 0, 0);
  assert.deepEqual(facets.codec, [
    { value: "x264", count: 0 },
    { value: "HEVC", count: 1 },
  ]);

  const toggled = toggleIn(new Set(["2160p"]), "1080p");
  assert.deepEqual([...toggled], ["2160p", "1080p"]);
  assert.deepEqual([...toggleIn(toggled, "2160p")], ["1080p"]);
});

test("entityKeyOf, collectEntities, and groupHitsByEntity group movies/tv and sink unparsed hits", () => {
  const unparsed = makeHit({ id: "u1", release: { title: "", year: null, season: null, episode: null } });
  const tv2022 = makeHit({ id: "t1", release: { title: "Severance", year: 2022, season: 1, episode: 1 } });
  const tv2025 = makeHit({ id: "t2", release: { title: "Severance", year: 2025, season: 2, episode: 1 } });
  const mv1984 = makeHit({ id: "m1", release: { title: "Dune", year: 1984, season: null, episode: null } });
  const mv2021 = makeHit({ id: "m2", release: { title: "Dune", year: 2021, season: null, episode: null } });

  assert.equal(entityKeyOf(unparsed), ENTITY_UNPARSED);
  assert.equal(entityKeyOf(tv2022), "tv|Severance");
  assert.equal(entityKeyOf(tv2025), "tv|Severance");
  assert.equal(entityKeyOf(mv1984), "movie|Dune|1984");
  assert.equal(entityKeyOf(mv2021), "movie|Dune|2021");

  const items = [unparsed, tv2022, mv1984, tv2025, mv2021];
  const entities = collectEntities(items);
  assert.equal(entities.size, 3);
  assert.equal(entities.get("tv|Severance")?.mediaType, "tv");

  const buckets = groupHitsByEntity(items);
  assert.deepEqual(
    buckets.map((b) => [b.key, b.rows.map((r) => r.id)]),
    [
      ["tv|Severance", ["t1", "t2"]],
      ["movie|Dune|1984", ["m1"]],
      ["movie|Dune|2021", ["m2"]],
      [ENTITY_UNPARSED, ["u1"]],
    ],
  );
});

test("presentation helpers format labels, chips, spec summary, badges, and URL params", () => {
  assert.equal(seasonEpLabel(makeRelease({ season: 2, episode: 3, episode_to: null })), "S02E03");
  assert.equal(seasonEpLabel(makeRelease({ season: 1, episode: 1, episode_to: 6 })), "S01E01-E06");
  assert.equal(seasonEpLabel(makeRelease({ season: 1, episode: null, episode_to: null })), "S01");
  assert.equal(seasonEpLabel(makeRelease({ season: null, episode: 5, episode_to: null })), "E05");

  assert.deepEqual(
    seasonEpisodeChip(makeRelease({ season: 2, episode: null, episode_to: null })),
    { text: "第2季", pack: true },
  );
  assert.deepEqual(
    seasonEpisodeChip(makeRelease({ season: 2, episode: 5, episode_to: null })),
    { text: "第2季 · 第5集", pack: false },
  );
  assert.equal(seasonEpisodeChip(makeRelease({ season: null, episode: null })), null);

  assert.equal(
    specSummary(
      makeRelease({
        season: 1,
        episode: 4,
        resolution: "2160p",
        source: "WEB-DL",
        codec: "HEVC",
        hdr: "HDR10",
        group: "FRDS",
      }),
    ),
    "S01E04 · 2160p · WEB-DL · HEVC · HDR10 · FRDS",
  );

  assert.equal(
    maxResolution([
      makeHit({ release: { resolution: "720p" } }),
      makeHit({ release: { resolution: "2160p" } }),
      makeHit({ release: { resolution: "1080p" } }),
    ]),
    "2160p",
  );

  assert.deepEqual(
    torrentPromoBadges(makeHit({ free: true, hr: true }), "row").map((b) => b.text),
    ["免费", "H&R"],
  );
  assert.deepEqual(
    torrentPromoBadges(makeHit({ free: false, hr: false }), "poster"),
    [],
  );

  assert.deepEqual(readSortFromUrl("?sort=size:asc"), { key: "size", dir: "asc" });
  assert.deepEqual(readSortFromUrl("?sort=invalid:asc"), { key: "seeders", dir: "desc" });
  assert.equal(initialView("?view=group"), "group");
  assert.equal(initialView("?poster=1"), "poster");
  assert.equal(initialView(""), "list");
});
