"use client";

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { Link } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { ContentEmptyState } from "@/components/content-empty-state";
import { HScroller } from "@/components/h-scroller";
import { LIBRARY_KIND_META } from "@/components/library-kind-meta";
import { LibraryKindCoverArt } from "@/components/library-cover-art";
import {
  FilmIcon,
  GearIcon,
  ListIcon,
  MoreIcon,
  PlusIcon,
} from "@/components/icons";
import { MediaRow } from "@/components/media-row";
import type { PosterCardAction } from "@/components/poster-card";
import { UpNextRow } from "@/components/up-next-row";
import {
  type LibraryItem,
  type MediaLibrary,
  listLibraries,
  listLibraryItems,
} from "@/lib/api/libraries";
import {
  type FavoriteItem,
  type FavoritesPage,
  listFavorites,
  listUpNext,
  type UpNextItem,
} from "@/lib/api/playback";
import type { Subscription } from "@/lib/api/subscriptions";
import { publicEnv } from "@/lib/env";
import { favoriteLevelLabel } from "@/lib/favorites";
import {
  buildHomeRows,
  FAVORITES_SORT_PRESETS,
  type HomeLibraryLike,
  type HomeRow,
  orderParamFor,
  rowTitle,
} from "@/lib/home-rows";
import {
  coverFetchKey,
  rowFetchKey,
  rowItemQuery,
} from "@/lib/home-row-items";
import { formatBytes } from "@/lib/format";
import { cardVariantFor, imageUrl } from "@/lib/image-proxy";
import { libraryInventoryAction } from "@/lib/library-inventory-summary";
import type { MediaItem } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { useUiPrefs } from "@/lib/ui-prefs";
import { useVisiblePolling } from "@/lib/use-visible-polling";
import { useScrollRestoration } from "@/lib/use-scroll-restoration";

/** 每个库行 / 合集行的格数（也是本页向服务端要的条目数上限）。 */
const RECENT_COUNT = 20;
/** 「接下来继续」横滚行最多几张卡。 */
const UP_NEXT_COUNT = 20;
/** 「我的收藏」横滚行只放最近收藏的这么多部，更多的到 /library/favorites 看。 */
const FAVORITES_COUNT = 20;

/**
 * 库存条目的悬浮操作与本卡 hover 的完整度文案同源：季或集有一项未齐就
 * 「补齐缺集」；当前已知季集全部在库则「自动续订」，等待未来出现的新季。
 * 已在“我的订阅”中的条目由 PosterCardVisual 统一隐藏操作，不再显示
 * 没有决策价值的“已订阅”按钮。
 */
export function libraryCardAction(item: LibraryItem): PosterCardAction {
  // 新契约的 LibraryItem 无库存汇总（inventory_summary 已删），一律按无摘要处理
  return libraryInventoryAction(item.kind, null);
}

/**
 * MediaLibrary → 首页行清单的投影（lib/home-rows.ts 的 HomeLibraryLike）。
 * 新契约的库 id 是 UUID 字符串，而 home-rows 的合并逻辑把它当数字行键用
 * （存过清单的旧数字 id 将不再命中，出厂默认行照常生成）；viewer_access /
 * 首页要遵守管理页的 exclude_from_home 开关。
 */
function toHomeLibraryLike(libraries: MediaLibrary[]): HomeLibraryLike[] {
  return libraries.map((lib) => ({
    id: lib.id,
    name: lib.name,
    kind: lib.kind,
    viewer_access: true,
    exclude_from_home: Boolean(lib.exclude_from_home),
  }));
}

/**
 * 订阅的实际归属库：显式指定优先，否则该类型的默认库。
 * 与后端 resolve_for_subscription 同一语义，库页与单库页共用。
 */
export function effectiveLibraryId(
  sub: Subscription,
  libraries: MediaLibrary[],
): string | null {
  if (sub.library_id) {
    const hit = libraries.find((l) => l.id === sub.library_id);
    if (hit) return hit.id;
  }
  return libraries.find((l) => l.kind === sub.media.kind && l.is_default)?.id ?? null;
}

/** 媒体库首页摘要：只聚合接口随库返回的预计算快照，不触发额外请求。 */
export function libraryStatsSummary(libraries: MediaLibrary[] | null): string {
  if (libraries === null) return "正在汇总媒体库统计…";
  if (libraries.length === 0) return "还没有媒体库，创建后会在这里显示库存统计";
  const movieCount = libraries
    .filter((library) => library.kind === "movie")
    .reduce((total, library) => total + library.stats.item_count, 0);
  const tvCount = libraries
    .filter((library) => library.kind === "tv")
    .reduce((total, library) => total + library.stats.item_count, 0);
  const videoCount = libraries
    .filter((library) => library.kind === "video")
    .reduce((total, library) => total + library.stats.item_count, 0);
  const totalSizeBytes = libraries.reduce(
    (total, library) => total + library.stats.total_size_bytes,
    0,
  );
  const videoPart = videoCount > 0 ? ` · ${videoCount} 个其他视频` : "";
  return `${libraries.length} 个媒体库 · ${movieCount} 部电影 · ${tvCount} 部剧集${videoPart} · 共占用 ${formatBytes(totalSizeBytes)} 存储空间`;
}

/**
 * 媒体库页（/library）：全部库的 Emby 风格卡片横排——**只做浏览入口**。
 *
 * 每张卡是一个库：封面用库内作品的海报做「货架」展示（最多 4 张站立海报
 * 带底部倒影，纯前端 CSS 合成、零后端开销），叠库名/类型/统计；
 * 点击进入单库海报墙（/library/[id]）。库的增删改/设默认/扫描/排序全部在
 * 管理页（/library/manage）完成，见 docs/design/library-manage.md——卡片上只留
 * 预告"马上会看到新内容"的信息：扫描进度环与「入库中」徽标。
 *
 * 数据源是 library_file 台账的**真实库存**（L3 起）：入库管线与存量扫描
 * 落账的文件聚合，不再用订阅占位。
 */
//: 首帧等合集列表的预算（毫秒）。见 reload 里的说明
const COLLECTIONS_FIRST_PAINT_BUDGET_MS = 1500;

/**
 * 上一次成功加载的首页数据（模块级，进程内存，跨路由驻留）。
 *
 * 顶栏「媒体库」等入口回到本页时组件会重挂载：没有这份快照，首帧只能画
 * 「正在加载…」的矮内容，滚动恢复（use-scroll-restoration）要等行数据到齐、
 * 内容撑到旧位置的高度才能写入 scrollTop——页面先在最上端闪一拍、再跳回
 * 离开处。快照让首帧直接以全量内容渲染，恢复就能在首次绘制前落位。
 * 数据仍照常重新拉取刷新，快照只是绘制起点，不承担缓存有效期职责。
 */
let lastLoadedHome: {
  libraries: MediaLibrary[];
  upNext: UpNextItem[];
  favorites: FavoritesPage;
  itemsByKey: Map<string, LibraryItem[]>;
} | null = null;

export function LibraryView({ hero }: { hero?: ReactNode }) {
  const { canManageLibraries } = usePermissions();
  // 首页的行清单存在界面偏好里（成员各存各的），应用启动时已随全站偏好拉过一次
  const { prefs } = useUiPrefs();
  const homePrefs = prefs.home;
  const scrollRef = useScrollRestoration("library");
  // 各状态初值取上次会话留存的快照（没有则走加载态），见 lastLoadedHome
  const [libraries, setLibraries] = useState<MediaLibrary[] | null>(
    () => lastLoadedHome?.libraries ?? null,
  );
  // 库行 / 合集行 / 库卡片封面的条目按「取数键」缓存：同一个库同一种排序只请求
  // 一次（库卡片封面与默认的「最近添加」行共用 added_at 那一份）
  const [itemsByKey, setItemsByKey] = useState<Map<string, LibraryItem[]>>(
    () => lastLoadedHome?.itemsByKey ?? new Map(),
  );
  const [upNext, setUpNext] = useState<UpNextItem[] | null>(() => lastLoadedHome?.upNext ?? null);
  // 我的收藏：与接下来继续同一轮拉取、同一套失败策略（拉不到保留旧数据）
  const [favorites, setFavorites] = useState<FavoritesPage | null>(
    () => lastLoadedHome?.favorites ?? null,
  );
  const [failed, setFailed] = useState(false);

  // 轮询乱序守卫：扫描期间后端响应时间抖动大，上一轮的慢响应可能晚于
  // 下一轮到达，不作废就会用旧快照覆盖新状态（进度回跳、卡片状态闪烁）
  const reloadSeq = useRef(0);
  // 上一轮已拉过条目时的快照（库状态 + 要取哪些行）：都没变说明库存也不会变，
  // 不必再逐行全量拉一遍——否则空闲时每 30 秒也要打出 1 + N 个请求
  const lastSnapshot = useRef<string | null>(null);
  const reloadRef = useRef<() => void>(() => {});
  const reload = useCallback(() => {
    const seq = ++reloadSeq.current;
    listLibraries()
      .then(async (libs) => {
        if (seq !== reloadSeq.current) return;
        setFailed(false);
        const libsSnapshot = JSON.stringify(libs);
        // 内容没变就复用旧引用，跳过整页卡片的无谓重渲染
        setLibraries((prev) => (prev && JSON.stringify(prev) === libsSnapshot ? prev : libs));

        // 只取**显示中的**行：隐藏的行不发请求。合集端点已删，合集行自然不再出现
        const rows = buildHomeRows(homePrefs, toHomeLibraryLike(libs), []).filter(
          (row) => !row.hidden,
        );
        const favoritesRow = rows.find((row) => row.kind === "favorites");
        // 接下来继续 / 我的收藏随播放状态变，每轮都拉；失败不拖垮首页，保留旧数据
        const [latestUpNext, latestFavorites] = await Promise.all([
          rows.some((row) => row.kind === "up-next")
            ? listUpNext(UP_NEXT_COUNT).catch(() => null)
            : Promise.resolve(null),
          favoritesRow && favoritesRow.kind === "favorites"
            ? listFavorites(
                FAVORITES_COUNT,
                0,
                // 「未看优先」是首页这一行的默认：没看完的提前（全量页不传，保持收藏时间序）
                favoritesRow.sort === "unwatched_first",
                {
                  sort:
                    favoritesRow.sort === "unwatched_first" ? "favorited_at" : favoritesRow.sort,
                  // 反转了自然方向才带 order，与海报墙同一条规矩
                  order: orderParamFor(
                    FAVORITES_SORT_PRESETS[favoritesRow.sort].direction,
                    favoritesRow.reversed,
                  ),
                },
              ).catch(() => null)
            : Promise.resolve(null),
        ]);
        if (seq !== reloadSeq.current) return;
        if (latestUpNext !== null) setUpNext(latestUpNext);
        else setUpNext((previous) => previous ?? []);
        if (latestFavorites !== null) setFavorites(latestFavorites);
        else setFavorites((previous) => previous ?? { items: [], total: 0 });

        const fetches = rowFetches(
          rows,
          libs.filter((library) => !library.exclude_from_home),
        );
        const snapshot = `${libsSnapshot}|${[...fetches.keys()].join(",")}`;
        if (snapshot === lastSnapshot.current) return;
        const entries = await Promise.all(
          [...fetches].map(
            async ([key, fetch]) => [key, await fetch().catch((): LibraryItem[] => [])] as const,
          ),
        );
        if (seq !== reloadSeq.current) return;
        lastSnapshot.current = snapshot;
        setItemsByKey(new Map(entries));
      })
      // 瞬时失败不清已有数据：failed 只决定提示条，卡片继续用上一份快照，
      // 下一轮轮询成功即自动恢复（整页错误屏只留给一次都没加载成功的情况）
      .catch(() => {
        if (seq === reloadSeq.current) setFailed(true);
      });
  }, [homePrefs]);

  useEffect(() => {
    reloadRef.current = reload;
    (window as any).__CM_RELOAD_HOME__ = () => {
      lastLoadedHome = null;
      reload();
    };
    reload();
    return () => {
      delete (window as any).__CM_RELOAD_HOME__;
    };
  }, [reload]);

  // 成功到手的数据随手更新模块级快照，供下次重挂载首帧直出（见 lastLoadedHome）。
  // 只在 libraries 已加载时写：加载态/失败态不该顶掉上一份好数据。
  useEffect(() => {
    if (libraries === null) return;
    lastLoadedHome = {
      libraries,
      upNext: upNext ?? [],
      favorites: favorites ?? { items: [], total: 0 },
      itemsByKey,
    };
  }, [libraries, upNext, favorites, itemsByKey]);

  // 新契约无扫描/整理/刷新等实时状态，空闲低频轮询兜底即可（30 秒）
  useVisiblePolling(reload, 30_000);

  // 新契约无可见范围（viewer_access 已删）：全部库都可浏览
  const visibleLibraries = useMemo(
    () => (libraries ?? []).filter((library) => !library.exclude_from_home),
    [libraries],
  );

  // 首页 = 行清单：存过的按存的顺序，没存过的内置行与每库默认行补在后面
  // （规则见 lib/home-rows.ts）。这里再合并一次是为了渲染，与 reload 里取数
  // 用的是同一个纯函数，不会出现"取了 A 行、画了 B 行"
  const rows = useMemo(
    () => buildHomeRows(homePrefs, toHomeLibraryLike(libraries ?? []), []),
    [homePrefs, libraries],
  );
  const visibleRows = useMemo(() => rows.filter((row) => !row.hidden), [rows]);

  // 「我的收藏」横滚行：与库行同一张海报卡、同一个行组件，只把 hover
  // 层换成收藏的层级说明；落点是服务端解析好的可见库里的条目详情
  const favoriteRow = useMemo(() => {
    const items = favorites?.items ?? [];
    const hrefs = new Map(
      items.map((it) => [
        libraryItemKey(it),
        `/library/${it.library_id}/item/${it.media_item_id}`,
      ]),
    );
    return {
      items: items.map(favoriteItemToMediaItem),
      hrefOf: (m: MediaItem) => hrefs.get(m.id),
    };
  }, [favorites]);

  /** 库行 / 合集行：服务端已按这一行的排序给到前 20，复用发现页的横滚海报行。
   *  已在库的条目点击进**媒体库条目详情**（本地刮削信息 + 片源规格 + 条目操作），
   *  与单库页库存格同一目标。只呈现入库上下文，订阅/补齐操作留在单库页。 */
  const contentRow = (row: HomeRow, moreHref: string) => {
    const items = itemsByKey.get(rowFetchKey(row)) ?? [];
    // 空行整段隐藏：偏好决定「想不想看」，数据决定「有没有」。
    // 「未看优先」的行不会因为"整库都看过"而空——那一步由服务端回退到全部
    // （见 lib/home-row-items.ts 的说明）；真空只可能是空库、或「最近观看」没播过。
    if (items.length === 0) return null;
    const fallbackLibrary = row.kind === "library" ? row.library.id : null;
    const hrefs = new Map(
      items.map((it) => [
        libraryItemKey(it),
        `/library/${it.library_id ?? fallbackLibrary ?? ""}/item/${it.media_item_id}`,
      ]),
    );
    return (
      <div key={row.id} className="mt-8 max-md:mt-6" data-testid={`home-row-${row.id}`}>
        <MediaRow
          row={{
            id: `home-${row.id}`,
            title: rowTitle(row),
            items: items.map(libraryItemToMediaItem),
          }}
          moreHref={moreHref}
          moreLabel="查看全部"
          cardAction="none"
          cardHref={(m) => hrefs.get(m.id)}
          cardRevealInfoOnTouch
        />
      </div>
    );
  };

  const renderRow = (row: HomeRow) => {
    switch (row.kind) {
      case "up-next":
        // 当前账号跨可见库聚合的播放状态；空列表时组件整段隐藏。
        // 清空观看记录的入口就在这一行的标题右侧，清完重新拉一次数据。
        return (
          <UpNextRow key={row.id} items={upNext} libraries={visibleLibraries} onCleared={reload} />
        );
      case "favorites":
        // 只横滚最近收藏的 20 部（网页与 Jellyfin 客户端点的心同一份），「查看全部」
        // 进与单库页同一套海报墙的 /library/favorites；没有收藏时整段隐藏
        if (favoriteRow.items.length === 0) return null;
        return (
          <div key={row.id} className="mt-8 max-md:mt-6" data-testid="favorites-row">
            <MediaRow
              row={{ id: "favorites", title: rowTitle(row), items: favoriteRow.items }}
              moreHref={"/library/favorites"}
              moreLabel={`查看全部 ${favorites?.total ?? 0} 部`}
              cardAction="none"
              cardHref={favoriteRow.hrefOf}
              cardRevealInfoOnTouch
            />
          </div>
        );
      case "libraries":
        // 库卡片横排：库多了不换行堆高，改为一行横滚（与库行同一交互）
        if (visibleLibraries.length === 0) return null;
        return (
          <section key={row.id} className="mt-8 max-md:mt-6" aria-labelledby="my-libraries-title">
            <div className="flex items-center justify-between gap-4 px-6 max-md:px-4">
              <h3
                id="my-libraries-title"
                className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]"
              >
                {rowTitle(row)}
              </h3>
            </div>
            <HScroller className="mt-3 gap-5 px-6 pb-1 pt-1 max-md:gap-3.5 max-md:px-4">
              {visibleLibraries.map((library) => (
                <div
                  key={library.id}
                  data-library-card={library.id}
                  className="w-[268px] shrink-0 rounded-2xl max-md:w-[230px]"
                >
                  <LibraryCard
                    library={library}
                    items={itemsByKey.get(coverFetchKey(library.id)) ?? []}
                  />
                </div>
              ))}
            </HScroller>
          </section>
        );
      case "library":
        return contentRow(row, `/library/${row.library.id}`);
      case "collection":
        // 合集端点已删，合集行不会再出现；保留分支只为类型收口
        return null;
    }
  };

  return (
    <div ref={scrollRef} className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
      {/* Netflix 主题的全出血 Billboard（原内容首页并入，见 library-hero.tsx）：
          挂在滚动容器内、跟随页面一起滚走，页头与行清单依次排在其后 */}
      {hero}
      {/* 页头：标题 + 统计，右侧是页面级操作「自定义首页」「管理媒体库」（SaaS 惯例：
          页面动作放标题行右端；分区标题行只留分区自己的东西）。首页上没有任何
          排序细节与行菜单——调整全部收进自定义页，首页只负责看 */}
      <div className="flex items-start justify-between gap-4 px-6 pt-7 max-md:px-4 max-md:pt-4">
        <div className="min-w-0">
          <h2 className="text-on-image text-[26px] font-bold leading-tight tracking-[-0.02em] text-white max-md:text-[21px]">
            媒体库
          </h2>
          <p className="text-on-image mt-1.5 text-ui text-[var(--text-muted)] max-md:mt-1 max-md:line-clamp-2 max-md:text-sub">
            {failed && libraries === null
              ? "暂时无法获取媒体库统计，正在自动重试"
              : libraryStatsSummary(libraries === null ? null : visibleLibraries)}
          </p>
        </div>
        {/* 两个页面级动作都是图标钮：自定义首页（所有人）、管理媒体库（有权限的人） */}
        <div className="flex shrink-0 items-center gap-2">
          <Link to={"/library/customize"}
            aria-label="自定义首页"
            title="自定义首页"
            className="btn-glass mt-1 grid size-8 shrink-0 place-items-center !p-0 max-md:mt-0"
          >
            <ListIcon className="size-4" />
          </Link>
          {canManageLibraries && (
            <Link to={"/library/manage"}
              aria-label="管理媒体库"
              title="管理媒体库"
              className="btn-glass mt-1 grid size-8 shrink-0 place-items-center !p-0 max-md:mt-0"
            >
              <GearIcon className="size-4" />
            </Link>
          )}
        </div>
      </div>

      {libraries === null && !failed && (
        <div className="mt-16 flex items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在加载媒体库…
        </div>
      )}

      {/* 只有一次都没加载成功过才整页报错；已有数据在手时，瞬时失败只挂
          提示条（stale-while-error），卡片照常展示上一份快照 */}
      {failed && libraries === null && (
        <div className="mt-16 flex flex-col items-center gap-3 text-center">
          <p className="text-ui text-[var(--text-muted)]">媒体库加载失败</p>
          <button
            type="button"
            onClick={reload}
            className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            重试
          </button>
        </div>
      )}

      {failed && libraries !== null && (
        <div className="mx-6 mt-4 rounded-xl border border-amber-400/25 bg-amber-500/10 px-4 py-3 text-sub text-amber-200 max-md:mx-4">
          与后端通信失败，正在自动重试；下方显示的是最近一次成功加载的数据
        </div>
      )}

      {libraries !== null && libraries.length === 0 && (
        <ContentEmptyState
          variant="library"
          title={canManageLibraries ? "为收藏准备一个家" : "还没有可浏览的媒体库"}
          description={
            canManageLibraries
              ? "创建电影库或剧集库，选好根目录后，订阅完成的内容会自动整理到这里。"
              : "当前账号暂时没有可浏览的媒体库，请联系管理员分配媒体库权限。"
          }
          action={
            canManageLibraries ? (
              <Link to={"/library/manage?create=1"}
                className="btn-accent flex items-center gap-1 rounded-full py-2 pl-3 pr-4 text-ui font-semibold"
              >
                <PlusIcon className="size-4" />
                创建第一个媒体库
              </Link>
            ) : undefined
          }
        />
      )}

      {/* 全部藏光时不出白页：给一个指回自定义页的空态 */}
      {libraries !== null && libraries.length > 0 && visibleRows.length === 0 && (
        <div
          className="mx-6 mt-16 rounded-2xl border border-dashed border-white/15 px-6 py-8 text-center max-md:mx-4"
          data-testid="home-all-hidden"
        >
          <p className="text-ui font-semibold text-[var(--text)]">首页空空如也</p>
          <p className="mt-1 text-sub text-[var(--text-muted)]">
            所有行都被隐藏了。到「自定义首页」挑几行回来，或恢复默认。
          </p>
          <Link to={"/library/customize"}
            className="btn-glass mt-4 inline-flex px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            自定义首页
          </Link>
        </div>
      )}

      {libraries !== null && visibleRows.map(renderRow)}
    </div>
  );
}

/**
 * 显示中的行各自要打的请求，按缓存键去重。排序、截断与「未看优先」的回退都交给
 * 服务端——早先在这里拉整库再本地切片，一个几千部的库光这一个请求就是几百 KB；
 * 回退做在服务端也才可能对 Jellyfin 那个面生效（见 lib/home-row-items.ts）。
 */
function rowFetches(
  rows: HomeRow[],
  libraries: MediaLibrary[],
): Map<string, () => Promise<LibraryItem[]>> {
  const fetches = new Map<string, () => Promise<LibraryItem[]>>();
  for (const row of rows) {
    if (row.kind === "library") {
      fetches.set(rowFetchKey(row), () =>
        listLibraryItems(String(row.library.id), rowItemQuery(row, RECENT_COUNT)),
      );
    } else if (row.kind === "libraries") {
      for (const library of libraries) {
        const key = coverFetchKey(library.id);
        if (!fetches.has(key)) {
          fetches.set(key, () =>
            listLibraryItems(library.id, { sort: "added_at", limit: RECENT_COUNT }),
          );
        }
      }
    }
  }
  return fetches;
}

/**
 * 库存条目 → 发现页海报卡的数据形态。点击走 /media/{type}/{tmdb_id} 详情
 * （与单库页库存格同一目标）。卡片底部只留片名与年份；本批季集范围和入库
 * 时间进入 hover，不能拿累计库存季集数冒充新增内容。海报不打清晰度徽章。
 */
/** 海报卡的 id：TMDB 条目用 tmdb_id（订阅状态按它对齐），本地条目没有外部 id，
 *  用带前缀的条目 id 占位——只用来当 Map 键与 React key，不会被当成 TMDB id 请求。 */
function libraryItemKey(item: { media_item_id: string }): string {
  // 新契约无 tmdb_id：一律用带前缀的条目 id 占位（只做 Map 键与 React key）
  return `local:${item.media_item_id}`;
}

/** 主图宽高比：新契约无 primary_aspect，按类型推定（其他库横版抓帧 16:9，影视库竖版海报 2:3）。 */
function itemAspect(item: LibraryItem): number {
  return item.kind === "video" ? 16 / 9 : 2 / 3;
}

function libraryItemToMediaItem(item: LibraryItem): MediaItem {
  return {
    id: libraryItemKey(item),
    source: "tmdb",
    // 其他库条目没有发现页类型；卡片只当本地内容展示，不给订阅入口
    type: item.kind === "video" ? "movie" : item.kind,
    title: item.title,
    originalTitle: "",
    year: item.year ?? 0,
    rating: 0,
    genres: [],
    extent: "",
    badges: [],
    overview: "",
    // 新契约无最近入库/入库时间字段，hover 层不再给「X 入库」副文案
    // 海报可能是本地刮削资产的相对路径（/images/assets/...），也可能是
    // TMDB 图床绝对地址——统一经 imageUrl 解析（补 API base / 走缓存代理）。
    // 取 poster-card 派生图而非原图：格子实测渲染 150~170 CSS px，328px 的
    // 预设覆盖 2x 屏绰绰有余，而原图是 500px 宽的刮削资产——一屏 60 格直出
    // 原图要 4.9 MB，取派生图只要 1.7 MB（实测单张 82KB → 29KB）
    posterUrl: imageUrl(item.poster_url, cardVariantFor(itemAspect(item))),
  };
}

/** 收藏卡：海报卡形态同库存条目，hover 层只说「收藏的是哪一层」（整剧与电影不解释）。 */
function favoriteItemToMediaItem(item: FavoriteItem): MediaItem {
  const level = favoriteLevelLabel(
    item.kind,
    item.favorite_season_number,
    item.favorite_episode_number,
  );
  return {
    // FavoriteItem 的 library_id 在 lib/api/playback 里仍按旧 number 声明，
    // 与新 LibraryItem 的 string 不一致——这里只借用库存字段，断言到 LibraryItem
    ...libraryItemToMediaItem(item as unknown as LibraryItem),
    overlayDetails: level ? { primary: level } : undefined,
  };
}

/* —— 库卡片：海报货架封面 + 库名/徽标/计数，Emby「我的媒体」磁贴风 —— */

function LibraryCard({ library, items }: { library: MediaLibrary; items: LibraryItem[] }) {
  // 封面横图优先取库自己的 cover_url，备选库内首部作品的 backdrop_url（fanart）
  const backdropUrl = items.find((s) => Boolean(s.backdrop_url))?.backdrop_url ?? null;

  return (
    <div className="group/lib relative">
      <Link
        to={`/library/${library.id}`}
        aria-label={`打开「${library.name}」`}
        className="block overflow-hidden rounded-2xl ring-1 ring-white/10 outline-none transition duration-300 hover:ring-white/35 focus-visible:ring-2 focus-visible:ring-[var(--accent-ring)]"
      >
        <div className="relative aspect-[21/10] bg-[#0a0c12]">
          <LibraryCover
            libraryId={library.id}
            kind={library.kind}
            coverUrl={library.cover_url ?? null}
            backdropUrl={backdropUrl}
          />
        </div>
      </Link>

      {/* 库名：居中展示，与「默认」共处一行 */}
      <div className="mt-2.5 flex items-center justify-center gap-2 px-2">
        <h3 className="truncate text-body-lg font-semibold text-white">{library.name}</h3>
        {library.is_default && (
          <span className="shrink-0 rounded-full border border-white/[0.14] bg-white/[0.1] px-2 py-0.5 text-micro font-semibold text-white/80">
            默认
          </span>
        )}
      </div>
    </div>
  );
}

/**
 * 封面「宽幅 Fanart 横卡」：
 * 1. 优先使用库定制封面（/libraries/{id}/cover，可能是用户自定义上传或系统自动选取的横版背景）；
 * 2. 备选作品列表里的 backdrop_url（fanart）全出血铺满；
 * 3. 严禁使用纵向 poster 居中模糊展示；若无横图，直接回退到优雅定制矢量封面。
 */
function LibraryCover({
  libraryId,
  kind,
  coverUrl,
  backdropUrl,
}: {
  libraryId: string;
  kind: MediaLibrary["kind"];
  coverUrl: string | null;
  backdropUrl: string | null;
}) {
  const [collageFailed, setCollageFailed] = useState(false);

  // 1. 如果媒体库有封面（或主动尝试加载 cover），优先展示
  const candidateUrl = coverUrl ? `${publicEnv.apiBaseUrl}${coverUrl}` : (collageFailed ? null : `${publicEnv.apiBaseUrl}/libraries/${libraryId}/cover`);
  if (candidateUrl && !collageFailed) {
    return (
      <div className="absolute inset-0 overflow-hidden">
        <img
          src={candidateUrl}
          alt=""
          loading="lazy"
          className="absolute inset-0 size-full object-cover transition duration-500 ease-out group-hover/lib:scale-[1.03]"
          onError={() => setCollageFailed(true)}
        />
        {/* 底部暗色渐变，衬托层次 */}
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/60 via-transparent to-transparent" />
        {/* 悬停扫光 */}
        <div className="pointer-events-none absolute -left-[45%] bottom-0 h-[30%] w-[45%] -skew-x-12 bg-gradient-to-r from-transparent via-white/[0.12] to-transparent transition-transform duration-700 ease-out group-hover/lib:translate-x-[350%]" />
      </div>
    );
  }

  // 2. 方案 A：使用横版 Fanart 剧照全出血铺满长方形卡片
  if (backdropUrl) {
    return (
      <div className="absolute inset-0 overflow-hidden bg-[#0a0c12]">
        <img
          src={imageUrl(backdropUrl, "landscape-card")}
          alt=""
          loading="lazy"
          className="absolute inset-0 size-full object-cover transition duration-500 ease-out group-hover/lib:scale-[1.04]"
        />
        {/* 细微内阴影与底部渐暗，提升层次与质感 */}
        <div className="pointer-events-none absolute inset-0 bg-gradient-to-t from-black/70 via-black/10 to-transparent" />
        {/* 悬停扫光 */}
        <div className="pointer-events-none absolute -left-[45%] bottom-0 h-[30%] w-[45%] -skew-x-12 bg-gradient-to-r from-transparent via-white/[0.15] to-transparent transition-transform duration-700 ease-out group-hover/lib:translate-x-[350%]" />
      </div>
    );
  }

  // 3. 空库或无横图：高清定制矢量封面（21:10 电影/剧集专属画卷）
  return (
    <div className="absolute inset-0 overflow-hidden">
      <LibraryKindCoverArt kind={kind} className="size-full transition duration-500 ease-out group-hover/lib:scale-[1.03]" />
    </div>
  );
}
