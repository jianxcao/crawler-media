import { StrictMode, lazy } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Navigate, Route, Routes } from "react-router-dom";

import RootLayout from "@/app/layout";
import AppLayout from "@/app/(app)/layout";
import PlayLayout from "@/app/play/layout";
import HomePage from "@/app/(app)/page";
import { NotFound } from "@/src/not-found";

const LoginPage = lazy(() => import("@/app/login/page"));
const HealthPage = lazy(() => import("@/app/health/page"));
const ActivityPage = lazy(() => import("@/app/(app)/activity/page"));
const MediaTransferView = lazy(() =>
  import("@/components/media-transfer-view").then((m) => ({ default: m.MediaTransferView })),
);
const CatalogCacheView = lazy(() =>
  import("@/components/catalog-cache-view").then((m) => ({ default: m.CatalogCacheView })),
);
const CollectionsPage = lazy(() => import("@/app/(app)/collections/page"));
const CollectionDetailPage = lazy(() => import("@/app/(app)/collections/[id]/page"));
const DiscoverPage = lazy(() => import("@/app/(app)/discover/[type]/page"));
const DiscoveryCollectionPage = lazy(
  () => import("@/app/(app)/discover/[type]/collections/[provider]/[collectionId]/page"),
);
const HighScorePage = lazy(() => import("@/app/(app)/discover/movie/high-score/page"));
const Top250Page = lazy(() => import("@/app/(app)/discover/movie/top250/page"));
const DiscoveredPersonPage = lazy(() => import("@/app/(app)/discover/people/[id]/page"));
const LibraryPage = lazy(() => import("@/app/(app)/library/page"));
const LibraryDetailPage = lazy(() => import("@/app/(app)/library/[id]/page"));
const LibraryItemDetailPage = lazy(() => import("@/app/(app)/library/[id]/item/[mediaItemId]/page"));
const LibraryCustomizePage = lazy(() => import("@/app/(app)/library/customize/page"));
const FavoritesPage = lazy(() => import("@/app/(app)/library/favorites/page"));
const LibraryManagePage = lazy(() => import("@/app/(app)/library/manage/page"));
const MediaDetailPage = lazy(() => import("@/app/(app)/media/[type]/[id]/page"));
const DoubanMediaDetailPage = lazy(() => import("@/app/(app)/media/douban/[id]/page"));
const MyPage = lazy(() => import("@/app/(app)/my/page"));
const SearchPage = lazy(() => import("@/app/(app)/search/page"));
const SettingsIndexPage = lazy(() => import("@/app/(app)/settings/page"));
const SettingsSectionPage = lazy(() => import("@/app/(app)/settings/[section]/page"));
const SubscriptionsPageRoute = lazy(() => import("@/app/(app)/subscriptions/page"));
const SubscriptionDetailPage = lazy(() => import("@/app/(app)/subscriptions/[id]/page"));
const TasksRedirectPage = lazy(() => import("@/app/(app)/tasks/page"));
const PlayPage = lazy(() => import("@/app/play/[mediaItemId]/[[...unit]]/page"));

/**
 * Next App Router 路由表 → react-router 对照:
 * 目录组 (app) 即 <Route element={<AppLayout/>}> 下的所有路由;
 * /play 独立外壳不套工作台;动态段 [x] 变 :x;可选单集段变 :unit?。
 * 静态段与动态段的优先级由 react-router 按特异性自动排序
 * (如 /library/customize 会命中静态路由而不是 /library/:id)。
 */
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <BrowserRouter>
      <Routes>
        <Route element={<RootLayout />}>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/health" element={<HealthPage />} />
          <Route element={<AppLayout />}>
            <Route index element={<HomePage />} />
            <Route path="activity" element={<ActivityPage />} />
            <Route path="collections" element={<CollectionsPage />} />
            <Route path="collections/:id" element={<CollectionDetailPage />} />
            <Route path="transfers" element={<MediaTransferView />} />
            <Route path="jobs" element={<Navigate to="/activity?view=scheduled" replace />} />
            <Route path="cache" element={<CatalogCacheView />} />
            <Route path="discover/:type" element={<DiscoverPage />} />
            <Route
              path="discover/:type/collections/:provider/:collectionId"
              element={<DiscoveryCollectionPage />}
            />
            <Route path="discover/movie/high-score" element={<HighScorePage />} />
            <Route path="discover/movie/top250" element={<Top250Page />} />
            <Route path="discover/people/:id" element={<DiscoveredPersonPage />} />
            <Route path="library" element={<LibraryPage />} />
            <Route path="library/:id" element={<LibraryDetailPage />} />
            <Route path="library/:id/item/:mediaItemId" element={<LibraryItemDetailPage />} />
            <Route path="library/customize" element={<LibraryCustomizePage />} />
            <Route path="library/favorites" element={<FavoritesPage />} />
            <Route path="library/manage" element={<LibraryManagePage />} />
            <Route path="media/:type/:id" element={<MediaDetailPage />} />
            <Route path="media/douban/:id" element={<DoubanMediaDetailPage />} />
            <Route path="my" element={<MyPage />} />
            <Route path="search" element={<SearchPage />} />
            <Route path="settings" element={<SettingsIndexPage />} />
            <Route path="settings/:section" element={<SettingsSectionPage />} />
            <Route path="subscriptions" element={<SubscriptionsPageRoute />} />
            <Route path="subscriptions/:id" element={<SubscriptionDetailPage />} />
            <Route path="tasks" element={<TasksRedirectPage />} />
          </Route>
          <Route element={<PlayLayout />}>
            <Route path="play/:mediaItemId/:unit?" element={<PlayPage />} />
          </Route>
          <Route path="*" element={<NotFound />} />
        </Route>
      </Routes>
    </BrowserRouter>
  </StrictMode>,
);
