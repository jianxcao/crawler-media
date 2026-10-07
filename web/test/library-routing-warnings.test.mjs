import assert from "node:assert/strict";
import test from "node:test";

import { routingOverlapWarnings } from "../lib/library-routing-warnings.ts";

function lib(overrides = {}) {
  return {
    id: "lib-1",
    name: "电影A",
    kind: "movie",
    root_paths: ["/data/movies"],
    root_missing: false,
    is_default: false,
    access_mode: "everyone",
    admin_visible: true,
    member_ids: [],
    stats: { item_count: 0, file_count: 0, total_size_bytes: 0 },
    ...overrides,
  };
}

test("完全相同的 match_rules 提示相同", () => {
  const warnings = routingOverlapWarnings([
    lib({ name: "华语电影1", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN"] }] }),
    lib({ name: "华语电影2", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN"] }] }),
  ]);
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /收藏范围相同/);
});

test("相交但不完全相同的 match_rules 提示有重叠", () => {
  const warnings = routingOverlapWarnings([
    lib({ name: "华语电影", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN", "TW", "HK"] }] }),
    lib({ name: "大陆电影", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["CN"] }] }),
  ]);
  assert.equal(warnings.length, 1);
  assert.match(warnings[0], /收藏范围有重叠/);
});

test("不同 field 或不相交 values 不产生警告", () => {
  const warnings = routingOverlapWarnings([
    lib({ name: "日本动漫", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["JP"] }] }),
    lib({ name: "欧美电影", kind: "movie", match_rules: [{ field: "origin_countries", op: "any_of", values: ["US"] }] }),
  ]);
  assert.equal(warnings.length, 0);
});
