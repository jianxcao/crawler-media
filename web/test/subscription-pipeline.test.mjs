import assert from "node:assert/strict";
import test from "node:test";

import { libraryPipelineIdentity } from "../lib/subscription-pipeline.ts";

test("链路体检用接口返回的原始库名并展示库类别", () => {
  assert.deepEqual(
    libraryPipelineIdentity({ name: "动画剧集", kind: "tv" }),
    { name: "动画剧集", kindLabel: "剧集", isDefault: false },
  );
});

test("新旧库名字段同时存在时优先使用 library_name", () => {
  assert.deepEqual(
    libraryPipelineIdentity({
      library_name: "电影库",
      name: "旧字段名称",
      kind: "movie",
      is_default: true,
    }),
    { name: "电影库", kindLabel: "电影", isDefault: true },
  );
});
