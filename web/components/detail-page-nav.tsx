"use client";

import type { ReactNode } from "react";

import { NetflixBackButton, NetflixPageActions } from "@/components/netflix/back-button";
import { PageNav, type PageNavFallback } from "@/components/page-nav";
import { useBackNavigation } from "@/lib/back-navigation";
import { useTheme } from "@/lib/ui-prefs";
import { useIsMobile } from "@/lib/use-media-query";

/** Netflix 桌面端隐藏 PageNav（避免被 fixed z-40 顶栏遮挡），改用左/右上角浮键。 */
export function useIsNetflixDesktop(): boolean {
  const themeId = useTheme().id;
  const isMobile = useIsMobile();
  return themeId === "netflix" && !isMobile;
}

/**
 * 详情页与子页面共用的导航封装：把银玻璃/移动端的 PageNav 与 Netflix 桌面的
 * NetflixBackButton / NetflixPageActions 收口在一处，避免业务视图各自判断主题。
 */
export function DetailPageNav({
  title,
  fallback,
  onBack,
  actions,
  className = "",
}: {
  title: string;
  fallback: PageNavFallback;
  onBack?: () => void;
  actions?: ReactNode;
  className?: string;
}) {
  const isNfDesktop = useIsNetflixDesktop();
  const fallbackBack = useBackNavigation(fallback.href);
  const handleBack = onBack ?? fallbackBack;

  if (isNfDesktop) {
    return (
      <>
        <NetflixBackButton onBack={handleBack} />
        {actions ? <NetflixPageActions>{actions}</NetflixPageActions> : null}
      </>
    );
  }

  return <PageNav title={title} fallback={fallback} actions={actions} className={className} />;
}
