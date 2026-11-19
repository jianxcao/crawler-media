"use client";

import { createContext, useCallback, useEffect, useMemo, useState } from "react";

import {
  forgetDownloadTargetPref,
  listDownloadTargetPrefs,
  listDownloaders,
  type DownloadTargetPref,
} from "@/lib/api/downloaders";

/** 手动选种上下文：SearchResults 顶层拉取订阅标题后灌入，行组件按需消费。 */
export const GrabContext = createContext<{ id: string; title: string } | null>(null);

/**
 * 保存位置记忆上下文。
 *
 * 整页只拉一次（最多 8 条），下载按钮按种子分类查表决定弹确认条还是完整弹窗。
 * 放 context 而不是每个按钮各拉一次：一页几十条结果，逐行请求毫无必要。
 */
export const DownloadTargetPrefContext = createContext<{
  byCategory: Map<string, DownloadTargetPref>;
  /** 记忆指向的固定目录是否已不在候选里（库被删、路径映射改了）；非 dir 恒 false */
  isStaleDir: (pref: DownloadTargetPref) => boolean;
  forget: (category: string) => Promise<void>;
  refresh: () => Promise<void>;
}>({
  byCategory: new Map(),
  isStaleDir: () => false,
  forget: async () => {},
  refresh: async () => {},
});

/** 拉取并维护当前登录者的保存位置记忆。 */
export function useDownloadTargetPrefs(enabled: boolean) {
  const [byCategory, setByCategory] = useState<Map<string, DownloadTargetPref>>(new Map());
  // 目录候选来自下载器配置（默认保存目录 + 各映射的本机侧目录）。整页拉
  // 一次，确认条据此判断记住的目录还在不在——放到确认条里现拉会让它没法立刻出现
  const [dirs, setDirs] = useState<Set<string> | null>(null);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    // 拉不到就当作没有记忆——退化成每次弹窗，是安全的一侧
    const rows = await listDownloadTargetPrefs().catch(() => [] as DownloadTargetPref[]);
    setByCategory(new Map(rows.map((r) => [r.category, r])));
  }, [enabled]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    void listDownloaders()
      .then((rows) => {
        if (cancelled) return;
        // 候选目录取 path_maps 的本机侧前缀（from），连接正常（status=active）的才可信
        const usable = rows.filter((d) => d.status === "active");
        const all = new Set<string>();
        for (const d of usable) {
          for (const m of d.path_maps ?? []) all.add(m.from.replace(/\/+$/, ""));
        }
        setDirs(all);
      })
      // 拉不到就置空集合以外的 null：宁可不判失效，也不要误报把好记忆判死
      .catch(() => setDirs(null));
    return () => {
      cancelled = true;
    };
  }, [enabled]);

  const isStaleDir = useCallback(
    (pref: DownloadTargetPref) => {
      if (pref.kind !== "dir" || !pref.save_path || dirs === null) return false;
      return !dirs.has(pref.save_path.replace(/\/+$/, ""));
    },
    [dirs],
  );

  const forget = useCallback(async (category: string) => {
    // 先本地摘掉再打请求：用户点了「不再记住」就不该再看到这条默认
    setByCategory((prev) => {
      const next = new Map(prev);
      next.delete(category);
      return next;
    });
    await forgetDownloadTargetPref(category).catch(() => undefined);
  }, []);

  return useMemo(
    () => ({ byCategory, isStaleDir, forget, refresh }),
    [byCategory, isStaleDir, forget, refresh],
  );
}
