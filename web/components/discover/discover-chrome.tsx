"use client";

import { Link } from "react-router-dom";

import { GlobeIcon } from "@/components/icons";
import { HttpError } from "@/lib/http";
import type { MediaSource, MediaType } from "@/lib/media-types";

export interface DiscoverErrorInfo {
  message: string;
  code?: string;
  hint?: string;
}

export function toErrorInfo(err: unknown): DiscoverErrorInfo {
  if (err instanceof HttpError) {
    const payload = err.details as
      | { code?: string; details?: { service?: string; hint?: string }[] }
      | undefined;
    return {
      message: err.message,
      code: payload?.code,
      hint: payload?.details?.[0]?.hint,
    };
  }
  return { message: (err as Error)?.message || "加载失败，请稍后重试" };
}

export function SourceSwitcher({
  value,
  onChange,
  compact = false,
}: {
  value: MediaSource;
  onChange: (source: MediaSource) => void;
  compact?: boolean;
}) {
  return (
    <div className="segmented-glass flex shrink-0 rounded-full p-1">
      {(["tmdb", "douban"] as const).map((source) => (
        <button
          key={source}
          type="button"
          aria-pressed={value === source}
          onClick={() => onChange(source)}
          className={`segmented-glass__item rounded-full font-semibold transition ${
            compact ? "px-1.5 py-2 text-micro" : "px-4 py-1.5 text-sub"
          }`}
        >
          {source === "tmdb" ? "TMDB" : "豆瓣"}
        </button>
      ))}
    </div>
  );
}

export function MediaTypeSwitcher({
  value,
  onChange,
}: {
  value: MediaType;
  onChange: (type: MediaType) => void;
}) {
  const labels: Record<MediaType, string> = { movie: "电影", tv: "剧集" };
  return (
    <div
      className="segmented-glass flex shrink-0 rounded-full p-1"
      aria-label="内容类型"
    >
      {(["movie", "tv"] as const).map((type) => (
        <button
          key={type}
          type="button"
          aria-pressed={value === type}
          onClick={() => onChange(type)}
          className="segmented-glass__item rounded-full px-1.5 py-2 text-micro font-semibold transition"
        >
          {labels[type]}
        </button>
      ))}
    </div>
  );
}

export function HeroSkeleton({ fullBleed = false }: { fullBleed?: boolean }) {
  return (
    <div
      className={`h-[46vh] min-h-[320px] animate-pulse bg-white/[0.05] max-md:h-[38vh] max-md:min-h-[230px] ${
        fullBleed ? "" : "rounded-2xl ring-1 ring-white/10"
      }`}
    />
  );
}

export function RowItemsSkeleton() {
  return (
    <>
      {Array.from({ length: 8 }, (_, i) => (
        <div
          key={i}
          className="m-row-skel aspect-[2/3] w-[152px] shrink-0 animate-pulse rounded-2xl bg-white/[0.05] max-md:w-[126px] xl:w-[164px]"
        />
      ))}
    </>
  );
}

export function DiscoverSkeleton({ fullBleed = false }: { fullBleed?: boolean }) {
  return (
    <div
      className={`flex-1 overflow-hidden pb-10 ${fullBleed ? "" : "px-6 max-md:px-4"}`}
      aria-busy="true"
      aria-label="发现页加载中"
    >
      <HeroSkeleton fullBleed={fullBleed} />
      {[0, 1].map((row) => (
        <div key={row} className="mt-10">
          <div className="h-4 w-28 animate-pulse rounded bg-white/[0.08]" />
          <div className="mt-4 flex gap-4 overflow-hidden">
            <RowItemsSkeleton />
          </div>
        </div>
      ))}
    </div>
  );
}

export function RowSkeleton({
  stub,
  fullBleed = false,
}: {
  stub: { title: string };
  fullBleed?: boolean;
}) {
  const inset = fullBleed ? "px-[4vw]" : "px-6 max-md:px-4";
  return (
    <section aria-busy="true" aria-label={`「${stub.title}」加载中`}>
      <div className={`mb-3 max-md:mb-2 ${inset}`}>
        <h3 className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
          {stub.title}
        </h3>
      </div>
      <div className={`flex gap-4 overflow-hidden pb-1 pt-1 max-md:gap-3 ${inset}`}>
        <RowItemsSkeleton />
      </div>
    </section>
  );
}

export function DiscoverError({ error, onRetry }: { error: DiscoverErrorInfo; onRetry: () => void }) {
  const unreachable = error.code === "UPSTREAM_UNREACHABLE";
  return (
    <div className="flex flex-1 items-center justify-center px-6">
      <div className="css-glass max-w-md rounded-2xl p-8 text-center">
        {unreachable && (
          <span className="icon-chip mx-auto mb-4 flex size-11 !rounded-2xl">
            <GlobeIcon className="size-5" />
          </span>
        )}
        <p className="text-body-lg font-semibold text-[var(--text)]">
          {unreachable ? "无法连接数据源" : "发现页加载失败"}
        </p>
        <p className="mt-2 break-all text-ui leading-6 text-[var(--text-muted)]">{error.message}</p>
        {unreachable && error.hint && (
          <p className="mt-2 text-sub leading-6 text-[var(--text-faint)]">{error.hint}</p>
        )}
        <div className="mt-5 flex items-center justify-center gap-3">
          {unreachable && (
            <Link
              to="/settings/metadata#proxy"
              className="btn-accent flex h-9 items-center rounded-full px-5 text-ui font-semibold"
            >
              前往网络设置
            </Link>
          )}
          <button
            type="button"
            onClick={onRetry}
            className={`${unreachable ? "btn-glass" : "btn-accent"} h-9 rounded-full px-5 text-ui font-semibold`}
          >
            重试
          </button>
        </div>
      </div>
    </div>
  );
}
