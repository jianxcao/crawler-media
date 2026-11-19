"use client";

import { Link } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { ArrowLeftIcon } from "@/components/icons";
import { NetflixBackButton } from "@/components/netflix/back-button";
import { PageNav } from "@/components/page-nav";

export function ItemDetailFallback({
  failed,
  hidePageNav,
  navFallback,
  onBack,
}: {
  failed: boolean;
  hidePageNav: boolean;
  navFallback: { label: string; href: string };
  onBack: () => void;
}) {
  return (
    <div className="ambient-fallback flex h-full flex-col">
      {!hidePageNav && <PageNav title="" fallback={navFallback} />}
      {hidePageNav && <NetflixBackButton onBack={onBack} />}
      {failed ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center">
          <p className="text-body-lg font-semibold text-[var(--text)]">未能加载该条目</p>
          <p className="max-w-sm text-ui leading-6 text-[var(--text-muted)]">
            条目可能已被删除或重新识别为其他作品，请返回后查看。
          </p>
          <Link
            to={navFallback.href}
            replace
            className="btn-glass flex items-center gap-2 px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            <ArrowLeftIcon className="size-4" />
            返回{navFallback.label}
          </Link>
        </div>
      ) : (
        <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在读取本地刮削信息…
        </div>
      )}
    </div>
  );
}
