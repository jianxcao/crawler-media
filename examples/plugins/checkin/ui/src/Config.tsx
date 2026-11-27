import "./style.css";

import { useEffect, useState } from "react";
import type { OptionView, PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, FormField } from "@crawler-media/plugin-ui-kit";

export default function CheckinConfig({ host, config }: PluginSurfaceProps) {
  const [sites, setSites] = useState<OptionView[]>([]);
  const [intervalSecs, setIntervalSecs] = useState<number>(() =>
    Number(config?.values?.interval_secs ?? 86400),
  );
  const [siteId, setSiteId] = useState<string>(() =>
    String(config?.values?.site_id ?? ""),
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setIntervalSecs(Number(config?.values?.interval_secs ?? 86400));
    setSiteId(String(config?.values?.site_id ?? ""));
  }, [config?.values?.interval_secs, config?.values?.site_id]);

  useEffect(() => {
    let cancelled = false;
    host.catalogs
      .list("sites", { limit: 100 })
      .then((res) => {
        if (!cancelled) setSites(res.items);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [host]);

  const editable = config?.editable !== false;

  function emitDraft(nextInterval: number, nextSite: string) {
    const next: Record<string, unknown> = { interval_secs: nextInterval };
    if (nextSite) next.site_id = nextSite;
    config?.onDraftChange(next);
  }

  async function handleSave() {
    if (!config) return;
    setError(null);
    if (!Number.isInteger(intervalSecs) || intervalSecs < 3600 || intervalSecs > 86400) {
      setError("签到调度周期必须在 3600 至 86400 秒之间。");
      return;
    }
    setSaving(true);
    try {
      const next: Record<string, unknown> = { interval_secs: intervalSecs };
      if (siteId) next.site_id = siteId;
      await config.save(next);
    } catch (err) {
      setError((err as Error).message || "保存配置失败");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="cm-checkin-shell">
      <Card
        title="站点自动签到设置"
        description="设置每日自动签到的巡检间隔与默认优先站。"
      >
        <div className="cm-checkin-shell">
          <FormField
            label="巡检周期（秒）"
            htmlFor="checkin-interval"
            description="允许范围：3600 ~ 86400（默认 86400 即每 24 小时一次）"
            error={error ?? undefined}
          >
            <input
              id="checkin-interval"
              type="number"
              min={3600}
              max={86400}
              disabled={!editable || saving}
              value={intervalSecs}
              onChange={(e) => {
                const v = Number(e.target.value);
                setIntervalSecs(v);
                emitDraft(v, siteId);
              }}
              className="cm-checkin-input"
            />
          </FormField>

          <FormField
            label="默认站点（留空表示全部已启用站点）"
            htmlFor="checkin-default-site"
          >
            <select
              id="checkin-default-site"
              disabled={!editable || saving}
              value={siteId}
              onChange={(e) => {
                const v = e.target.value;
                setSiteId(v);
                emitDraft(intervalSecs, v);
              }}
              className="cm-checkin-select"
            >
              <option value="">全部已启用站点</option>
              {siteId.trim().length > 0 && !sites.some((s) => s.id === siteId) && (
                <option value={siteId}>{`${siteId} (已失效)`}</option>
              )}
              {sites.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </FormField>

          <ActionBar
            align="end"
            items={[
              {
                id: "save",
                label: "保存签到设置",
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
