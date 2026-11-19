"use client";

import type { ReactNode } from "react";

import type { PathMapping, PathProbe } from "@/lib/api/downloaders";

import { PATH_STATE_LABEL } from "./downloader-meta";

/* —— 详情排版原语（与站点页同款）：段标签在桌面抽成左侧固定列，内容左缘对齐 —— */

export function DetailSection({ label, children }: { label: string; children: ReactNode }) {
  return (
    <section className="grid gap-1.5 py-3 first:pt-1 last:pb-1 sm:grid-cols-[46px_minmax(0,1fr)] sm:gap-x-4 sm:gap-y-0">
      <p className="text-micro font-medium tracking-wide text-[var(--text-faint)] sm:pt-2">
        {label}
      </p>
      <div className="min-w-0 space-y-2">{children}</div>
    </section>
  );
}

/** 统计区容器：等宽列，不给底色和描边，层级靠对齐与字重撑住 */
export function StatGrid({ children }: { children: ReactNode }) {
  return <div className="grid grid-cols-2 gap-x-5 gap-y-3 sm:grid-cols-4">{children}</div>;
}

/**
 * 单个信息项：淡色小标签在上、值在下。带 href 时值是可点的外链（新窗口打开）；
 * wide 让长值（地址、目录）横跨两列，不与短值抢宽度。
 */
export function DetailStat({
  label,
  value,
  href,
  title,
  wide = false,
}: {
  label: string;
  value: string;
  href?: string;
  title?: string;
  wide?: boolean;
}) {
  const valueClass = "mt-0.5 block truncate text-ui font-semibold text-[var(--text)] max-sm:text-sub";
  return (
    <div className={`min-w-0 ${wide ? "col-span-2" : ""}`}>
      <p className="truncate text-micro text-[var(--text-faint)]">{label}</p>
      {href ? (
        <a
          href={href}
          target="_blank"
          rel="noreferrer"
          title={title}
          onClick={(e) => e.stopPropagation()}
          className={`${valueClass} underline decoration-transparent underline-offset-4 transition-colors hover:text-white/80 hover:decoration-white/50`}
        >
          {value}
        </a>
      ) : (
        <p className={valueClass} title={value}>
          {value}
        </p>
      )}
    </div>
  );
}

/**
 * 映射表。每行右侧挂本地侧的体检结论：配置"看起来对"而运行时挂载失效时，
 * 这里是唯一能让用户不靠命令行就看出"目录是空的"的地方——那正是文案
 * "请检查路径映射"把人引过来之后，必须能给出答案的位置。
 */
export function PathMappingTable({
  mappings,
  health,
}: {
  mappings: PathMapping[];
  health: PathProbe[];
}) {
  if (mappings.length === 0) {
    return (
      <p className="pt-1.5 text-sub text-[var(--text-muted)]">
        未配置——本机与下载器看到的是同一套路径。
      </p>
    );
  }
  const probeOf = (local: string) => health.find((p) => p.local === local);
  return (
    <div className="overflow-hidden rounded-lg border border-white/[0.07]">
      <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-x-3 bg-white/[0.03] px-3 py-1.5 text-micro font-medium tracking-wide text-[var(--text-faint)]">
        <span>本机路径</span>
        <span aria-hidden="true" className="w-4" />
        <span>下载器视角</span>
      </div>
      <ul className="divide-y divide-white/[0.05]">
        {mappings.map((m, i) => {
          const probe = probeOf(m.local);
          const broken = probe && probe.state !== "ok";
          return (
            <li key={`${m.local}→${m.remote}:${i}`} className="px-3 py-2">
              <div className="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-x-3">
                <span className="flex min-w-0 items-center gap-2">
                  <code
                    className={`truncate font-mono text-sub ${broken ? "text-[var(--danger)]" : "text-[var(--text)]"}`}
                    title={m.local}
                  >
                    {m.local}
                  </code>
                  {broken && (
                    <span className="shrink-0 rounded-full bg-[color-mix(in_oklab,var(--danger)_14%,transparent)] px-1.5 py-px text-micro font-medium text-[var(--danger)]">
                      {PATH_STATE_LABEL[probe.state as keyof typeof PATH_STATE_LABEL]}
                    </span>
                  )}
                </span>
                <span aria-hidden="true" className="w-4 text-center text-[var(--text-faint)]">
                  →
                </span>
                <code className="truncate font-mono text-sub text-[var(--text)]" title={m.remote}>
                  {m.remote}
                </code>
              </div>
              {broken && (
                <p className="mt-1 text-caption leading-snug text-[var(--text-muted)]">
                  {probe.detail}
                </p>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
