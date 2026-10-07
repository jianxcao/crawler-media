import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Route, Routes } from "react-router-dom";

import RootLayout from "@/app/layout";
import AppLayout from "@/app/(app)/layout";
import PlayLayout from "@/app/play/layout";
import LoginPage from "@/app/login/page";
import HealthPage from "@/app/health/page";
import HomePage from "@/app/(app)/page";
import ActivityPage from "@/app/(app)/activity/page";
import { MediaTransferView } from "@/components/media-transfer-view";
import { ScheduledJobsView } from "@/components/scheduled-jobs-view";
import { CatalogCacheView } from "@/components/catalog-cache-view";
import CollectionsPage from "@/app/(app)/collections/page";
import CollectionDetailPage from "@/app/(app)/collections/[id]/page";
import DiscoverPage from "@/app/(app)/discover/[type]/page";
import DiscoveryCollectionPage from "@/app/(app)/discover/[type]/collections/[provider]/[collectionId]/page";
import HighScorePage from "@/app/(app)/discover/movie/high-score/page";
import Top250Page from "@/app/(app)/discover/movie/top250/page";
import DiscoveredPersonPage from "@/app/(app)/discover/people/[id]/page";
import LibraryPage from "@/app/(app)/library/page";
import LibraryDetailPage from "@/app/(app)/library/[id]/page";
import LibraryItemDetailPage from "@/app/(app)/library/[id]/item/[mediaItemId]/page";
import LibraryCustomizePage from "@/app/(app)/library/customize/page";
import FavoritesPage from "@/app/(app)/library/favorites/page";
import LibraryManagePage from "@/app/(app)/library/manage/page";
import MediaDetailPage from "@/app/(app)/media/[type]/[id]/page";
import DoubanMediaDetailPage from "@/app/(app)/media/douban/[id]/page";
import MyPage from "@/app/(app)/my/page";
import SearchPage from "@/app/(app)/search/page";
import SettingsIndexPage from "@/app/(app)/settings/page";
import SettingsSectionPage from "@/app/(app)/settings/[section]/page";
import SubscriptionsPageRoute from "@/app/(app)/subscriptions/page";
import SubscriptionDetailPage from "@/app/(app)/subscriptions/[id]/page";
import TasksRedirectPage from "@/app/(app)/tasks/page";
import PlayPage from "@/app/play/[mediaItemId]/[[...unit]]/page";
import { NotFound } from "@/src/not-found";

/**
 * Next App Router 路由表 → react-router 对照:
 * 目录组 (app) 即 <Route element={<AppLayout/>}> 下的所有路由;
 * /play 独立外壳不套工作台;动态段 [x] 变 :x;[[...unit]] 变 :unit*。
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
            <Route path="jobs" element={<ScheduledJobsView />} />
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
            <Route path="play/:mediaItemId/:unit*" element={<PlayPage />} />
          </Route>
          <Route path="*" element={<NotFound />} />
        </Route>
      </Routes>
    </BrowserRouter>
  </StrictMode>,
);
