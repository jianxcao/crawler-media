import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  discoveryFilterCount,
  discoveryFiltersQuery,
  parseDiscoveryFilters,
} from "../lib/discovery-filters.ts";

const readWeb = (relativePath) =>
  readFile(new URL(`../${relativePath}`, import.meta.url), "utf8");

test("六维筛选可从分享 URL 安全恢复并重新序列化", () => {
  const filters = parseDiscoveryFilters({
    genres: "878,28,878,bad",
    country: "jp",
    year: "2025",
    rating: "7",
    runtime: "90",
    sort: "rating",
  });

  assert.deepEqual(filters, {
    genreIds: [878, 28],
    originCountry: "JP",
    year: 2025,
    ratingGte: 7,
    runtimeLte: 90,
    sort: "rating",
  });
  assert.equal(discoveryFilterCount(filters), 6);
  assert.equal(
    discoveryFiltersQuery(filters),
    "genres=878%2C28&country=JP&year=2025&rating=7&runtime=90&sort=rating",
  );
});

test("非法筛选值被忽略并回退热门排序", () => {
  assert.deepEqual(
    parseDiscoveryFilters({
      genres: "-1,0,nope",
      country: "Japan",
      year: "2200",
      rating: "11",
      runtime: "0",
      sort: "unknown",
    }),
    { genreIds: [], sort: "popular" },
  );
});

test("发现页筛选视图在第一页时完全替换列表且在筛选条件变更时响应", async () => {
  const component = await readWeb("components/filtered-discovery-view.tsx");
  assert.match(
    component,
    /if\s*\(\s*page\s*===\s*1\s*\)\s*return\s*result\.items;/,
    "第一页加载必须重置并直接返回新结果，不可追加在旧筛选结果后",
  );
  assert.match(
    component,
    /discoveryFiltersKey/,
    "必须通过 discoveryFiltersKey 建立筛选依赖，保证条件切换时重新加载第一页",
  );
});
