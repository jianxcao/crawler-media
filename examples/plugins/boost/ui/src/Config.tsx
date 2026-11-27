import "./style.css";

import { useEffect, useRef, useState } from "react";
import type { OptionView, PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, FormField } from "@crawler-media/plugin-ui-kit";

const GIB = 1024 ** 3;

interface BoostDraft {
  sites: string[];
  downloader_id: string;
  budget_gib: number | "";
  min_free_disk_gib: number | "";
  hold_days: number | "";
  max_active: number | "";
  max_adds_per_hour: number | "";
  max_adds_per_round: number | "";
  min_seeders: number | "";
  dry_run: boolean;
  auto_cleanup_files: boolean;
  free_only: boolean;
  exclude_hr: boolean;
  enforce_budget_on_all: boolean;
}

function numOr(value: number | "", fallback: number): number {
  return value === "" || !Number.isFinite(value) ? fallback : value;
}

export function toDraft(values?: Readonly<Record<string, unknown>>): BoostDraft {
  const v = values ?? {};
  const rawBudget = v.budget_bytes;
  const budget_gib: number | "" =
    rawBudget === undefined || rawBudget === null || rawBudget === ""
      ? ""
      : Number((Number(rawBudget) / GIB).toFixed(4));
  const rawMinFree = v.min_free_disk_bytes;
  const min_free_disk_gib =
    rawMinFree === undefined || rawMinFree === null || rawMinFree === ""
      ? 0
      : Number((Number(rawMinFree) / GIB).toFixed(4));
  return {
    sites: Array.isArray(v.sites) ? v.sites.map(String) : [],
    downloader_id: String(v.downloader_id ?? ""),
    budget_gib,
    min_free_disk_gib,
    hold_days: Number(v.hold_days ?? 0),
    max_active: Number(v.max_active ?? 20),
    max_adds_per_hour: Number(v.max_adds_per_hour ?? 10),
    max_adds_per_round: Number(v.max_adds_per_round ?? 20),
    min_seeders: Number(v.min_seeders ?? 1),
    dry_run: v.dry_run !== undefined ? v.dry_run === true : true,
    auto_cleanup_files: v.auto_cleanup_files === true,
    free_only: v.free_only !== false,
    exclude_hr: v.exclude_hr !== undefined ? v.exclude_hr === true : true,
    enforce_budget_on_all: v.enforce_budget_on_all === true,
  };
}

export function toPayload(draft: BoostDraft): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    sites: draft.sites,
    min_free_disk_bytes: Math.max(0, Math.round(numOr(draft.min_free_disk_gib, 0) * GIB)),
    hold_days: Math.max(0, Math.min(365, Math.trunc(numOr(draft.hold_days, 0)))),
    max_active: Math.max(1, Math.min(200, Math.trunc(numOr(draft.max_active, 20)))),
    max_adds_per_hour: Math.max(1, Math.min(100, Math.trunc(numOr(draft.max_adds_per_hour, 10)))),
    max_adds_per_round: Math.max(1, Math.min(100, Math.trunc(numOr(draft.max_adds_per_round, 20)))),
    min_seeders: Math.max(0, Math.trunc(numOr(draft.min_seeders, 1))),
    dry_run: draft.dry_run,
    auto_cleanup_files: draft.auto_cleanup_files,
    free_only: draft.free_only,
    exclude_hr: draft.exclude_hr,
    enforce_budget_on_all: draft.enforce_budget_on_all,
  };
  if (draft.budget_gib !== "" && Number.isFinite(draft.budget_gib)) {
    payload.budget_bytes = Math.max(0, Math.round(draft.budget_gib * GIB));
  }
  if (draft.downloader_id) {
    payload.downloader_id = draft.downloader_id;
  }
  return payload;
}

type NumericFieldKey =
  | "budget_gib"
  | "min_free_disk_gib"
  | "hold_days"
  | "max_active"
  | "max_adds_per_hour"
  | "max_adds_per_round"
  | "min_seeders";

const NUMERIC_FIELDS: Array<{
  key: NumericFieldKey;
  id: string;
  label: string;
  min: number;
  max?: number;
}> = [
  { key: "budget_gib", id: "boost-budget", label: "空间预算 (GiB)", min: 0 },
  { key: "min_free_disk_gib", id: "boost-min-free", label: "磁盘最低保留空闲 (GiB)", min: 0 },
  { key: "hold_days", id: "boost-hold", label: "H&R / 汰换保留天数 (0-365)", min: 0, max: 365 },
  { key: "max_active", id: "boost-max-active", label: "最大活动任务数", min: 1, max: 200 },
  { key: "max_adds_per_hour", id: "boost-max-hour", label: "每小时最大添种", min: 1, max: 100 },
  { key: "max_adds_per_round", id: "boost-max-round", label: "单轮最大添种 (1-100)", min: 1, max: 100 },
  { key: "min_seeders", id: "boost-min-seeders", label: "最低做种人数", min: 0 },
];

type ToggleFieldKey =
  | "free_only"
  | "exclude_hr"
  | "dry_run"
  | "auto_cleanup_files"
  | "enforce_budget_on_all";

const TOGGLE_FIELDS: Array<{ key: ToggleFieldKey; label: string }> = [
  { key: "free_only", label: "仅抓免费 (Free)" },
  { key: "exclude_hr", label: "跳过 H&R" },
  { key: "dry_run", label: "模拟运行 (Dry-run)" },
  { key: "auto_cleanup_files", label: "自动清理到期文件" },
  { key: "enforce_budget_on_all", label: "对全部下载执行配额门禁" },
];

function BoostQuotaGrid({
  draft,
  downloaders,
  disabled,
  onPatch,
}: {
  draft: BoostDraft;
  downloaders: OptionView[];
  disabled: boolean;
  onPatch: (partial: Partial<BoostDraft>) => void;
}) {
  return (
    <div className="cm-boost-grid">
      <FormField label="目标下载器" htmlFor="boost-dl">
        <select
          id="boost-dl"
          disabled={disabled}
          value={draft.downloader_id}
          onChange={(e) => onPatch({ downloader_id: e.target.value })}
          className="cm-boost-select"
        >
          <option value="">默认下载器</option>
          {draft.downloader_id.trim().length > 0 &&
            !downloaders.some((d) => d.id === draft.downloader_id) && (
              <option value={draft.downloader_id}>{`${draft.downloader_id} (已失效)`}</option>
            )}
          {downloaders.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
            </option>
          ))}
        </select>
      </FormField>

      {NUMERIC_FIELDS.map((field) => (
        <FormField key={field.key} label={field.label} htmlFor={field.id}>
          <input
            id={field.id}
            type="number"
            step="any"
            min={field.min}
            max={field.max}
            disabled={disabled}
            value={draft[field.key]}
            onChange={(e) => {
              const raw = e.target.value.trim();
              if (raw === "") {
                onPatch({ [field.key]: "" });
                return;
              }
              const parsed = Number(raw);
              if (Number.isFinite(parsed)) {
                onPatch({ [field.key]: parsed });
              }
            }}
            className="cm-boost-input"
          />
        </FormField>
      ))}
    </div>
  );
}

function BoostPolicyToggles({
  draft,
  disabled,
  onPatch,
}: {
  draft: BoostDraft;
  disabled: boolean;
  onPatch: (partial: Partial<BoostDraft>) => void;
}) {
  return (
    <div className="cm-boost-row">
      {TOGGLE_FIELDS.map((item) => (
        <label key={item.key} className="cm-boost-chip">
          <input
            type="checkbox"
            disabled={disabled}
            checked={draft[item.key]}
            onChange={(e) => onPatch({ [item.key]: e.target.checked })}
          />
          <span>{item.label}</span>
        </label>
      ))}
    </div>
  );
}

export default function BoostConfig({ host, config }: PluginSurfaceProps) {
  const [sites, setSites] = useState<OptionView[]>([]);
  const [downloaders, setDownloaders] = useState<OptionView[]>([]);
  const [draft, setDraft] = useState<BoostDraft>(() => toDraft(config?.values));
  const draftRef = useRef<BoostDraft>(draft);
  draftRef.current = draft;
  const lastEmittedPayloadRef = useRef<Readonly<Record<string, unknown>> | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (config?.values !== undefined && config.values === lastEmittedPayloadRef.current) {
      return;
    }
    lastEmittedPayloadRef.current = null;
    const synced = toDraft(config?.values);
    draftRef.current = synced;
    setDraft(synced);
  }, [config?.values]);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      host.catalogs.list("sites", { limit: 100 }).catch(() => ({ items: [] })),
      host.catalogs.list("downloaders", { limit: 100 }).catch(() => ({ items: [] })),
    ]).then(([sRes, dRes]) => {
      if (cancelled) return;
      setSites(sRes.items);
      setDownloaders(dRes.items);
    });
    return () => {
      cancelled = true;
    };
  }, [host]);

  const editable = config?.editable !== false;
  const disabled = !editable || saving;
  const siteOptions: OptionView[] = [...sites];
  for (const sid of draft.sites) {
    const trimmed = sid.trim();
    if (trimmed && !siteOptions.some((s) => s.id === trimmed)) {
      siteOptions.push({ id: trimmed, name: `${trimmed} (已失效)` });
    }
  }

  function patch(nextPartial: Partial<BoostDraft>) {
    const next = { ...draftRef.current, ...nextPartial };
    draftRef.current = next;
    const payload = toPayload(next);
    lastEmittedPayloadRef.current = payload;
    setDraft(next);
    config?.onDraftChange(payload);
  }

  async function handleSave() {
    if (!config) return;
    setError(null);
    setSaving(true);
    try {
      await config.save(toPayload(draftRef.current));
    } catch (err) {
      setError((err as Error).message || "保存刷流配置失败");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="cm-boost-shell">
      <Card
        title="自动刷流策略与配额设置"
        description="选择参与刷流的 PT 站点、下载器目标、空间预算及安全淘汰保护期。"
      >
        <div className="cm-boost-shell">
          <FormField label="参与刷流的站点">
            <div className="cm-boost-row">
              {siteOptions.length === 0 ? (
                <span>暂无已接入站点</span>
              ) : (
                siteOptions.map((site) => (
                  <label key={site.id} className="cm-boost-chip">
                    <input
                      type="checkbox"
                      disabled={disabled}
                      checked={draft.sites.includes(site.id)}
                      onChange={() =>
                        patch({
                          sites: draft.sites.includes(site.id)
                            ? draft.sites.filter((s) => s !== site.id)
                            : [...draft.sites, site.id],
                        })
                      }
                    />
                    <span>{site.name}</span>
                  </label>
                ))
              )}
            </div>
          </FormField>

          <BoostQuotaGrid draft={draft} downloaders={downloaders} disabled={disabled} onPatch={patch} />
          <BoostPolicyToggles draft={draft} disabled={disabled} onPatch={patch} />

          {error && <p role="alert">{error}</p>}

          <ActionBar
            align="end"
            items={[
              {
                id: "save",
                label: "保存刷流配置",
                tone: "primary",
                disabled: !editable,
                loading: saving,
                onClick: () => void handleSave(),
              },
            ]}
          />
        </div>
      </Card>
    </div>
  );
}
