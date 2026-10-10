"use client";

import type { ReactNode } from "react";
import { Children, Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { Link } from "react-router-dom";
import { useNavigate } from "react-router-dom";

import { shouldPollProbeDetails } from "@/lib/probe-status";
import { ArtworkPickerDialog } from "@/components/artwork-picker-dialog";
import { BrandLoader } from "@/components/brand-loader";
import { CastRow, type CastRowPerson } from "@/components/cast-row";
import { CollectionsAddButton } from "@/components/collections-add-button";
import { MediaRow } from "@/components/media-row";
import { ChapterStrip } from "@/components/chapter-strip";
import { MediaTrackRows } from "@/components/media-track-rows";
import { NetflixBackButton, NetflixPageActions } from "@/components/netflix/back-button";
import { PAGE_NAV_BUTTON_CLASS, PageNav } from "@/components/page-nav";
import { HScroller } from "@/components/h-scroller";
import {
  ArrowLeftIcon,
  CheckIcon,
  ChevronRightIcon,
  FolderIcon,
  HeartIcon,
  MoreIcon,
  PlayIcon,
  TrashIcon,
} from "@/components/icons";
import { useConfirm, useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import { PosterImage } from "@/components/poster-image";
import { playHref, rememberPlayerReturnPath } from "@/lib/player/play-links";
import { ReidentifyDialog } from "@/components/reidentify-dialog";
import { Tooltip } from "@/components/tooltip";
import {
  type ItemChapter,
  fetchItemProbeStatus,
  type ItemDeleteResult,
  type LibraryEpisode,
  fetchItemChapters,
  fetchItemSimilar,
  generateItemChapters,
  probeItem,
  refreshItemChapters,
  type LibraryItemDetail,
  type LibraryItemFile,
  type MediaLibrary,
  type SeasonEpisodes,
  deleteLibraryItem,
  getItemEpisodes,
  getLibrary,
  getLibraryItemDetail,
  listLibraries,
  refreshItemMetadata,
} from "@/lib/api/libraries";
import {
  type PlaybackUnit,
  type PlaybackWatchState,
  fetchPlaybackMarks,
  fetchResumeState,
  clearPlaybackHistory,
  setPlaybackMarks,
} from "@/lib/api/playback";
import { useSubscribeEntry } from "@/components/subscribe-entry";
import { LIBRARY_KIND_LABELS, type MediaItem } from "@/lib/media-types";
import { getDiscoveryReturnPath } from "@/lib/discovery-return-path";
import { formatBytes, formatRuntimeMinutes, formatVideoResolution } from "@/lib/format";
import { formatClock } from "@/lib/player/timeline";
import { useBackNavigation } from "@/lib/back-navigation";
import { useBackdrop } from "@/lib/backdrop";
import { useIsMobile } from "@/lib/use-media-query";
import { resolveRequestUrl } from "@/lib/http";
import { cachedImageUrl } from "@/lib/image-proxy";
import { invalidateLibraryDetailSnapshot } from "@/lib/library-detail-snapshot";
import { refreshItemConfirm, rereadItemNfoConfirm } from "@/lib/library-confirm";
import { usePermissions } from "@/lib/permissions";
import { useTheme } from "@/lib/ui-prefs";
import { usePageTitle } from "@/lib/use-page-title";

/** 剧集详情页当前选中的分集上下文，供 Hero 与分集区共享同一份数据。
    文件类型是泛型：影片分享页（components/share/）复用分集区时文件视图更窄。 */
export interface SelectedEpisodeContext<F = LibraryItemFile> {
  seasonNumber: number;
  episode: LibraryEpisode;
  files: F[];
}

/** 分集区需要的条目最小形状（详情视图与分享页的访客视图都满足）。 */
export interface EpisodeSectionItem<F extends { file_id: string; season: number | null }> {
  media_item_id: string;
  files: F[];
}

/**
 * 媒体库条目详情页（/library/[id]/item/[mediaItemId]）——与发现页详情
 * （MediaDetailView，纯 TMDB 实时数据）职责不同：这里回答的是
 * 「**我拥有的这份拷贝**是什么」，全部信息来自本地刮削成果与文件本体：
 *
 *   1. 沉浸背景（全站背景临时换成本片剧照）优先条目目录里的 fanart（本地美术图接口），其次 TMDB 剧照；
 *   2. 简介 / 风格 / 片长 / 演职员来自条目目录的 NFO（TMM/Emby 刮削产物）；
 *   3. 片源规格来自 ffprobe 对文件本体的探测；基础信息展示当前文件的音轨 / 字幕，
 *      底部文件区按物理文件折叠尺寸 / 视频 / 码率，剧集只展示当前选中集；
 *   4. 底部文件区默认只列原始文件名，悬浮看物理路径——识别错了
 *      用户要能立刻知道"这是哪个文件"，同时不让技术信息压过影片本身；
 *   5. 条目级操作：重新识别（识别器升级后的翻案通道）与删除（唯一会
 *      真删磁盘的入口，整个刮削目录一起清，二次确认）。文件行与分集
 *      文件卡上另有单文件删除（多版本洗掉一个 / 删某集重下，同样二次
 *      确认；最后一个文件升级为整条目删除）。
 */
export function LibraryItemDetailView({
  libraryId,
  mediaItemId,
  returnTo,
  fromRecent = false,
  initialSeason,
  initialEpisode,
}: {
  libraryId: string;
  mediaItemId: string;
  returnTo?: string;
  /** 从媒体库首页最近观看进入：左上角返回首页，而不是条目所在的单库库存页。 */
  fromRecent?: boolean;
  initialSeason?: number;
  initialEpisode?: number;
}) {
  const { canManageLibraries, isAdmin } = usePermissions();
  // 洗版入口（quality-upgrade.md §13.3/§13.5）：有订阅并入既有订阅，无订阅走
  // 订阅弹层的洗版变体（库存季预填、建完自动接一轮洗版）
  const { canSubscribe, subscriptionOf, open: openSubscribe } = useSubscribeEntry();
  const navigate = useNavigate();
  const discoveryReturnPath = getDiscoveryReturnPath(returnTo);
  const confirm = useConfirm();
  const toast = useToast();
  const [detail, setDetail] = useState<LibraryItemDetail | null>(null);
  const [library, setLibrary] = useState<MediaLibrary | null>(null);
  const [failed, setFailed] = useState(false);
  // 「修正识别结果」弹窗：重走识别链出结论 → 用户拍板 → 才落库。
  // 拍板后不立刻重拉详情——文件全改挂走时本页会 404 翻成兜底态、把弹窗
  // 连同"✓ 已改挂为《X》"的回执一起卸掉，分裂成多组时更是没法接着处理
  // 剩下的组。改成记一个脏标记，关窗时再刷新
  // 播放键是 button 而不是 <Link>,react-router 不做路由包预取;SPA 下 /play
  // 的 JS 包已在首屏加载,起播无需预取这一步(原 Next 版 §6.10 起播链路)。
  useEffect(() => {}, [navigate, detail]);

  const [reidentifyOpen, setReidentifyOpen] = useState(false);
  const [chapters, setChapters] = useState<ItemChapter[]>([]);
  const [chaptersBusy, setChaptersBusy] = useState(false);
  const [chapterError, setChapterError] = useState<string | null>(null);
  const chapterRefreshTimer = useRef<number | null>(null);
  const chapterRefreshToken = useRef(0);
  const [similar, setSimilar] = useState<MediaItem[]>([]);
  const [reidentifyDirty, setReidentifyDirty] = useState(false);
  // 元数据刷新的失败提示（原先与重识别共用一条横幅）
  const [refreshError, setRefreshError] = useState<string | null>(null);
  // 元数据刷新：**进行中的状态以服务端 detail.scraping 为准**（离开页面、
  // 刷新浏览器、换设备打开都能接着看到）；这个本地态只覆盖"点击到接口
  // 返回"这一小段，避免按钮闪一下没反应
  const [kicking, setKicking] = useState(false);
  // 「更换图片」弹层（手动选海报/背景，选后加锁）
  const [artworkOpen, setArtworkOpen] = useState(false);
  // 删除确认弹窗
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 单文件删除确认弹窗（非 null 即打开；多版本洗版 / 删某集重下的入口）
  const [deleteFileTarget, setDeleteFileTarget] = useState<LibraryItemFile | null>(null);
  // 剧集详情的 Hero 与分集区必须指向同一集；电影保持为 null。
  const [selectedSeriesEpisode, setSelectedSeriesEpisode] =
    useState<SelectedEpisodeContext | null>(null);
  // 顶部文件选择器由父级控制，分辨率才能与音轨、字幕同步切换。
  const [selectedTrackFileId, setSelectedTrackFileId] = useState<string | null>(null);
  // 当前播放单元的观看记录（上次看到哪 / 是否看完）。播放按钮据此在
  // 「播放 / 继续观看 / 重新播放」之间切换——null 表示还没问到，按钮先按
  // 「播放」渲染，不为一次可能是全零的查询留骨架。
  const [watched, setWatched] = useState<PlaybackWatchState | null>(null);
  // 整个条目（电影 / 整部剧）的收藏态——与 Jellyfin 客户端在条目页点的心是
  // 同一份数据。null = 还没问到，心先按未收藏渲染。
  const [favorite, setFavorite] = useState<boolean | null>(null);
  // 心 / 对勾的请求进行中：防连点，两个按钮共用一把锁（同一行数据）
  const [marking, setMarking] = useState(false);
  // 在页面内改写了观看状态（对勾）之后让分集区静默重拉：分集卡的绿色对勾
  // 与 Hero 的「已看完」必须同步，但不能重置当前选中的集
  const [episodesVersion, setEpisodesVersion] = useState(0);
  const [probing, setProbing] = useState(false);

  /**
   * 播放入口指向的播放单元（media_item + 季集三元组，与播放器同一套约定）。
   * 电影用 (0, 0) 哨兵；剧集跟随分集区当前选中的那一集，选集变了续播点也要跟着变。
   */
  const playUnit = useMemo<PlaybackUnit | null>(() => {
    if (!detail) return null;
    if (detail.kind !== "tv") {
      // 电影与其他库条目都是单一播放单元 (0, 0)
      return {
        // lib/api/playback 的播放单元仍按 number 声明，条目 id 是 UUID 字符串，
        // 这里仅做类型投影，运行期原样传给播放器
        media_item_id: detail.media_item_id as unknown as number,
        season_number: 0,
        episode_number: 0,
      };
    }
    if (!selectedSeriesEpisode) return null;
    return {
      media_item_id: detail.media_item_id as unknown as number,
      season_number: selectedSeriesEpisode.seasonNumber,
      episode_number: selectedSeriesEpisode.episode.episode_number,
    };
  }, [detail, selectedSeriesEpisode]);

  // 单元三元组的字符串形式：detail 每次轮询都是新对象，直接依赖 playUnit
  // 会让刮削期间每两秒重查一次续播点。
  const playUnitKey = playUnit
    ? `${playUnit.media_item_id}/${playUnit.season_number}/${playUnit.episode_number}`
    : null;
  useEffect(() => {
    if (!playUnit) {
      setWatched(null);
      return;
    }
    let cancelled = false;
    setWatched(null);
    fetchResumeState(playUnit)
      .then((state) => {
        if (!cancelled) setWatched(state);
      })
      // 查不到续播点不影响起播，按钮退回「播放」即可，不打扰用户
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // playUnit 是对象字面量，按内容（三元组）比较
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playUnitKey]);

  useEffect(() => {
    let cancelled = false;
    setFavorite(null);
    fetchPlaybackMarks({ media_item_id: mediaItemId as unknown as number })
      .then((marks) => {
        if (!cancelled) setFavorite(marks.is_favorite);
      })
      // 查不到收藏态不影响浏览，心按未收藏渲染即可
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [mediaItemId]);

  /** 收藏 / 取消收藏整个条目：先翻按钮再请求，失败翻回来并提示。 */
  const toggleFavorite = useCallback(async () => {
    if (marking) return;
    const next = !(favorite ?? false);
    setMarking(true);
    setFavorite(next);
    try {
      const marks = await setPlaybackMarks(
        { media_item_id: mediaItemId as unknown as number },
        { favorite: next },
      );
      setFavorite(marks.is_favorite);
    } catch (e) {
      setFavorite(!next);
      toast.error(e instanceof Error ? e.message : "收藏失败，请稍后重试");
    } finally {
      setMarking(false);
    }
  }, [favorite, marking, mediaItemId, toast]);

  /**
   * 标记当前播放单元已看 / 未看（电影，或剧集当前选中的那一集）。写完重查一次
   * 续播点而不是本地改字段：标已看会清零续播位置、取消会清零播放次数，这些
   * 结论以服务端为准（与 Jellyfin 客户端点对勾的语义完全一致）。
   */
  const togglePlayed = useCallback(async () => {
    if (marking || !playUnit) return;
    const next = !(watched?.played ?? false);
    setMarking(true);
    try {
      await setPlaybackMarks(playUnit, { played: next });
      setWatched(await fetchResumeState(playUnit));
      setEpisodesVersion((v) => v + 1);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "标记失败，请稍后重试");
    } finally {
      setMarking(false);
    }
  }, [marking, playUnit, toast, watched]);

  const reload = useCallback(() => {
    setFailed(false);
    Promise.all([
      getLibraryItemDetail(libraryId, mediaItemId),
      getLibrary(libraryId).catch(() => null),
    ])
      .then(([data, lib]) => {
        setDetail(data);
        setLibrary(lib);
      })
      .catch(() => setFailed(true));
  }, [libraryId, mediaItemId]);

  useEffect(() => {
    setDetail(null);
    setRefreshError(null);
    setSelectedSeriesEpisode(null);
    setSelectedTrackFileId(null);
    reload();
  }, [reload]);

  useEffect(() => () => {
    chapterRefreshToken.current += 1;
    if (chapterRefreshTimer.current != null) {
      window.clearTimeout(chapterRefreshTimer.current);
      chapterRefreshTimer.current = null;
    }
  }, []);

  // 后台任务状态存储在数据库。即使已有章节，也持续刷新详情直到探测任务终态，
  // 这样媒体流信息和声纹结果会在任务完成后及时显示。
  useEffect(() => {
    if (!detail || detail.files.length === 0) return;
    const stages = detail.files.flatMap((file) =>
      file.probe_stages
        ? [file.probe_stages.metadata, file.probe_stages.intro, file.probe_stages.outro].filter(
            (stage): stage is NonNullable<typeof stage> => stage != null,
          )
        : [],
    );
    const active = stages.length > 0
      ? shouldPollProbeDetails(stages)
      : detail.files.some((file) => file.probe_queued);
    if (!active) return;
    const timer = window.setInterval(() => {
      reload();
    }, 5000);
    return () => window.clearInterval(timer);
  }, [detail, reload]);

  // Reattach to a persisted season refresh after navigation or browser reload.
  // The click handler also reports immediate progress, while this effect owns
  // the longer-lived page state and reloads chapter results at a terminal state.
  const probeSeasonsKey = detail?.kind === "tv"
    ? [...new Set(detail.files.map((file) => file.season ?? 1))].sort((a, b) => a - b).join(",")
    : "";
  useEffect(() => {
    if (!detail || detail.kind !== "tv" || !probeSeasonsKey) return;
    let cancelled = false;
    let timer: number | null = null;
    const observedActive = new Set<number>();
    const seasons = probeSeasonsKey.split(",").map(Number);
    const poll = async () => {
      let stillActive = false;
      for (const season of seasons) {
        try {
          const status = await fetchItemProbeStatus(libraryId, detail.media_item_id, season);
          if (status.active) {
            observedActive.add(season);
            stillActive = true;
            continue;
          }
          const wasActive = observedActive.delete(season);
          const selected = selectedSeriesEpisode;
          if (
            (wasActive || status.job?.status === "succeeded" || status.job?.status === "failed" || status.job?.status === "cancelled")
            && selected?.seasonNumber === season
          ) {
            const target = {
              season,
              episode: selected.episode.episode_number,
            };
            const latestChapters = await fetchItemChapters(libraryId, detail.media_item_id, target);
            if (!cancelled) setChapters(latestChapters);
          }
        } catch {
          // Retry transient status failures instead of abandoning a persisted job.
          stillActive = true;
        }
      }
      if (!cancelled && stillActive) {
        timer = window.setTimeout(() => void poll(), 5000);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      if (timer != null) window.clearTimeout(timer);
    };
  }, [libraryId, detail?.media_item_id, detail?.kind, probeSeasonsKey, selectedSeriesEpisode]);

  // 沉浸背景：进入本页把全站背景临时换成该片剧照（侧栏、外壳留白一起透出，
  // 不再只铺详情卡片的局部），离开即恢复用户配置的背景——见 lib/backdrop.tsx
  // 的 setOverrideBackdrop。没有横幅剧照时退回海报，覆盖层自己会铺满作氛围色。
  const { setOverrideBackdrop } = useBackdrop();
  const isMobile = useIsMobile();
  const isNfDesktop = useTheme().id === "netflix" && !isMobile;
  // Netflix 桌面的滚动退场（与发现详情页同一套）：挂 html.nf-hero-live 标记类，
  // 把滚动进度写进 --nf-hero-recede，globals.css 据此给沉浸覆盖层加渐暗 + 模糊、
  // 左侧纯黑遮罩护住上移后的标题。仅桌面启用——手机的剧照是页内 Hero
  // （showMobileHero），滚动容器铺黑已把全站背景层整个挡住，标记类无处生效。
  const scrollRef = useRef<HTMLDivElement>(null);
  const hasDetail = detail !== null;
  useEffect(() => {
    if (!isNfDesktop) return;
    const root = document.documentElement;
    root.classList.add("nf-hero-live");
    const el = scrollRef.current;
    if (!el) return () => {
      root.classList.remove("nf-hero-live");
      root.style.removeProperty("--nf-hero-recede");
    };
    let frame = 0;
    const sync = () => {
      frame = 0;
      const range = Math.max(320, el.clientHeight * 0.75);
      const progress = Math.min(1, Math.max(0, el.scrollTop / range));
      root.style.setProperty("--nf-hero-recede", progress.toFixed(3));
    };
    const onScroll = () => {
      if (!frame) frame = window.requestAnimationFrame(sync);
    };
    sync();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      el.removeEventListener("scroll", onScroll);
      if (frame) window.cancelAnimationFrame(frame);
      root.classList.remove("nf-hero-live");
      root.style.removeProperty("--nf-hero-recede");
    };
  }, [isNfDesktop, hasDetail]);
  // 沉浸背景与手机 Hero 的主图：优先条目目录的 fanart（本地美术图接口），
  // 其次海报（本地刮削资产 / TMDB 图床），经 imageUrl 解析补 API base。
  const immersiveUrl = imageUrl(detail?.backdrop_url ?? detail?.poster_url ?? null);
  // 手机也换全站背景，但页面本身不靠它显示：横版剧照铺满又高又窄的整屏只能按高度放大、
  // 从正中裁一条竖条，所以手机上看到的剧照是页内 Hero（mobileHeroSrc），滚动容器铺黑把
  // 全站背景整个挡住。仍然要换，是因为侧栏的液态玻璃折射的就是全站背景
  // （useBackdrop().backdrop）：不换的话展开侧栏透出的是用户自己的壁纸，与页面上的剧照
  // 断成两截。两处是同一个 URL，浏览器只下载一次
  useEffect(() => {
    if (!immersiveUrl) return;
    setOverrideBackdrop(immersiveUrl);
    return () => setOverrideBackdrop(null);
  }, [immersiveUrl, setOverrideBackdrop]);
  // 手机 Hero 用剧照，与桌面同一张：海报在外面的海报墙上已经看过了，进详情页要的是另一张
  // 画面。没有剧照时才退回主图（其他库的抓帧、极少数没刮到剧照的条目）
  const mobileHeroSrc = immersiveUrl;
  // Hero 的框比剧照高：宽度撑满、高约 1.15 倍宽（封顶 62svh，390px 宽的屏上约 448px）。
  // 横版剧照按高度铺满、上下不裁，左右裁掉两边、居中留下人物主体——比按宽度塞下整张
  // （只有 219px 高）多一倍画面，又不像铺满整屏那样只剩中间一条
  const mobileHeroHeight = "min(115vw, 62svh)";
  const showMobileHero = isMobile && mobileHeroSrc !== "";

  usePageTitle(detail?.title);
  // 新契约的详情无刮削状态 / 章节生成状态（scraping / chapters_pending 已删），
  // 刷新中状态不再轮询；kicking 保留给「点击到接口返回」这一小段
  const scrapingNow = kicking;

  // 兜底态（加载中/失败）的顶栏：条目标题未知，末项留空——渲染 PageNav 是为了
  // 向外壳登记「本页自带顶栏」，否则移动端全局顶栏（☰ + logo）会先显示再消失，
  // 顶部闪一下；同时转圈期间就有返回键可点。加载态与正文共用同一兜底目标。
  const navFallback = discoveryReturnPath
    ? { label: "发现详情", href: discoveryReturnPath }
    : fromRecent
      ? { label: "媒体库", href: "/library" }
      : { label: library?.name ?? "库存", href: `/library/${libraryId}` };
  // Netflix 桌面的顶栏（fixed z-40）会把 PageNav（sticky z-30）整个盖住——
  // 返回键与 ⋯ 菜单都点不到（发现详情页踩过并修过的同款问题）；该形态下
  // 退役 PageNav，改用悬浮返回键 + 页面操作簇（见 media-detail-view 的
  // hidePageNav 分支，银玻璃与移动端仍走 PageNav）。
  const hidePageNav = isNfDesktop;
  const back = useBackNavigation(navFallback.href);

  useEffect(() => {
    if (!detail || detail.files.length === 0) return;
    if (detail.kind === "tv" && !selectedSeriesEpisode) {
      setChapters([]);
      return;
    }
    let cancelled = false;
    const target = detail.kind === "tv" && selectedSeriesEpisode
      ? { season: selectedSeriesEpisode.seasonNumber, episode: selectedSeriesEpisode.episode.episode_number }
      : undefined;
    void fetchItemChapters(libraryId, detail.media_item_id, target)
      .then((list) => {
        if (!cancelled && list.length > 0) setChapters(list);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [libraryId, detail?.media_item_id, selectedSeriesEpisode?.seasonNumber, selectedSeriesEpisode?.episode.episode_number]);

  // 相似推荐：仅依赖 tmdb_id 和 media_item_id，一次加载终身缓存，绝不随详情轮询重复请求导致页面闪跳
  const mediaTmdbId = detail?.tmdb_id;
  const mediaItemIdKey = detail?.media_item_id;
  useEffect(() => {
    if (!mediaTmdbId || !mediaItemIdKey) return;
    let cancelled = false;
    void fetchItemSimilar(libraryId, mediaItemIdKey)
      .then((hits) => {
        if (cancelled) return;
        setSimilar(
          hits.map((hit) => ({
            id: hit.tmdb_id ?? hit.id ?? "",
            title: hit.title,
            type: hit.kind === "tv" ? "tv" : "movie",
            year: hit.year ?? undefined,
            posterUrl: hit.poster_url ? imageUrl(hit.poster_url) : null,
            href: hit.tmdb_id
              ? `/media/${hit.kind === "tv" ? "tv" : "movie"}/${hit.tmdb_id}`
              : undefined,
          }) as unknown as MediaItem),
        );
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [libraryId, mediaTmdbId, mediaItemIdKey]);

  if (failed) {
    return (
      // ambient-fallback：同 MediaDetailView——本页豁免全局蒙版，兜底态没有沉浸
      // 背景可铺，文案会压在用户壁纸上，自己带一层底才读得清
      <div className="ambient-fallback flex h-full flex-col">
        {!hidePageNav && <PageNav title="" fallback={navFallback} />}
        {hidePageNav && <NetflixBackButton onBack={back} />}
        <div className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center">
          <p className="text-body-lg font-semibold text-[var(--text)]">未能加载该条目</p>
          <p className="max-w-sm text-ui leading-6 text-[var(--text-muted)]">
            条目可能已被删除或重新识别为其他作品，请返回后查看。
          </p>
          <Link to={navFallback.href}
            replace
            className="btn-glass flex items-center gap-2 px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            <ArrowLeftIcon className="size-4" />
            返回{navFallback.label}
          </Link>
        </div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="ambient-fallback flex h-full flex-col">
        {!hidePageNav && <PageNav title="" fallback={navFallback} />}
        {hidePageNav && <NetflixBackButton onBack={back} />}
        <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在读取本地刮削信息…
        </div>
      </div>
    );
  }

  // 「非剧集」= 单文件叶子：电影与其他库条目的分集区、文件区、播放单元都同一套
  const isMovie = detail.kind !== "tv";
  // 有 TMDB 锚点才有订阅/洗版/外链；本地条目（其他库、影视库里未识别的文件）没有
  const tmdbId = detail.tmdb_id;
  // 条目所在库有刮削链（影视库）才给「刷新元数据/更换图片」等入口；
  // 新契约的能力位字段已删，按库形态判定
  const scrapedLibrary = library != null && (library.kind === "movie" || library.kind === "tv");
  const trackFiles = isMovie
    ? detail.files
    : (selectedSeriesEpisode?.files ?? []);
  // 新契约的详情文件没有 state 字段：返回的文件都是可播在位文件
  const availableTrackFiles = trackFiles;
  const selectedTrackFile =
    availableTrackFiles.find((file) => file.file_id === selectedTrackFileId) ??
    availableTrackFiles[0] ??
    null;
  // 电影和剧集共用同一套基础信息层级：标题行只保留年份、片长、分辨率、HDR；
  // 帧率、编码、文件大小等技术细节留在文件区。电影概览全部版本，
  // 剧集只显示当前集当前文件的画面规格，并随文件选择器同步变化。
  const resolutionSources = isMovie
    ? detail.files
    : selectedTrackFile
      ? [selectedTrackFile]
      : [];
  const itemResolutions = [
    ...new Set(
      resolutionSources
        .map((file) => fileResolutionLabel(file))
        .filter((resolution): resolution is string => Boolean(resolution)),
    ),
  ];
  const itemHdrFormats = [
    ...new Set(
      resolutionSources.map((file) => file.hdr).filter((hdr): hdr is string => Boolean(hdr)),
    ),
  ].sort((a, b) => {
    const priority = ["Dolby Vision", "HDR10+", "HDR10", "HLG", "HDR"];
    const aIndex = priority.indexOf(a);
    const bIndex = priority.indexOf(b);
    return (aIndex < 0 ? priority.length : aIndex) -
      (bIndex < 0 ? priority.length : bIndex);
  });
  const itemFacts = [
    detail.year ? String(detail.year) : null,
    detail.rating ? `评分 ${detail.rating}` : null,
    detail.runtime_minutes ? `${detail.runtime_minutes} 分钟` : null,
  ].filter((fact): fact is string => Boolean(fact));
  const itemPlot =
    detail.overview ??
    (isMovie ? undefined : (selectedSeriesEpisode?.episode.overview ?? undefined));

  const runMetadataRefresh = async () => {
    // 重操作先确认：单条目刷新是 force 语义（图片覆盖重下），说清再动手
    // 其他库的视频条目：刷新是重读 NFO + 重新生成封面，不套 TMDB 那份文案
    const ask = detail?.kind === "video" ? rereadItemNfoConfirm : refreshItemConfirm;
    if (!(await confirm(ask(detail?.title ?? "此条目")))) return;
    setKicking(true);
    try {
      await refreshItemMetadata(libraryId, mediaItemId);
      // 立刻重拉一次详情：服务端的 scraping 标志会接管后续状态展示，
      // 前端不再盲等固定秒数（那种写法一旦离开页面状态就丢了）
      reload();
      // 后台任务在响应发出后才起跑，上面那次 reload 可能抢在 scraping
      // 标志立起之前拉到 false——稍后再拉一次兜底，否则界面毫无动静、
      // 轮询也不会启动，用户会以为没点上
      setTimeout(reload, 1500);
    } catch (err) {
      setRefreshError(err instanceof Error ? err.message : "元数据刷新失败，请稍后重试");
    } finally {
      setKicking(false);
    }
  };

  const regenerateChapters = async () => {
    setChaptersBusy(true);
    try {
      const target = !isMovie && selectedSeriesEpisode
        ? { season: selectedSeriesEpisode.seasonNumber, episode: selectedSeriesEpisode.episode.episode_number }
        : undefined;
      await generateItemChapters(libraryId, detail.media_item_id, target);
      setChapters(await fetchItemChapters(libraryId, detail.media_item_id, target));
    } catch (error) {
      setChapterError(error instanceof Error ? error.message : "章节生成失败");
    } finally {
      setChaptersBusy(false);
    }
  };

  const forceRefreshChaptersAndMarkers = async () => {
    if (chapterRefreshTimer.current != null) {
      window.clearTimeout(chapterRefreshTimer.current);
      chapterRefreshTimer.current = null;
    }
    const refreshToken = ++chapterRefreshToken.current;
    setChaptersBusy(true);
    try {
      const target = !isMovie && selectedSeriesEpisode
        ? { season: selectedSeriesEpisode.seasonNumber, episode: selectedSeriesEpisode.episode.episode_number }
        : undefined;
      const refreshed = await refreshItemChapters(libraryId, detail.media_item_id, target);
      if (chapterRefreshToken.current !== refreshToken) return;
      setChapterError(null);
      if (refreshed.fingerprint_refresh_error) {
        throw new Error(refreshed.fingerprint_refresh_error);
      }
      if (refreshed.fingerprint_refresh_already_running && !refreshed.fingerprint_refresh_queued) {
        setChapters((current) => current.length > 0 ? current : refreshed.chapters);
        toast.info("该剧该季已有探测任务正在运行，请等待完成后再刷新片头片尾");
      } else if (refreshed.fingerprint_refresh_queued) {
        setChapters((current) => current.length > 0 ? current : refreshed.chapters);
        toast.info(refreshed.fingerprint_refresh_already_running
          ? "该剧该季的片头片尾正在生成中"
          : "已提交片头片尾生成任务，当前结果会在完成后更新");
      }
      if (refreshed.fingerprint_refresh_queued || refreshed.fingerprint_refresh_already_running) {
        const poll = (attempt: number) => {
          if (chapterRefreshToken.current !== refreshToken) return;
          const delay = Math.min(5_000 * 2 ** Math.min(attempt, 3), 30_000);
          chapterRefreshTimer.current = window.setTimeout(async () => {
            if (chapterRefreshToken.current !== refreshToken) return;
            try {
              const latest = await fetchItemProbeStatus(
                libraryId,
                detail.media_item_id,
                target?.season ?? refreshed.fingerprint_refresh_job?.season ?? 1,
              );
              if (chapterRefreshToken.current !== refreshToken) return;
              if (latest.active) {
                void poll(attempt + 1);
                return;
              }
              if (latest.job?.status === "succeeded") {
                const updated = await fetchItemChapters(libraryId, detail.media_item_id, target);
                if (chapterRefreshToken.current !== refreshToken) return;
                setChapters(updated);
                chapterRefreshTimer.current = null;
                toast.success("片头片尾任务已完成，章节结果已更新");
                return;
              }
              if (latest.job?.status === "cancelled") {
                chapterRefreshTimer.current = null;
                toast.info("源文件已删除，片头片尾任务已取消，原有结果已保留");
                return;
              }
              if (latest.job?.status === "failed") {
                const updated = await fetchItemChapters(libraryId, detail.media_item_id, target);
                if (chapterRefreshToken.current !== refreshToken) return;
                setChapters(updated);
                chapterRefreshTimer.current = null;
                toast.error(latest.job.error
                  ? `片头片尾生成失败：${latest.job.error}`
                  : "片头片尾生成失败，原有章节结果已保留");
                return;
              }
              poll(attempt + 1);
            } catch {
              poll(attempt + 1);
            }
          }, delay);
        };
        poll(0);
      } else if (!refreshed.fingerprint_refresh_already_running) {
        setChapters(refreshed.chapters);
        toast.success("章节已刷新");
      }
    } catch (error) {
      setChapterError(error instanceof Error ? error.message : "重新获取片头与章节失败");
    } finally {
      setChaptersBusy(false);
    }
  };

  const handleProbeMedia = async () => {
    if (!detail) return;
    setProbing(true);
    try {
      const result = await probeItem(libraryId, detail.media_item_id);
      if (result.already_running) {
        toast.info("该条目已有媒体探测任务正在运行");
      } else {
        toast.success(`已提交 ${result.queued} 个文件的媒体信息与声纹探测`);
      }
      reload();
    } catch (error) {
      setRefreshError(error instanceof Error ? error.message : "重新探测媒体信息失败");
    } finally {
      setProbing(false);
    }
  };

  // 页面级操作（⋯ 菜单）：银玻璃/移动端排在 PageNav 吸顶行右端；Netflix 桌面
  // 没有 PageNav，由 NetflixPageActions 浮在顶栏下方右上角（与返回键对称）
  const itemActions = (
    <ItemActionsMenu
      canManage={canManageLibraries}
      onClearHistory={() => {
        void confirm({
          title: `清除《${detail.title}》的观看记录？`,
          description:
            "续播进度、已看标记和播放次数都会清除，无法恢复。只影响你自己的记录。",
          confirmLabel: "清除",
          tone: "danger",
        }).then((ok) => {
          if (!ok) return;
          clearPlaybackHistory("item", {
            mediaItemId: detail.media_item_id as unknown as number,
          })
            .then(({ message }) => toast.success(message))
            .catch((e) => toast.error((e as Error).message));
        });
      }}
      identifiable={scrapedLibrary}
      scraped={scrapedLibrary && detail.tmdb_id != null}
      readsNfo={detail.kind === "video"}
      scraping={scrapingNow}
      searchHref={`/search?q=${encodeURIComponent(detail.title)}`}
      onReidentify={() => setReidentifyOpen(true)}
      onRefreshMetadata={runMetadataRefresh}
      onRefreshChapters={detail.files.length > 0 ? () => void forceRefreshChaptersAndMarkers() : undefined}
      onChangeArtwork={() => setArtworkOpen(true)}

      onDelete={() => setDeleteOpen(true)}
      // 未识别/本地条目没有订阅锚点，不给洗版入口
      onUpgrade={
        canSubscribe && tmdbId != null && detail.kind !== "video"
          ? () => {
              const existing = subscriptionOf({
                id: String(tmdbId),
                type: detail.kind === "tv" ? "tv" : "movie",
              });
              if (existing) {
                // 一部影片只有一个订阅：并入既有订阅，跳详情直接开弹层
                navigate(`/subscriptions/${existing.id}?upgrade-run=1`,);
                return;
              }
              void openSubscribe(
                {
                  id: String(tmdbId),
                  title: detail.title,
                  rating: 0,
                  posterUrl: "",
                  type: detail.kind === "tv" ? "tv" : "movie",
                  year: detail.year ?? undefined,
                },
                { upgradeIntent: true },
              );
            }
          : undefined
      }
    />
  );

  return (
    // rounded-2xl + overflow 裁切：顶部剧照渐变到纯黑内容板，方角
    // 会与全站"浮起圆角卡片"的形状语言冲突——按侧栏同规格圆角收尾。
    // max-md:rounded-none：这套圆角只在桌面成立——桌面外壳有 p-3.5 的留白，
    // 圆角落在留白里、背景大图从四周透出，才是一张"浮起的卡片"。窄屏是通栏
    // 满屏布局，没有留白，圆角直接压在屏幕边上：顶栏的吸顶雾层被这层
    // overflow 一裁，就成了贴在屏幕顶上的一块圆角色块（手机上肉眼可见的
    // 两个缺角），底边同理被 Home 指示条切掉。手机上一律方角、真通栏。
    <div
      ref={scrollRef}
      className={`detail-ambient scroll-thin scroll-safe relative isolate h-full overflow-y-auto rounded-2xl max-md:rounded-none ${
        showMobileHero ? "detail-ambient--hero" : ""
      }`}
    >
      {/* 没有任何 Hero 图层：全站背景此刻就是本片剧照（沉浸覆盖 + 本页豁免
          全局蒙版，见 app-shell 的 isHome），大图直出、零边界；.detail-ambient
          在滚动容器上铺「透明 → 纯黑」的渐变板托住下方内容（见 globals.css）。
          顶栏首屏只有返回键与操作入口浮在剧照上。 */}
      {hidePageNav ? (
        <>
          <NetflixBackButton onBack={back} />
          <NetflixPageActions>{itemActions}</NetflixPageActions>
        </>
      ) : (
        <PageNav title={detail.title} fallback={navFallback} actions={itemActions} />
      )}

      {/* 手机 Hero（剧照）：宽度撑满，从状态栏底下起铺（绝对定位在滚动内容顶端，PageNav
          的返回键与吸顶雾层浮在它上面），随内容一起滚走，不固定在背景上。顶部一抹暗让状态栏
          与返回键落在亮图上也读得清；底部从中段开始压暗，到底边落成纯黑，与下方黑底无缝接上 */}
      {showMobileHero && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-x-0 top-0 z-0 overflow-hidden"
          style={{ height: mobileHeroHeight }}
        >
          <img src={mobileHeroSrc} alt="" decoding="async" className="size-full object-cover object-center" />
          <div className="absolute inset-x-0 top-0 h-28 bg-gradient-to-b from-black/45 to-transparent" />
          <div className="absolute inset-x-0 bottom-0 h-[55%] bg-gradient-to-b from-transparent via-black/55 to-black" />
        </div>
      )}

      {/* 氛围留白：这一段什么都不放，让剧照完整呼吸。手机有 Hero 时 = Hero 高度减去吸顶
          顶栏的占位（52px + 安全区）与片名压进图里的那一截（150px），片名与信息落在剧照
          底部的渐变上；视口很矮时减到负数就不留白 */}
      <div
        className={showMobileHero ? undefined : "h-[var(--detail-hero-h)] min-h-[var(--detail-hero-min-h)]"}
        style={
          showMobileHero
            ? { height: `max(0px, calc(${mobileHeroHeight} - 52px - var(--safe-top) - 150px))` }
            : undefined
        }
      />

      {/* 内容层：-mt-28/pt-28 与 .detail-ambient 的渐变起点对齐——渐变从标题
          上方开始压暗，音轨附近已接近纯黑，下面保持全黑。detail-content 是
          Netflix 主题的标题上移钩子（globals.css 把它的 pt 收小、标题借左侧
          遮罩直接落在剧照上，与发现详情页同一构图）。 */}
      <div className="detail-content relative z-10 -mt-28 pb-12 pt-28">
      {/* —— 头部信息区 —— */}
      <div className="relative z-10 px-12 pt-6 max-md:px-4 max-md:pt-3">
        {/* 桌面端竖版海报（Emby 风格）：左侧海报 + 右侧标题；手机端由
           页首 Hero 承担封面展示，这里不再重复 */}
        {!isMobile && immersiveUrl && (
          <div className="mb-6 flex items-start gap-6">
            <img
              src={immersiveUrl}
              alt=""
              decoding="async"
              className="aspect-[2/3] w-44 shrink-0 rounded-xl object-cover shadow-[0_18px_45px_rgba(0,0,0,0.55)] ring-1 ring-white/15"
            />
          </div>
        )}
        <div className="min-w-0 max-w-5xl pb-1">
          {/* break-words：未识别条目的标题就是文件名（Some.Movie.2023.2160p…），
              整串无空格，不允许断词就会横向撑开整页 */}
          <h1 className="text-on-image break-words text-[42px] font-bold leading-[1.1] tracking-[-0.02em] text-white max-md:text-[28px]">
            {detail.title}
          </h1>
          {!isMovie && selectedSeriesEpisode && (
            <p className="text-on-image mt-2 text-body text-white/65 max-md:mt-1.5 max-md:text-ui">
              {`第 ${selectedSeriesEpisode.seasonNumber} 季 第 ${selectedSeriesEpisode.episode.episode_number} 集${
                selectedSeriesEpisode.episode.name
                  ? ` - ${selectedSeriesEpisode.episode.name}`
                  : ""
              }`}
            </p>
          )}
          {(itemFacts.length > 0 ||
            itemResolutions.length > 0 ||
            itemHdrFormats.length > 0) && (
            <div className="tnum mt-3.5 flex flex-wrap items-center gap-x-3 gap-y-2 text-ui text-white/80 max-md:mt-2 max-md:gap-x-2 max-md:text-sub">
              {itemFacts.map((fact, index) => (
                <span key={fact} className="flex items-center gap-3 max-md:gap-2">
                  {index > 0 && <span aria-hidden="true">·</span>}
                  <span>{fact}</span>
                </span>
              ))}
              {itemResolutions.length > 0 && (
                <span className="flex items-center gap-3 max-md:gap-2">
                  {itemFacts.length > 0 && <span aria-hidden="true">·</span>}
                  <span>{itemResolutions.join(" / ")}</span>
                </span>
              )}
              {itemHdrFormats.length > 0 && (
                <span className="flex items-center gap-3 max-md:gap-2">
                  {(itemFacts.length > 0 || itemResolutions.length > 0) && (
                    <span aria-hidden="true">·</span>
                  )}
                  <span>{itemHdrFormats.join(" / ")}</span>
                </span>
              )}
            </div>
          )}
          {/* 系列 / 合集行随合集端点一并移除（新契约详情无 series_name / collections） */}
          <MediaTrackRows
            files={trackFiles}
            selectedFileId={selectedTrackFile?.file_id ?? null}
            onSelectedFileIdChange={setSelectedTrackFileId}
            onChanged={reload}
          />

          {/* 播放入口：只在真有在位文件时出现。缺集/待回收的版本给一个点了
              必然报错的按钮，比不给更糟。剧集跟随分集区当前选中的那一集。

              位置刻意排在音轨 / 字幕之后：这一屏的动线是「先挑版本与轨道，
              再决定开播」，播放键作为这段选择的落点，而不是把它插在标题与
              轨道之间、把一次连续的阅读切成两截。 */}
          {availableTrackFiles.length > 0 && (isMovie || selectedSeriesEpisode) && (
            <PlayAction
              watched={watched}
              favorite={favorite}
              favoriteLabel={isMovie ? "这部电影" : "这部剧"}
              marking={marking}
              onToggleFavorite={toggleFavorite}
              onTogglePlayed={togglePlayed}
              extraAction={<CollectionsAddButton mediaItemId={detail.media_item_id} />}
              onPlay={() => {
                // 退出播放要回到用户离开的这一屏（含季集查询参数）。导航
                // 状态不进播放页地址（§6.10 地址就是分享凭证），走
                // sessionStorage。读 location 而不是 useSearchParams()：
                // 后者会把整页拖进「必须包 Suspense」的预渲染约束。
                rememberPlayerReturnPath(window.location.pathname + window.location.search);
                navigate(playHref(detail.media_item_id as unknown as number, {
                    season:
                      !isMovie && selectedSeriesEpisode
                        ? selectedSeriesEpisode.seasonNumber
                        : undefined,
                    episode:
                      !isMovie && selectedSeriesEpisode
                        ? selectedSeriesEpisode.episode.episode_number
                        : undefined,
                  }),);
              }}
            />
          )}

          {/* 元数据刷新的失败提示（识别相关的结论都在「修正识别结果」弹窗里给） */}
          {refreshError && (
            <div className="mt-4 max-w-2xl rounded-xl border border-white/[0.1] bg-[rgba(14,16,22,0.6)] px-4 py-3 text-sub leading-6 text-[#ff9f9f] backdrop-blur-md">
              {refreshError}
            </div>
          )}
        </div>
      </div>

      {/* 简介承接标题、类型与介质轨信息；电影与剧集保持同一阅读路径。 */}
      {itemPlot && (
        <div className="mt-4 px-12 max-md:px-4">
          <ExpandablePlot text={itemPlot} />
        </div>
      )}

      {/* —— 剧集分集区：季选择 + 分集横滚卡 + 选中集的简介/规格/文件 ——
          分集置于简介之后，确保追剧用户首屏滑下即可立即选集 */}
      {!isMovie && detail.files.some((f) => f.season != null) && (
        <div className="mt-6 px-12 max-md:px-4">
          <SeasonEpisodesSection
            libraryId={libraryId}
            detail={detail}
            initialSeason={initialSeason}
            initialEpisode={initialEpisode}
            onEpisodeChange={setSelectedSeriesEpisode}
            refreshKey={episodesVersion}
          />
        </div>
      )}

      {/* 章节：内嵌章节标记 + 场景图（chapter_N.jpg），从章节起播跳到对应时间点。
          即使暂未识别到任何章节/片头片尾，也保留区块与「重新获取」入口，
          避免用户连从哪里触发识别都找不到（STRM 场景尤其需要手动配置 TheIntroDB 后重试）。 */}
      {detail.files.length > 0 && (
        <div className="mt-6 px-12 max-md:px-4">
          <div className="mb-2.5 flex flex-wrap items-center justify-between gap-2">
            <h3 className="text-sub font-semibold text-white/70">章节与片头片尾</h3>
            <div className="flex flex-wrap items-center gap-1.5 text-micro">
              <button
                type="button"
                disabled={chaptersBusy}
                onClick={() => void forceRefreshChaptersAndMarkers()}
                title="清除缓存并重新从 TheIntroDB 或视频内嵌章节识别"
                className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
              >
                {chaptersBusy ? "识别中…" : "重新获取片头/章节"}
              </button>
              <button
                type="button"
                disabled={chaptersBusy}
                onClick={() => void regenerateChapters()}
                className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
              >
                {chaptersBusy ? "生成中…" : "生成场景图"}
              </button>
              <button
                type="button"
                disabled={probing}
                onClick={() => void handleProbeMedia()}
                title="清空该条目全部文件的流信息/章节/片头片尾缓存并重新后台探测（视频流、音轨、字幕、声纹）"
                className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
              >
                {probing ? "探测中…" : "重新探测媒体信息"}
              </button>
            </div>
          </div>
          {chapters.length === 0 && (
            <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-5 text-caption text-white/45">
              暂未识别到片头片尾或章节数据。
              {detail.kind === "tv" && !isMovie ? " 请先在上方选集列表选中一集。" : ""}
              {" "}若为 STRM / 网盘挂载，建议在「设置 → 刮削设置 → 片头片尾」中配置
              TheIntroDB API Key 后点击右上角「重新获取片头/章节」。
            </div>
          )}
          <ChapterStrip
            chapters={chapters.map((chapter, index) => ({
              index,
              start_ms: chapter.start_ms,
              end_ms: chapter.end_ms,
              frame_ms: chapter.start_ms,
              title: chapter.title,
              synthetic: false,
              image_url: chapter.image_url,
            }))}
            pending={chaptersBusy}
            resumeMs={watched?.position_ms ?? null}
            onPlay={(chapter) => {
              rememberPlayerReturnPath(window.location.pathname + window.location.search);
              navigate(
                playHref(detail.media_item_id as unknown as number, {
                  season: !isMovie && selectedSeriesEpisode ? selectedSeriesEpisode.seasonNumber : undefined,
                  episode: !isMovie && selectedSeriesEpisode ? selectedSeriesEpisode.episode.episode_number : undefined,
                  tSeconds: Math.floor(chapter.start_ms / 1000),
                }),
              );
            }}
          />
          {chapterError && (
            <p className="mt-2 text-caption text-amber-300/80">{chapterError}</p>
          )}
        </div>
      )}

      {/* 演职员：条目目录 NFO 刮削产物（Emby/TMM 约定），与发现页同一组件。 */}
      {detail.cast.length > 0 && (
        <div className="mt-6 px-12 max-md:px-4">
          <CastRow
            cast={detail.cast.map<CastRowPerson>((member) => ({
              name: member.name,
              role: member.role,
              avatarUrl: member.avatar_url ? imageUrl(member.avatar_url) : null,
              tmdbPersonId: member.tmdb_person_id,
            }))}
            personHrefPrefix="/discover/people"
          />
        </div>
      )}

      {/* 文件区固定收尾：电影列全部版本；剧集只列当前选中集的全部版本。
          两者共用同一套弱化、默认折叠样式，未入库集自然不渲染。 */}
      {(() => {
        const files = isMovie ? detail.files : (selectedSeriesEpisode?.files ?? []);
        if (files.length === 0) return null;
        return (
          <div className="mt-6 px-12 max-md:px-4">
            <FileSection
              files={files}
              title="文件"
              onDeleteFile={canManageLibraries ? setDeleteFileTarget : undefined}
            />
          </div>
        );
      })()}

      {similar.length > 0 && (
        <div className="mt-8">
          <MediaRow
            row={{ id: "similar", title: "相似推荐", items: similar }}
            insetClassName="px-12 max-md:px-4"
          />
        </div>
      )}

      {/* 外部词条像友情链接一样固定在页面底部，继续使用原有的新窗口跳转；
          豆瓣在移动端沿用 App 唤起逻辑。 */}
      {tmdbId != null && (
        <div className="mt-6 px-12 max-md:px-4">
          <nav
            aria-label="外部词条"
            className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-white/[0.04] pt-4 text-caption"
          >
            <span className="text-[var(--text-faint)]">相关链接</span>
            <SourceLink
              href={`https://www.themoviedb.org/${detail.kind}/${tmdbId}`}
              label="TMDB"
            />
          </nav>
        </div>
      )}
      </div>

      {canManageLibraries && <DeleteDialog
        open={deleteOpen}
        detail={detail}
        onClose={() => setDeleteOpen(false)}
        onDeleted={() => {
          invalidateLibraryDetailSnapshot(libraryId);
          navigate(`/library/${libraryId}`, { replace: true });
        }}
        libraryId={libraryId}
      />}





      {canManageLibraries && <ReidentifyDialog
        open={reidentifyOpen}
        libraryId={libraryId}
        mediaItemId={mediaItemId}
        onClose={() => {
          setReidentifyOpen(false);
          // 条目可能已经不在了（文件全改挂走 / 全标为非独立作品）：重拉
          // 失败会落到本页的兜底态，引导用户回库存页
          if (reidentifyDirty) {
            setReidentifyDirty(false);
            reload();
          }
        }}
        onApplied={() => {
          invalidateLibraryDetailSnapshot(libraryId);
          setReidentifyDirty(true);
        }}
      />}

      {canManageLibraries && <ArtworkPickerDialog
        open={artworkOpen}
        libraryId={libraryId}
        mediaItemId={mediaItemId}
        onClose={() => setArtworkOpen(false)}
        onChanged={reload}
      />}

    </div>
  );
}

/**
 * 播放键旁的心 / 对勾的骨架：桌面端是与播放键**等高**（48px）的圆形玻璃键，
 * 只放图标（说明走悬停提示）；窄屏没有悬停，改成「图标 + 文字」的胶囊，两枚
 * 平分播放键下面的一整行（Netflix / Apple TV 手机端的做法）。
 *
 * 曾经直接复用顶栏 ⋯ 键的 36px 规格：与 48px 的播放键排在同一行，一大两小，
 * 看起来像播放键旁边挂了两个页面工具，而不是同一组动作。
 *
 * 外壳在所有状态下都是同一副深色玻璃：状态只落在**图标颜色**上（红心 / 绿勾），
 * 不给按钮铺红底绿底——试过一版整块上色，两枚重色大键压在白色播放键旁边，
 * 比播放键还抢眼，把这一行的主次弄反了。图标颜色写在 svg 自己身上，与按钮的
 * 文字色不在同一个元素，不存在工具类互相覆盖的问题。
 */
const MARK_BUTTON_CLASS =
  "inline-flex h-12 shrink-0 items-center justify-center gap-2 rounded-full border border-white/[0.12] bg-white/[0.08] text-white/85 backdrop-blur-md transition duration-200 hover:bg-white/[0.14] hover:text-white active:scale-[0.96] disabled:pointer-events-none disabled:opacity-60 md:size-12 max-md:h-11 max-md:px-4";

/**
 * 播放入口（主行动按钮 + 续播进度）。
 *
 * 三态一个按钮，而不是并排铺三个入口：
 *   - 从未播过 → 「播放」
 *   - 播到一半 → 「继续观看」，右侧补一条细进度条与「看到 xx:xx · 剩余 xx」
 *   - 已看完   → 「重新播放」，右侧换成一枚对勾与观看次数
 *
 * 主流媒体产品（Netflix / Apple TV / Disney+）在详情页都是这么处理的：
 * 一个页面只保留一个「现在就看」的落点，进度信息挂在按钮旁边做解释，
 * 而不是让用户先在「播放」和「继续播放」两个按钮之间做一次选择题。
 *
 * 尺寸也按这套规格给：整页标题 42px、正文栏宽 max-w-4xl，原来 px-6/py-2.5
 * 的小胶囊放在这样的版面里明显不像主行动按钮，因此抬到 h-12 + text-body，
 * 窄屏改为整行铺满（拇指区最容易命中的形状）。
 *
 * 播放键右侧是两枚等高的圆键：心（收藏整个条目）与对勾（标记当前单元已看 /
 * 未看）——Jellyfin 客户端条目页上那两个按钮的网页对应物，点的是同一份数据
 * （同一张 playback_state 表），在 Infuse 里点过的这里立刻能看到。已看态由
 * 对勾自己变绿表达，不再另写一行「已看完」；只有看过不止一次时才在
 * 旁边补一句次数。传了 onToggle* 才渲染；影片分享页的访客没有成员身份，
 * 不传就没有这两枚键。
 */
export function PlayAction({
  watched,
  onPlay,
  favorite = null,
  favoriteLabel = "",
  marking = false,
  onToggleFavorite,
  onTogglePlayed,
  extraAction = null,
}: {
  watched: PlaybackWatchState | null;
  onPlay: () => void;
  /** 整个条目的收藏态；null = 还没问到，按未收藏渲染 */
  favorite?: boolean | null;
  /** 收藏对象的称呼（「这部电影」/「这部剧」），进提示文案 */
  favoriteLabel?: string;
  /** 标记请求进行中：两枚键一起禁用，防连点 */
  marking?: boolean;
  onToggleFavorite?: () => void;
  onTogglePlayed?: () => void;
  /** 额外动作（如「加入合集」），排在收藏/已看之后。 */
  extraAction?: React.ReactNode | null;
}) {
  const positionMs = watched?.position_ms ?? 0;
  const durationMs = watched?.duration_ms ?? null;
  const finished = watched?.played ?? false;
  // 「能续播」以服务端结论为准：已看完的重播从头开始（播放器同一套判定），
  // 所以看完之后不再展示续播点，否则点进去的位置和文案对不上。
  const resumable = !finished && positionMs > 0;
  // 进度条至少留 2%：真按比例画，刚开头几分钟的记录在条上是看不见的一根线。
  const percent =
    resumable && durationMs && durationMs > 0
      ? Math.min(100, Math.max(2, Math.round((positionMs / durationMs) * 100)))
      : null;
  const remainingMinutes =
    resumable && durationMs && durationMs > positionMs
      ? Math.round((durationMs - positionMs) / 60000)
      : null;
  const label = finished ? "重新播放" : resumable ? "继续观看" : "播放";
  const progressText = resumable
    ? [
        `看到 ${formatClock(positionMs)}`,
        remainingMinutes && remainingMinutes >= 1
          ? `剩余 ${formatRuntimeMinutes(remainingMinutes)}`
          : durationMs
            ? "即将看完"
            : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : null;

  return (
    <div className="mt-5 flex max-w-4xl flex-wrap items-center gap-x-5 gap-y-3 max-md:mt-4">
      <button
        type="button"
        onClick={onPlay}
        aria-label={progressText ? `${label}，${progressText}` : label}
        className="inline-flex h-12 shrink-0 items-center justify-center gap-2.5 rounded-full bg-white px-8 text-body font-semibold text-black shadow-[0_10px_30px_rgba(0,0,0,0.35)] transition duration-200 hover:bg-white/90 active:scale-[0.985] max-md:h-11 max-md:w-full max-md:px-6"
      >
        <PlayIcon className="size-5" />
        {label}
      </button>

      {(onToggleFavorite || onTogglePlayed) && (
        <div className="flex items-center gap-2.5">
          {onToggleFavorite && (
            <Tooltip
              content={favorite ? `取消收藏${favoriteLabel}` : `收藏${favoriteLabel}`}
              dismissOnReferencePress
            >
              <button
                type="button"
                onClick={onToggleFavorite}
                disabled={marking}
                aria-pressed={Boolean(favorite)}
                aria-label={favorite ? "取消收藏" : "收藏"}
                className={MARK_BUTTON_CLASS}
              >
                <HeartIcon
                  className={favorite ? "size-5 text-[var(--danger)]" : "size-5"}
                  fill={favorite ? "currentColor" : "none"}
                />
                {/* 窄屏没有悬停提示，按钮自己带文字；文字随状态变，一眼知道现在是什么 */}
                <span className="text-ui font-medium md:hidden">{favorite ? "已收藏" : "收藏"}</span>
              </button>
            </Tooltip>
          )}
          {onTogglePlayed && (
            <Tooltip content={finished ? "标记为未看" : "标记为已看"} dismissOnReferencePress>
              <button
                type="button"
                onClick={onTogglePlayed}
                disabled={marking}
                aria-pressed={finished}
                aria-label={finished ? "标记为未看" : "标记为已看"}
                className={MARK_BUTTON_CLASS}
              >
                <CheckIcon
                  className={
                    finished ? "size-5 stroke-[2.4] text-[var(--ok)]" : "size-5 stroke-[2.4]"
                  }
                />
                <span className="text-ui font-medium md:hidden">
                  {finished ? "已看完" : "标为已看"}
                </span>
              </button>
            </Tooltip>
          )}
          {extraAction}
        </div>
      )}

      {progressText && (
        <div className="min-w-0 max-md:w-full">
          <div
            className="h-1 w-44 overflow-hidden rounded-full bg-white/[0.18] max-md:w-full"
            role="progressbar"
            aria-valuenow={percent ?? 0}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label="观看进度"
          >
            <div className="h-full rounded-full bg-white/85" style={{ width: `${percent ?? 0}%` }} />
          </div>
          <p className="tnum mt-1.5 text-caption text-white/60">{progressText}</p>
        </div>
      )}

      {/* 已看态已由对勾变绿表达；只有看过不止一次才值得多说一句 */}
      {finished && watched && watched.play_count > 1 && (
        <p className="tnum text-caption text-white/55">看过 {watched.play_count} 次</p>
      )}
    </div>
  );
}

/**
 * 条目操作 ⋯ 菜单：修正识别结果 / 刷新元数据 / 更换图片 / 转移 / 删除。
 *
 * 这几个都是低频且不可逆（改身份锚、重下全套图、搬目录、删文件）的操作，
 * 摆成常驻大按钮既压着正文，又把「误点」的成本摊在最显眼的位置。收进顶栏
 * 右上角的 ⋯ 后，页面主区只剩内容；跑起来之后的状态仍由正文里的进度条
 * 完整交代（与媒体库页「操作进 ⋯、状态看正文」的分工一致）。
 *
 * 「转移到其他库」与「修正识别结果」是**两种不同的错**的补救，菜单里紧挨着
 * 摆：前者是"片子认对了、库放错了"（韩剧进了大陆剧库），后者是"片子认错了"。
 *
 * 「修正识别结果」带省略号是有意的——它开的是一个拍板面板（重跑识别链、
 * 摆出结论让人选），不是点下去就改身份的一次性动作。
 */
function ItemActionsMenu({
  canManage,
  identifiable,
  scraped,
  readsNfo,
  scraping,
  searchHref,
  onReidentify,
  onRefreshMetadata,
  onRefreshChapters,
  onChangeArtwork,
  onDelete,
  onUpgrade,
  onClearHistory,
}: {
  /** 媒体库管理权限：识别/刮削/图片/转移/删除这些条目管理项按它显隐 */
  canManage: boolean;
  /** 所在库有识别链（影视库）：给「修正识别结果」；其他库没有可认领的外部身份 */
  identifiable: boolean;
  /** 条目本身来自 TMDB：给刷新元数据/更换图片；本地条目只有封面 */
  scraped: boolean;
  /** 其他库的视频条目：刷新会先重读视频旁的 NFO（照片、影视库里的临时条目只重建封面） */
  readsNfo: boolean;
  scraping: boolean;
  /** 站点资源搜索直达（预填片名）：手动补版本/换版本的入口 */
  searchHref: string;
  onReidentify: () => void;
  onRefreshMetadata: () => void;
  onRefreshChapters?: () => void;
  onChangeArtwork: () => void;
  onDelete: () => void;
  /** 洗版入口（quality-upgrade.md §13.5）；无订阅权限或条目未识别时不传 */
  onUpgrade?: () => void;
  /** 清除当前登录身份自己对这部作品的观看记录：个人数据，与管理权无关 */
  onClearHistory: () => void;
}) {
  const navigate = useNavigate();
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="更多操作"
          className={`${PAGE_NAV_BUTTON_CLASS} relative data-[state=open]:bg-black/55 data-[state=open]:text-white`}
        >
          <MoreIcon className="size-[18px] max-md:size-[22px]" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[11rem] p-1"
        >
          {canManage && (
            <DropdownMenu.Item
              onSelect={() => navigate(searchHref)}
              className={itemClass}
            >
              搜索资源
            </DropdownMenu.Item>
          )}
          {onUpgrade && (
            <DropdownMenu.Item onSelect={onUpgrade} className={itemClass}>
              洗版…
            </DropdownMenu.Item>
          )}
          {canManage && (
            <>
              <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
              {identifiable && (
                <DropdownMenu.Item onSelect={onReidentify} className={itemClass}>
                  修正识别结果…
                </DropdownMenu.Item>
              )}
              <DropdownMenu.Item
                onSelect={onRefreshMetadata}
                disabled={scraping}
                className={itemClass}
              >
                {scraped
                  ? scraping
                    ? "正在刷新元数据…"
                    : "刷新元数据"
                  : scraping
                    ? readsNfo
                      ? "正在读取 NFO…"
                      : "正在生成封面…"
                    : readsNfo
                      ? "重新读取 NFO 与封面"
                      : "重新生成封面"}
              </DropdownMenu.Item>
              {onRefreshChapters && (
                <DropdownMenu.Item
                  onSelect={onRefreshChapters}
                  className={itemClass}
                >
                  重新识别片头片尾与章节
                </DropdownMenu.Item>
              )}
              {scraped && (
              <DropdownMenu.Item onSelect={onChangeArtwork} className={itemClass}>
                更换图片…
              </DropdownMenu.Item>
              )}
              <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
              <DropdownMenu.Item
                onSelect={onDelete}
                className={`${itemClass} !text-[#ff9f9f] data-[highlighted]:!bg-[rgba(255,90,90,0.16)] data-[highlighted]:!text-[#ffb4b4]`}
              >
                删除影片
              </DropdownMenu.Item>
            </>
          )}
          {(canManage || onUpgrade) && (
            <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />
          )}
          <DropdownMenu.Item onSelect={onClearHistory} className={itemClass}>
            清除观看记录…
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/* ------------------------------------------------------------------------ */
/* 展示格式化：ffprobe 原始值 → 用户认知的规格语言                              */
/* ------------------------------------------------------------------------ */

/** 图片地址：本地美术图是 API 相对路径（补 base），TMDB 图床走缓存代理。 */
function imageUrl(url: string | null): string {
  if (!url) return "";
  return /^https?:\/\//i.test(url) ? cachedImageUrl(url) : resolveRequestUrl(url);
}

const VIDEO_CODEC_LABELS: Record<string, string> = {
  hevc: "HEVC",
  h264: "H.264",
  h265: "HEVC",
  av1: "AV1",
  vc1: "VC-1",
  mpeg2video: "MPEG-2",
  vp9: "VP9",
};

function videoCodecLabel(codec: string | null): string | null {
  if (!codec) return null;
  return VIDEO_CODEC_LABELS[codec.toLowerCase()] ?? codec.toUpperCase();
}

function fileResolutionLabel(file: LibraryItemFile | null | undefined): string | null {
  if (!file) return null;
  if (file.resolution) return formatVideoResolution(file.resolution);
  if (file.video_track?.width && file.video_track?.height) {
    const h = file.video_track.height;
    const w = file.video_track.width;
    if (w >= 3800 || h >= 2100) return "4K";
    if (h >= 1000) return "1080p";
    if (h >= 700) return "720p";
    return `${h}p`;
  }
  return null;
}

function fileHdrLabel(file: LibraryItemFile | null | undefined): string | null {
  return file?.hdr ?? null;
}

function fileCodecLabel(file: LibraryItemFile | null | undefined): string | null {
  if (!file) return null;
  const codec = file.codec || file.video_track?.codec || null;
  return videoCodecLabel(codec);
}

/** ffprobe 小数帧率保留行业常用精度，如 23.976 / 29.97 / 59.94 fps。 */
function frameRateLabel(frameRate: number | null): string | null {
  if (frameRate == null || frameRate <= 0) return null;
  return `${Math.round(frameRate * 1000) / 1000} fps`;
}

/* ------------------------------------------------------------------------ */
/* 子组件                                                                     */
/* ------------------------------------------------------------------------ */

/**
 * 电影与分集简介共用的四行摘要。只有真实发生溢出时才出现展开入口；剧集
 * 切换分集会先恢复折叠，再按新文案重新测量，避免沿用上一集的展开状态。
 */
export function ExpandablePlot({ text }: { text: string }) {
  const paragraphRef = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [hasOverflow, setHasOverflow] = useState(false);

  useEffect(() => {
    setExpanded(false);
    setHasOverflow(false);
  }, [text]);

  useEffect(() => {
    if (expanded) return;
    const paragraph = paragraphRef.current;
    if (!paragraph) return;

    const measure = () => {
      setHasOverflow(paragraph.scrollHeight > paragraph.clientHeight + 1);
    };
    const frame = window.requestAnimationFrame(measure);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(paragraph);
    return () => {
      window.cancelAnimationFrame(frame);
      observer?.disconnect();
    };
  }, [expanded, text]);

  return (
    <div className="max-w-3xl">
      <p
        ref={paragraphRef}
        className={`text-on-image text-body-lg leading-7 text-white/78 ${
          expanded ? "" : "line-clamp-4"
        }`}
      >
        {text}
      </p>
      {(hasOverflow || expanded) && (
        <button
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
          className="mt-1.5 inline-flex items-center gap-1 text-sub font-medium text-white/55 transition hover:text-white/85 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-2)]"
        >
          {expanded ? "收起" : "展开全文"}
          <ChevronRightIcon
            className={`size-3.5 transition-transform ${expanded ? "-rotate-90" : "rotate-90"}`}
          />
        </button>
      )}
    </div>
  );
}

/** 外部信息源链接：新窗口打开站点词条，样式与「在 TMDB 打开核对」保持一致。 */
export function SourceLink({ href, label }: { href: string; label: string }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="text-caption text-[var(--text-muted)] underline decoration-white/20 underline-offset-2 transition hover:text-white/80"
    >
      {label} ↗
    </a>
  );
}

/**
 * 季号 → 展示名（0 是特别篇，TMDB 的 specials 惯例）。整季一个文件都没有的
 * 季标"未入库"：季选择器里元数据的季和实有的季混在一起，不标出来用户会以为
 * 自己本地存了那么多季。原生 select 收起时显示的就是选中项的文本，所以这个
 * 后缀在展开和收起两种状态下都看得到。
 */
function seasonLabel(season: number, owned: boolean): string {
  const name = season === 0 ? "特别篇" : `第 ${season} 季`;
  return owned ? name : `${name} · 未入库`;
}

/**
 * 剧集分集区（播放器式）：季选择器 + 分集横滚缩略图卡（剧照 + 集名，
 * 缺集置灰）。本区只负责选择，当前集的简介与轨道回到 Hero，物理文件回到
 * 页面底部的统一折叠文件区。分集信息本地刮削优先，TMDB 分季兜底。
 */
export function SeasonEpisodesSection<F extends { file_id: string; season: number | null }>({
  libraryId,
  detail,
  initialSeason,
  initialEpisode,
  onEpisodeChange,
  fetchEpisodes,
  refreshKey = 0,
}: {
  libraryId: string;
  detail: EpisodeSectionItem<F>;
  initialSeason?: number;
  initialEpisode?: number;
  onEpisodeChange?: (selection: SelectedEpisodeContext<F> | null) => void;
  /** 分集数据源；缺省按库详情接口拉。影片分享页传访客通道的取数函数 */
  fetchEpisodes?: (mediaItemId: string, season: number) => Promise<SeasonEpisodes>;
  /** 变化即静默重拉本季分集（页面内改了观看状态后），不重置选中集与滚动位置 */
  refreshKey?: number;
}) {
  // 季清单：新契约详情不下发 seasons，从文件里实有的季号推导
  const seasons = useMemo(() => {
    const set = new Set<number>();
    for (const f of detail.files) {
      if (f.season != null) set.add(f.season);
    }
    return [...set].sort((a, b) => a - b);
  }, [detail.files]);
  // 哪些季真在库——台账文件的季号集合（这里与 seasons 同源）
  const ownedSeasons = useMemo(
    () => new Set(detail.files.map((f) => f.season).filter((v): v is number => v != null)),
    [detail.files],
  );
  // 默认落在第一个在库的季，而不是季号最小的那一季：只存了第 5、6 季的剧，
  // 打开详情页就停在满屏置灰的第 1 季，第一眼像"我的片子没了"
  const defaultSeason = seasons.find((s) => ownedSeasons.has(s)) ?? seasons[0];
  // 最近观看入口只有在目标季仍有在位文件时才采纳；记录过期则回退普通详情。
  const requestedSeason =
    initialSeason != null && ownedSeasons.has(initialSeason) ? initialSeason : undefined;
  const [season, setSeason] = useState(() => requestedSeason ?? defaultSeason);
  const [data, setData] = useState<SeasonEpisodes | null>(null);
  const [failed, setFailed] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const sectionRef = useRef<HTMLElement>(null);
  const initialEpisodeScrolled = useRef(false);

  useEffect(() => {
    let cancelled = false;
    setData(null);
    setFailed(false);
    (fetchEpisodes
      ? fetchEpisodes(detail.media_item_id, season)
      : getItemEpisodes(libraryId, detail.media_item_id, season)
    )
      .then((result) => {
        if (cancelled) return;
        setData(result);
        // 最近观看入口优先选中目标集；目标已不在库时安全回退到第一集在库内容。
        const requested =
          season === requestedSeason && initialEpisode != null
            ? result.episodes.find(
                (episode) => episode.episode_number === initialEpisode && episode.owned,
              )
            : undefined;
        const first = requested ?? result.episodes.find((e) => e.owned) ?? result.episodes[0];
        setSelected(first?.episode_number ?? null);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
    // detail.file_count：单文件删除后详情重拉，分集的 owned/file_ids 也要
    // 跟着刷新（用文件数而不是整个 detail 做依赖——刮削轮询也会换 detail
    // 对象，不该每轮都重拉分集）
  }, [
    libraryId,
    detail.media_item_id,
    detail.files.length,
    season,
    requestedSeason,
    initialEpisode,
    fetchEpisodes,
  ]);

  // Hero 的对勾改了当前集的观看状态：只换分集数据（进度条 / 绿色对勾跟上），
  // 不走上面那条会重置选中集的加载路径。首次渲染（refreshKey=0）不拉。
  useEffect(() => {
    if (!refreshKey) return;
    let cancelled = false;
    (fetchEpisodes
      ? fetchEpisodes(detail.media_item_id, season)
      : getItemEpisodes(libraryId, detail.media_item_id, season)
    )
      .then((result) => {
        if (!cancelled && result.season_number === season) setData(result);
      })
      // 拉不到就保持旧数据，下次换季自然刷新
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // 只响应 refreshKey：其余依赖变化由上面的加载效果处理
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshKey]);

  // 分集数据异步到达后，把最近观看对应的集卡横向滚到中间；只执行一次，
  // 后续用户手动换季/选集不抢滚动位置。
  useEffect(() => {
    if (
      initialEpisodeScrolled.current ||
      season !== requestedSeason ||
      selected == null ||
      selected !== initialEpisode
    ) {
      return;
    }
    const frame = window.requestAnimationFrame(() => {
      sectionRef.current
        ?.querySelector<HTMLElement>(`[data-episode-number="${selected}"]`)
        ?.scrollIntoView({ behavior: "smooth", inline: "center", block: "nearest" });
      initialEpisodeScrolled.current = true;
    });
    return () => window.cancelAnimationFrame(frame);
  }, [initialEpisode, requestedSeason, season, selected]);

  const filesById = useMemo(
    () => new Map(detail.files.map((f) => [f.file_id, f])),
    [detail.files],
  );
  const current =
    data?.season_number === season
      ? (data.episodes.find((e) => e.episode_number === selected) ?? null)
      : null;
  const currentFiles = useMemo(
    () =>
      (current?.file_ids ?? [])
        .map((id) => filesById.get(id))
        .filter((f): f is F => Boolean(f)),
    [current, filesById],
  );
  const ownedCount = data ? data.episodes.filter((e) => e.owned).length : 0;

  // Hero 的简介、文件、分辨率和轨道必须与分集区保持同一选择；数据加载或
  // 换季期间先清空，避免短暂显示上一季上一集的信息。
  useEffect(() => {
    onEpisodeChange?.(
      current
        ? {
            seasonNumber: season,
            episode: current,
            files: currentFiles,
          }
        : null,
    );
  }, [current, currentFiles, onEpisodeChange, season]);

  return (
    <section ref={sectionRef}>
      <div className="mb-3 flex items-center gap-3">
        <h2 className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
          分集
        </h2>
        {seasons.length > 1 ? (
          <select
            value={season}
            onChange={(e) => setSeason(Number(e.target.value))}
            className="rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-1.5 text-sub text-white/90 outline-none focus:border-white/25 [&>option]:bg-[#181c28]"
          >
            {seasons.map((s) => (
              <option key={s} value={s}>
                {seasonLabel(s, ownedSeasons.has(s))}
              </option>
            ))}
          </select>
        ) : (
          <span className="text-sub text-[var(--text-muted)]">
            {seasonLabel(season, ownedSeasons.has(season))}
          </span>
        )}
        {data && (
          <span className="tnum text-sub text-[var(--text-faint)]">
            在库 {ownedCount} / {data.episodes.length} 集
          </span>
        )}
      </div>

      {failed && (
        <p className="text-sub text-[var(--text-muted)]">分集信息加载失败，请稍后重试。</p>
      )}
      {!data && !failed && (
        <div className="flex items-center gap-2.5 py-6 text-sub text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在读取分集信息…
        </div>
      )}

      {/* 分集横滚卡：16:9 剧照 + "N. 集名"；缺集置灰，选中亮环。
          走 HScroller 以复用发现页海报行的左右翻页钮，避免用户看不出这行可横滑。 */}
      {data && (
        <HScroller className="-mx-1 gap-3 px-1 pb-1 pt-1">
          {data.episodes.map((episode) => (
            <EpisodeCard
              key={episode.episode_number}
              episode={episode}
              selected={episode.episode_number === selected}
              onSelect={() => setSelected(episode.episode_number)}
            />
          ))}
        </HScroller>
      )}
    </section>
  );
}

/** 分集横滚卡：16:9 剧照缩略图 + 集号集名；缺集置灰。

    观看状态与首页「最近观看」卡同一套视觉语言（同一张 playback_state 表，
    Jellyfin 客户端里看的进度也在内）：看了一半底部细进度条，看完右上角
    绿色对勾——用户扫一眼分集行就知道追到哪了。 */
function EpisodeCard({
  episode,
  selected,
  onSelect,
}: {
  episode: LibraryEpisode;
  selected: boolean;
  onSelect: () => void;
}) {
  // 看完 = 满条绿；看一半 = 百分比蓝；有记录但算不出百分比（无时长）给
  // 一根 60% 透明的整条兜底——三种形态与 RecentWatchCard 逐一对应
  const progress = episode.played ? 100 : episode.progress_percent;
  return (
    <button
      type="button"
      data-episode-number={episode.episode_number}
      onClick={onSelect}
      aria-pressed={selected}
      className={`w-[200px] shrink-0 rounded-xl text-left outline-none transition ${
        episode.owned ? "" : "opacity-45 saturate-50"
      }`}
    >
      <div
        className={`relative aspect-video overflow-hidden rounded-xl bg-[#141824] transition ${
          selected
            ? "ring-2 ring-white/85 shadow-[0_10px_30px_rgba(0,0,0,0.5)]"
            : "ring-1 ring-white/[0.08] hover:ring-white/35"
        }`}
      >
        <PosterImage
          src={imageUrl(episode.still_url)}
          alt={`第 ${episode.episode_number} 集剧照`}
          className="size-full object-cover"
          fallback={
            <span className="tnum flex size-full items-center justify-center text-[22px] font-bold text-white/20">
              {episode.episode_number}
            </span>
          }
        />
        {/* 右上角状态位：缺集与已看对勾同排（看过之后文件丢了两者会同时出现） */}
        {(!episode.owned || episode.played) && (
          <div className="pointer-events-none absolute right-1.5 top-1.5 flex items-center gap-1">
            {!episode.owned && (
              <span className="rounded bg-black/60 px-1.5 py-px text-micro font-semibold text-[var(--warn)]">
                缺
              </span>
            )}
            {episode.played && (
              <span
                aria-label="已看完"
                className="flex size-5 items-center justify-center rounded-full bg-[var(--ok)] text-[#07120c] shadow-lg"
              >
                <CheckIcon className="size-3 stroke-[2.5]" />
              </span>
            )}
          </div>
        )}
        {progress != null ? (
          <div className="pointer-events-none absolute inset-x-1.5 bottom-1.5 h-[3px] overflow-hidden rounded-full bg-white/25">
            <div
              className={`h-full rounded-full ${episode.played ? "bg-[var(--ok)]" : "bg-[var(--accent-2)]"}`}
              style={{ width: `${progress}%` }}
            />
          </div>
        ) : (
          episode.position_ms > 0 && (
            <div className="pointer-events-none absolute inset-x-1.5 bottom-1.5 h-[3px] rounded-full bg-[var(--accent-2)]/60" />
          )
        )}
      </div>
      <p
        className={`tnum mt-1.5 truncate text-sub ${
          selected ? "font-semibold text-white" : "text-[var(--text)]"
        }`}
      >
        {episode.episode_number}. {episode.name ?? `第 ${episode.episode_number} 集`}
      </p>
    </button>
  );
}

/** 从完整文件路径提取文件名（新契约的文件只有 path，没有 file_name）。 */
function fileNameOf(filePath: string): string {
  const cut = Math.max(filePath.lastIndexOf("/"), filePath.lastIndexOf("\\"));
  return cut < 0 ? filePath : filePath.slice(cut + 1);
}

/** 从完整文件路径提取保存目录，同时兼容 NAS/POSIX 与 Windows 分隔符。 */
function fileDirectory(filePath: string): string {
  const normalized = filePath.replace(/[\\/]+$/, "");
  const separatorIndex = Math.max(
    normalized.lastIndexOf("/"),
    normalized.lastIndexOf("\\"),
  );
  if (separatorIndex < 0) return "—";
  if (separatorIndex === 0) return normalized.slice(0, 1);
  return normalized.slice(0, separatorIndex);
}

/**
 * 文件区：详情正文的最后一块。电影与剧集当前集完全共用一套弱化样式，
 * 默认只露出文件名，点击后按物理文件展开尺寸、视频与码率。
 */
function FileSection({
  files,
  title,
  onDeleteFile,
}: {
  files: LibraryItemFile[];
  title: string;
  onDeleteFile?: (file: LibraryItemFile) => void;
}) {
  return (
    <section>
      <h2 className="mb-3 flex items-center gap-2 text-ui font-medium tracking-[-0.01em] text-[var(--text-muted)]">
        <span>
          {title}{" "}
          <span className="tnum text-caption font-normal text-[var(--text-faint)]">
            {files.length}
          </span>
        </span>
      </h2>
      <div className="overflow-hidden rounded-xl border border-white/[0.04] bg-white/[0.015]">
        {files.map((file) => (
          <FileRow
            key={file.file_id}
            file={file}
            onDelete={onDeleteFile ? () => onDeleteFile(file) : undefined}
          />
        ))}
      </div>
    </section>
  );
}

function FileRow({
  file,
  onDelete,
}: {
  file: LibraryItemFile;
  onDelete?: () => void;
}) {
  // 每个物理文件独立展开，多版本无需再维护额外的版本切换状态。
  const [expanded, setExpanded] = useState(false);
  const resolvedRes = fileResolutionLabel(file);
  const resolvedHdr = fileHdrLabel(file);
  const resolvedCodec = fileCodecLabel(file);
  const pictureInfo = [
    resolvedRes,
    resolvedHdr,
  ]
    .filter((value): value is string => Boolean(value))
    .join(" · ");
  const detailsId = `file-details-${file.file_id}`;
  const name = fileNameOf(file.path);

  return (
    <div className="border-b border-white/[0.035] last:border-b-0">
      <div className="group/filerow flex items-center gap-2 px-4 py-2.5 transition-colors hover:bg-white/[0.025]">
        <button
          type="button"
          aria-expanded={expanded}
          aria-controls={detailsId}
          onClick={() => setExpanded((value) => !value)}
          className="flex min-w-0 flex-1 items-center gap-2.5 text-left text-[var(--text-muted)] outline-none transition-colors hover:text-[var(--text)] focus-visible:text-[var(--text)]"
        >
          <ChevronRightIcon
            className={`size-3.5 shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
          />
          <Tooltip
            content={
              <span className="tnum break-all font-mono text-caption leading-5">
                {file.path}
              </span>
            }
            maxWidth={520}
          >
            <span className="min-w-0 truncate text-sub">{name}</span>
          </Tooltip>
        </button>
        {onDelete && (
          <button
            type="button"
            aria-label="删除此文件"
            title="删除此文件"
            onClick={onDelete}
            className="touch-reveal touch-target shrink-0 rounded-md p-1.5 text-[var(--text-faint)] opacity-0 transition group-hover/filerow:opacity-100 hover:bg-white/[0.05] hover:text-[#ff9f9f]"
          >
            <TrashIcon className="size-4" />
          </button>
        )}
      </div>
      {expanded && (
        <div
          id={detailsId}
          className="border-t border-white/[0.035] bg-black/[0.08] px-10 py-4 max-md:px-4"
        >
          <dl className="grid max-w-2xl grid-cols-[72px_minmax(0,1fr)] gap-x-5 gap-y-3 text-sub max-md:grid-cols-[64px_minmax(0,1fr)]">
            <dt className="text-[var(--text-faint)]">保存目录</dt>
            <dd className="min-w-0 break-all font-mono text-caption leading-5 text-[var(--text-muted)]">
              {fileDirectory(file.path)}
            </dd>
            <dt className="text-[var(--text-faint)]">画面规格</dt>
            <dd className="tnum text-[var(--text-muted)]">
              {pictureInfo || "未能探测（文件不可达或尚未扫描）"}
            </dd>
            <dt className="text-[var(--text-faint)]">视频编码</dt>
            <dd className="tnum text-[var(--text-muted)]">{resolvedCodec || "尚未探测"}</dd>
            <dt className="text-[var(--text-faint)]">片源</dt>
            <dd className="text-[var(--text-muted)]">
              {file.quality_source || "未识别"}
            </dd>
          </dl>
        </div>
      )}
    </div>
  );
}

function DeleteDialog({
  open,
  detail,
  libraryId,
  onClose,
  onDeleted,
}: {
  open: boolean;
  detail: LibraryItemDetail;
  libraryId: string;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ItemDeleteResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setConfirmed(false);
      setResult(null);
      setError(null);
    }
  }, [open]);

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      setResult(await deleteLibraryItem(libraryId, detail.media_item_id));
    } catch (err) {
      setError(err instanceof Error ? err.message : "删除失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal open={open} onClose={busy ? () => {} : onClose} label="删除影片" width="lg">
      <div className="p-6">
        {result ? (
          <>
            <h3 className="text-title-sm font-semibold text-[var(--text)]">
              {result.errors.length > 0 ? "部分删除完成" : "已从磁盘彻底删除"}
            </h3>
            <div className="scroll-thin mt-4 max-h-56 overflow-y-auto rounded-xl border border-white/[0.08] bg-white/[0.03] p-3.5">
              {result.removed_paths.map((p) => (
                <p key={p} className="tnum break-all py-0.5 font-mono text-caption leading-5 text-white/70">
                  {p}
                </p>
              ))}
              {result.removed_paths.length === 0 && (
                <p className="text-sub text-[var(--text-muted)]">没有删除任何磁盘路径</p>
              )}
            </div>
            {result.errors.length > 0 && (
              <div className="mt-3 space-y-1 text-sub leading-5 text-[#ff9f9f]">
                {result.errors.map((e) => (
                  <p key={e}>{e}</p>
                ))}
              </div>
            )}
            <p className="mt-3 text-sub text-[var(--text-muted)]">
              已清理 {result.rows_deleted} 条台账，释放 {formatBytes(result.freed_bytes)}。
            </p>
            <div className="mt-5 flex justify-end">
              <button
                type="button"
                onClick={onDeleted}
                className="btn-accent rounded-full px-5 py-2 text-ui font-semibold"
              >
                回到库存页
              </button>
            </div>
          </>
        ) : (
          <>
            <h3 className="flex items-center gap-2 text-title-sm font-semibold text-[var(--text)]">
              <TrashIcon className="size-4.5 text-[#ff9f9f]" />
              删除「{detail.title}」
            </h3>
            <p className="mt-3 text-ui leading-6 text-white/80">
              这不是「从列表移除」——将把下列目录从磁盘
              <span className="font-semibold text-[#ff9f9f]">彻底删除</span>
              ，包括视频文件、NFO、海报、字幕等全部刮削产物，共{" "}
              <span className="tnum font-semibold">{detail.files.length}</span> 个媒体文件。
              此操作不可恢复。
            </p>
            <div className="scroll-thin mt-4 max-h-40 overflow-y-auto rounded-xl border border-white/[0.08] bg-white/[0.03] p-3.5">
              {detail.files.map((f) => (
                <p key={f.file_id} className="tnum flex items-start gap-1.5 break-all py-0.5 font-mono text-caption leading-5 text-white/70">
                  <FolderIcon className="mt-0.5 size-3.5 shrink-0 text-white/40" />
                  {f.path}
                </p>
              ))}
            </div>
            <label className="mt-4 flex cursor-pointer items-start gap-2.5 text-sub leading-6 text-white/80">
              <input
                type="checkbox"
                checked={confirmed}
                onChange={(e) => setConfirmed(e.target.checked)}
                className="mt-1 size-4 accent-[#ff6b6b]"
              />
              我已明白：以上目录及其中全部文件将被永久删除，无法恢复。
            </label>
            {error && <p className="mt-3 text-sub text-[#ff9f9f]">{error}</p>}
            <div className="mt-5 flex justify-end gap-3">
              <button
                type="button"
                onClick={onClose}
                disabled={busy}
                className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
              >
                取消
              </button>
              <button
                type="button"
                onClick={run}
                disabled={!confirmed || busy}
                className="flex items-center gap-2 rounded-full bg-[var(--danger-solid)] px-5 py-2 text-ui font-semibold text-white transition hover:bg-[var(--danger-solid-hover)] disabled:cursor-not-allowed disabled:opacity-40"
              >
                {busy && (
                  <span className="size-3.5 animate-spin rounded-full border-2 border-white/30 border-t-white" />
                )}
                {busy ? "正在删除…" : "彻底删除"}
              </button>
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}
