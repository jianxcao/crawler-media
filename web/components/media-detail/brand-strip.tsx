"use client";

import { useState } from "react";

import { ArtworkImage } from "@/components/artwork-image";
import { brandLogoUrl, cachedImageUrl } from "@/lib/image-proxy";

export interface BrandStripItem {
  name: string;
  logo_url: string | null;
  kind?: "network" | "studio";
}

const MAX_DISPLAY_BRANDS = 4;

/** 详情页出品方 / 播出平台徽标条（最多展示 4 项：有 Logo 用深色玻璃托底原色渲染，缺失或加载失败退回文字胶囊）。 */
export function BrandStrip({ brands }: { brands?: readonly BrandStripItem[] | null }) {
  const visible = (brands ?? [])
    .filter((brand) => Boolean(brand?.name?.trim()))
    .slice(0, MAX_DISPLAY_BRANDS);
  if (visible.length === 0) return null;

  return (
    <div
      className="mt-3 flex flex-wrap items-center gap-2 max-md:mt-2"
      aria-label="出品与播出平台"
    >
      {visible.map((brand) => (
        <BrandBadge key={`${brand.kind ?? "brand"}:${brand.name}`} brand={brand} />
      ))}
    </div>
  );
}

function BrandBadge({ brand }: { brand: BrandStripItem }) {
  const [failedLogoUrl, setFailedLogoUrl] = useState<string | null>(null);
  const resolvedLogo = brand.logo_url
    ? brandLogoUrl
      ? brandLogoUrl(brand.logo_url)
      : cachedImageUrl(brand.logo_url, "brand-logo")
    : "";
  const showLogo = Boolean(resolvedLogo) && failedLogoUrl !== brand.logo_url;

  if (showLogo) {
    return (
      <span
        title={brand.name}
        className="inline-flex h-7 items-center rounded-md border border-white/[0.1] bg-black/45 px-2.5 py-1 backdrop-blur-sm"
      >
        <ArtworkImage
          src={resolvedLogo}
          alt={brand.name}
          className="h-4 w-auto max-w-[96px] object-contain"
          onError={() => setFailedLogoUrl(brand.logo_url)}
        />
      </span>
    );
  }

  return (
    <span
      title={brand.name}
      className="inline-flex h-7 items-center rounded-md border border-white/[0.1] bg-white/[0.06] px-2.5 py-1 text-caption font-medium text-white/80 backdrop-blur-sm"
    >
      {brand.name}
    </span>
  );
}
