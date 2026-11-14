import assert from "node:assert/strict";
import test from "node:test";

import { libraryPipelineIdentity } from "../lib/subscription-pipeline.ts";

test("链路体检用接口返回的原始库名并展示库类别", () => {
  assert.deepEqual(
    libraryPipelineIdentity({ name: "动画剧集", kind: "tv" }),
    { name: "动画剧集", kindLabel: "剧集", isDefault: false },
  );
});

test("链路体检使用当前 name 字段及默认库标记", () => {
  assert.deepEqual(
    libraryPipelineIdentity({ name: "电影库", kind: "movie", is_default: true }),
    { name: "电影库", kindLabel: "电影", isDefault: true },
  );
});
