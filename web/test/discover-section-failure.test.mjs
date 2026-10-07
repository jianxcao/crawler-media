import assert from "node:assert/strict";
import test from "node:test";

import {
  normalizeDiscoverySectionError,
  shouldCacheDiscoverySection,
  discoverySectionStatus,
  discoverySectionMessage,
  heroFailureMessage,
  retryCollectionCacheKeys,
} from "../lib/discovery-section-state.ts";

test("规范化提取分区错误信息", () => {
  assert.deepEqual(
    normalizeDiscoverySectionError({ code: "discover.section_failed", message: "分区暂时无法加载，请重试" }),
    { code: "discover.section_failed", message: "分区暂时无法加载，请重试" },
  );

  assert.equal(normalizeDiscoverySectionError(null), undefined);
  assert.equal(normalizeDiscoverySectionError({}), undefined);
  assert.equal(normalizeDiscoverySectionError({ code: "", message: "err" }), undefined);
});

test("带有错误的分区绝不缓存，正常空分区允许缓存", () => {
  const err = { code: "discover.section_failed", message: "fail" };
  assert.equal(shouldCacheDiscoverySection(err), false);
  assert.equal(shouldCacheDiscoverySection(undefined), true);
});

test("发现页全失败、部分失败与全成功状态判定", () => {
  const err = { code: "discover.section_failed", message: "fail" };
  assert.equal(discoverySectionStatus([]), "ok");
  assert.equal(
    discoverySectionStatus([
      { error: err },
      { error: err },
    ]),
    "all-failed",
  );
  assert.equal(
    discoverySectionStatus([
      { error: err },
      { error: undefined },
    ]),
    "partial",
  );
  assert.equal(
    discoverySectionStatus([
      { error: undefined },
      { error: undefined },
    ]),
    "ok",
  );
});

test("分区错误提示文案抽取", () => {
  assert.equal(
    discoverySectionMessage({ error: { code: "err", message: "上游超时" } }),
    "上游超时",
  );
  assert.equal(discoverySectionMessage({ error: undefined }), null);
});

test("Hero 失败文案与普通分区相同，成功空为 null", () => {
  assert.equal(
    heroFailureMessage({ error: { code: "discover.section_failed", message: "分区暂时无法加载，请重试" } }),
    "分区暂时无法加载，请重试",
  );
  assert.equal(heroFailureMessage({}), null);
});

test("重试只列出失败分区的 collectionCache key", () => {
  const keys = retryCollectionCacheKeys("movie:tmdb", [
    { collectionRef: "tmdb:movie:featured-weekly", previewLimit: 20, error: { code: "x", message: "y" } },
    { collectionRef: "tmdb:movie:popular", previewLimit: 20 },
  ]);
  assert.equal(keys.pageKey, "movie:tmdb");
  assert.deepEqual(keys.collectionKeys, ["tmdb:movie:featured-weekly:20"]);
});

test("首行失败的分区在存在错误时必须保留 hero presentation", () => {
  const section = {
    id: "featured-weekly",
    title: "本周精选",
    presentation: "hero",
    items: [],
    error: { code: "discover.section_failed", message: "fail" },
  };
  const sectionError = normalizeDiscoverySectionError(section.error);
  const presentation =
    section.presentation === "hero" &&
    (Boolean(sectionError) || section.items.some((i) => i.backdrop_url))
      ? "hero"
      : "poster-row";
  assert.equal(presentation, "hero");
});
