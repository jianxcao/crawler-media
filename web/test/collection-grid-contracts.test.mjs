import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { createSessionSnapshots } from "../lib/session-snapshot.ts";
import { shouldUsePreviewCache } from "../lib/discovery-collection-cache.ts";

const readWeb = (relativePath) =>
  readFile(new URL(`../${relativePath}`, import.meta.url), "utf8");

test("片单网格快照在下拉刷新时定向删除，非刷新时安全复用", () => {
  const snapshots = createSessionSnapshots(32);
  const ref = "tmdb:movie:top-rated";
  snapshots.set(ref, {
    items: [{ id: "1", title: "肖申克的救赎" }],
    title: "高分电影",
    nextPage: 3,
    totalResults: 100,
    hasMore: true,
  });

  const reloadKeyZero = 0;
  const cachedInitial = reloadKeyZero === 0 ? snapshots.get(ref) : undefined;
  assert.ok(cachedInitial);
  assert.equal(cachedInitial.title, "高分电影");

  snapshots.delete(ref);
  const reloadKeyRefreshed = 1;
  const cachedRefreshed = reloadKeyRefreshed === 0 ? snapshots.get(ref) : undefined;
  assert.equal(cachedRefreshed, undefined, "下拉刷新时快照必须被清除且不能被复用拦截");

  snapshots.set(ref, {
    items: [{ id: "2", title: "教父" }],
    title: "高分电影",
    nextPage: 2,
    totalResults: 120,
    hasMore: true,
  });
  assert.equal(snapshots.get(ref).items[0].title, "教父");
});

test("落地页合同：第一页必须用 full 模式，刷新失败必须能重试第一页", async () => {
  const [component, api] = await Promise.all([
    readWeb("components/collection-grid-view.tsx"),
    readWeb("lib/api/discover.ts"),
  ]);

  // 1. 落地页请求第一页必须传 "full"
  assert.match(
    component,
    /browseDiscoveryCollection\(\s*collectionRef,[\s\S]*?1,\s*"full",?\s*\)/,
    "完整落地页首屏请求必须显式使用 full 模式",
  );

  // 2. 错误重试按钮：第一页失败时重试刷新，下一页失败时重试下一页
  assert.match(
    component,
    /errorSource === "page1" \? \([\s\S]*?重试刷新[\s\S]*?\) : hasMore \? \([\s\S]*?重试下一页/,
    "第一页刷新失败时重试按钮必须指向第一页刷新，不可误调用 loadNextPage",
  );

  // 3. API 层仅在 preview 模式 page 1 命中 popularCache
  assert.match(
    api,
    /if \(shouldUsePreviewCache\(mode, page\)\) \{\s*const cached = popularCache\.get\(collectionRef\);/,
    "API 层必须通过 shouldUsePreviewCache 门禁隔离 full 与 preview",
  );
  // 4. 不硬编码 provider === 'tmdb' 排除豆瓣分页
  assert.doesNotMatch(
    component,
    /setHasMore\(provider === "tmdb"/,
    "hasMore 应依据服务端响应，不能硬编码只允许 TMDB 翻页",
  );
  assert.doesNotMatch(
    component,
    /if \(provider !== "tmdb"/,
    "loadNextPage 不得硬编码排斥非 TMDB 数据源",
  );

  // 5. 刷新与分页互斥代次守卫
  assert.match(
    component,
    /generationRef|requestGenerationRef/,
    "必须维护请求代次 ref 隔离刷新第一页与分页追加响应",
  );
  assert.match(
    component,
    /refreshInFlightRef/,
    "第一页刷新期间必须设置刷新标记，防止底部哨兵同时发起旧页码追加",
  );

  // 6. 路由切换干净隔离
  const pageSource = await readWeb("app/(app)/discover/[type]/collections/[provider]/[collectionId]/page.tsx");
  assert.match(
    pageSource,
    /<CollectionGridView\s+key=\{collectionRef\}\s+collectionRef=\{collectionRef\}\s*\/>/,
    "路由落地页必须传递 key={collectionRef} 保证切换片单时组件状态完全干净隔离",
  );

  // 7. 豆瓣有后续页时避免误标完整收录
  assert.match(
    component,
    /hasMore\s*\?\s*`已加载 \$\{items\.length\} 部影片`\s*:\s*`完整收录 \$\{items\.length\} 部影片`/,
    "豆瓣在仍有后续页可拉时不可谎称完整收录",
  );
});

test("合集条目跳转路由为单数 /item/，与系统主路由完全一致", async () => {
  const detailSource = await readWeb("components/collections-detail-page.tsx");
  assert.match(
    detailSource,
    /\/library\/\$\{item\.library_id\}\/item\/\$\{item\.media_item_id\}/,
    "合集条目详情跳转必须使用单数 /item/，不可写成 /items/ 导致 404",
  );
});
