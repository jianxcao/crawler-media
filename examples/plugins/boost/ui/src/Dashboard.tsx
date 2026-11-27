import "./style.css";

import { useEffect, useState } from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, StatBadge, StatusPill } from "@crawler-media/plugin-ui-kit";

import { type BoostFullSnapshot, formatBytes } from "./shared";

export default function BoostDashboard({ host }: PluginSurfaceProps) {
  const [snap, setSnap] = useState<BoostFullSnapshot | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    host
      .query("boost.snapshot", {})
      .then((res) => {
        if (!cancelled) setSnap(res as unknown as BoostFullSnapshot);
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
      title="自动刷流遥测"
      description={loading ? "正在加载配额账本..." : `托管任务 ${(snap?.tasks ?? []).length} 个`}
      actions={
        snap ? (
          <StatusPill
            label={`磁盘 ${snap.disk.status}`}
            tone={
              snap.disk.status === "available" || snap.disk.status === "ok"
                ? "ok"
                : "warn"
            }
          />
        ) : undefined
      }
      footer={
        <ActionBar
          align="end"
          items={[
            {
              id: "open-boost-app",
              label: "打开刷流管理",
              tone: "primary",
              onClick: () => void host.navigation.open("plugin", { surface_id: "app" }),
            },
          ]}
        />
      }
    >
      <div className="cm-boost-stats">
        <StatBadge label="已用空间" value={formatBytes(snap?.used_bytes)} />
        <StatBadge label="预留空间" value={formatBytes(snap?.reserved_bytes)} />
        <StatBadge
          label="待清理文件"
          value={formatBytes(snap?.retained_file_bytes)}
          tone={(snap?.retained_file_bytes ?? 0) > 0 ? "warn" : "neutral"}
        />
      </div>
    </Card>
  );
}
