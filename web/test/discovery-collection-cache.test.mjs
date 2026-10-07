import assert from "node:assert/strict";
import test from "node:test";

import { shouldUsePreviewCache } from "../lib/discovery-collection-cache.ts";

test("只有 preview 模式的第一页允许使用横滚行简略缓存", () => {
  assert.equal(shouldUsePreviewCache("preview", 1), true);
  assert.equal(shouldUsePreviewCache("preview", 2), false);
  assert.equal(shouldUsePreviewCache("full", 1), false, "完整落地页第一页必须发起真实分页请求获取真实的 has_more 与总数");
  assert.equal(shouldUsePreviewCache("full", 2), false);
});
