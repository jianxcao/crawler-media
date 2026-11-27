import "./style.css";

import { useEffect, useState } from "react";
import type { PluginSurfaceProps, Receipt } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, StatBadge, StatusPill } from "@crawler-media/plugin-ui-kit";

import { loadCheckinContext, receiptLabel, receiptTone } from "./shared";

export default function CheckinDashboard({ host }: PluginSurfaceProps) {
  const [siteCount, setSiteCount] = useState(0);
  const [latest, setLatest] = useState<Receipt | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    loadCheckinContext(host)
      .then((res) => {
        if (cancelled) return;
        setSiteCount(res.sites.length);
        setLatest(res.receipts[0] ?? null);
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  return (
    <Card
      title="站点签到状态"
      description={loading ? "正在同步站点回执..." : `已接管 ${siteCount} 个站点`}
      actions={
        latest ? (
          <StatusPill label={receiptLabel(latest.state)} tone={receiptTone(latest.state)} />
        ) : undefined
      }
      footer={
        <ActionBar
          align="end"
          items={[
            {
              id: "open-app",
              label: "打开签到工作台",
              tone: "primary",
              onClick: () => void host.navigation.open("plugin", { surface_id: "app" }),
            },
          ]}
        />
      }
    >
      <div className="cm-checkin-stats">
        <StatBadge label="接入站点数" value={siteCount} />
        <StatBadge
          label="最新状态"
          value={latest ? receiptLabel(latest.state) : "暂无记录"}
          tone={latest ? receiptTone(latest.state) : "neutral"}
        />
      </div>
    </Card>
  );
}
