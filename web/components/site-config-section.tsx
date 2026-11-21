"use client";

import { useCallback, useEffect, useMemo, useState } from "react";

import { PlusIcon, ServerIcon } from "@/components/icons";
import { SearchSection } from "@/components/search-settings";
import { CdpBrowserSection } from "@/components/settings/sites/cdp-browser-section";
import { AddSitePanel } from "@/components/settings/sites/site-form";
import { QueueCongestionTip, SiteRow } from "@/components/settings/sites/site-row";
import {
  type CatalogItem,
  listConfiguredSites,
  listSiteBoostStats,
  listSiteCatalog,
  type Site,
  type SiteBoostStats,
} from "@/lib/api/sites";
import { useTabParam } from "@/lib/use-tab-param";
import { useVisiblePolling } from "@/lib/use-visible-polling";

const TABS = [
  { id: "sites", label: "站点接入" },
  { id: "search", label: "搜索分类" },
  { id: "cdp", label: "CDP & 浏览器" },
] as const;

type SiteTab = (typeof TABS)[number]["id"];

function fallbackItem(siteId: string): CatalogItem {
  return { profile_id: siteId, auth_types: [] };
}

/**
 * 「资源站点」分区的动态副标题（设置头部用）。
 */
export function SitesSectionSubtitle({ fallback }: { fallback: string }) {
  const [sites, setSites] = useState<Site[] | null>(null);
  useEffect(() => {
    let alive = true;
    listConfiguredSites()
      .then((rows) => {
        if (alive) setSites(rows);
      })
      .catch(() => {
        /* 拉取失败保持介绍文案，分区主体会展示具体错误 */
      });
    return () => {
      alive = false;
    };
  }, []);

  if (!sites || sites.length === 0) return <>{fallback}</>;
  const disabled = sites.filter((s) => !s.enabled).length;
  if (disabled > 0) {
    return (
      <>
        已接入 {sites.length} 个站点，
        <span className="font-medium text-[var(--warn)]">{disabled} 个已停用</span>
      </>
    );
  }
  return <>已接入 {sites.length} 个站点，全部正常</>;
}

export function SiteConfigSection() {
  const [tab, setTab] = useTabParam<SiteTab>(
    TABS.map((t) => t.id),
    "sites",
  );
  const [catalog, setCatalog] = useState<CatalogItem[]>([]);
  const [configured, setConfigured] = useState<Site[]>([]);
  const [boostStats, setBoostStats] = useState<Record<string, SiteBoostStats>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [expandedSite, setExpandedSite] = useState<string | null>(null);
  const [createPresetNonce, setCreatePresetNonce] = useState(0);

  const catalogMap = useMemo(() => new Map(catalog.map((c) => [c.profile_id, c])), [catalog]);
  const configuredProfileIds = useMemo(
    () => new Set(configured.map((s) => s.profile_id)),
    [configured],
  );
  const availableItems = useMemo(
    () => catalog.filter((c) => !configuredProfileIds.has(c.profile_id)),
    [catalog, configuredProfileIds],
  );

  const load = useCallback(async () => {
    setError(null);
    try {
      const [cat, cfg, boost] = await Promise.all([
        listSiteCatalog(),
        listConfiguredSites(),
        listSiteBoostStats(),
      ]);
      setCatalog(cat);
      setConfigured(cfg);
      setBoostStats(boost);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const anyBoosting = Object.values(boostStats).some((b) => b.boost.enabled === true);
  useVisiblePolling(
    () => {
      void listSiteBoostStats()
        .then(setBoostStats)
        .catch(() => {
          /* 轮询失败静默重试，不打断页面 */
        });
    },
    anyBoosting ? 30000 : null,
  );

  const upsertConfigured = useCallback((next: Site) => {
    setConfigured((prev) => {
      const idx = prev.findIndex((s) => s.id === next.id);
      if (idx === -1) return [...prev, next];
      const copy = [...prev];
      copy[idx] = next;
      return copy;
    });
    void listSiteBoostStats().then(setBoostStats).catch(() => undefined);
  }, []);

  const ordered = useMemo(
    () => [...configured].sort((a, b) => (a.enabled ? 1 : 0) - (b.enabled ? 1 : 0)),
    [configured],
  );

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between gap-3">
        <div className="flex gap-1.5">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              aria-pressed={t.id === tab}
              onClick={() => setTab(t.id)}
              className={`rounded-full px-3.5 py-1.5 text-sub font-medium transition-colors ${
                t.id === tab
                  ? "bg-white/[0.14] text-white"
                  : "text-[var(--text-muted)] hover:bg-white/[0.07] hover:text-[var(--text)]"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
        {tab === "sites" ? (
          <button
            type="button"
            onClick={() => setAdding((v) => !v)}
            disabled={loading}
            className="btn-accent flex shrink-0 items-center gap-1 rounded-full py-1.5 pl-2.5 pr-3.5 text-sub font-semibold disabled:opacity-60"
          >
            <PlusIcon className="size-4" />
            添加站点
          </button>
        ) : (
          <button
            type="button"
            onClick={() => setCreatePresetNonce((n) => n + 1)}
            className="btn-accent flex shrink-0 items-center gap-1 rounded-full py-1.5 pl-2.5 pr-3.5 text-sub font-semibold"
          >
            <PlusIcon className="size-4" />
            新建自定义分类
          </button>
        )}
      </div>

      {tab === "search" ? (
        <SearchSection createRequest={createPresetNonce} />
      ) : tab === "cdp" ? (
        <CdpBrowserSection />
      ) : (
        <>
          {error && (
            <div className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-body text-[#ff6b6b]">
              {error}
            </div>
          )}

          <QueueCongestionTip />

          {adding && (
            <AddSitePanel
              available={availableItems}
              onCreated={(site) => {
                upsertConfigured(site);
                setAdding(false);
              }}
              onCancel={() => setAdding(false)}
              onError={setError}
            />
          )}

          {loading ? (
            <div className="space-y-px overflow-hidden rounded-xl border border-white/[0.08]">
              <div className="h-14 animate-pulse bg-white/[0.04]" />
              <div className="h-14 animate-pulse bg-white/[0.04]" />
            </div>
          ) : configured.length === 0 ? (
            <div className="flex flex-col items-center gap-3 rounded-xl border border-white/[0.08] bg-white/[0.03] px-6 py-12 text-center">
              <span className="icon-chip size-12 !rounded-2xl">
                <ServerIcon className="size-6" />
              </span>
              <div>
                <p className="text-body font-medium text-[var(--text)]">还没有配置任何站点</p>
                <p className="mt-1 text-sub text-[var(--text-muted)]">
                  点击右上角「添加站点」开始接入。
                </p>
              </div>
            </div>
          ) : (
            <div className="divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08] bg-white/[0.03]">
              {ordered.map((site) => (
                <SiteRow
                  key={site.id}
                  item={catalogMap.get(site.profile_id) ?? fallbackItem(site.profile_id)}
                  site={site}
                  boost={boostStats[site.id]}
                  expanded={expandedSite === site.id}
                  onToggle={() =>
                    setExpandedSite((cur) => (cur === site.id ? null : site.id))
                  }
                  onOpen={() => setExpandedSite(site.id)}
                  onChanged={upsertConfigured}
                  onDeleted={(siteId) => {
                    setConfigured((prev) => prev.filter((s) => s.id !== siteId));
                    setExpandedSite((cur) => (cur === siteId ? null : cur));
                  }}
                  onError={setError}
                />
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
