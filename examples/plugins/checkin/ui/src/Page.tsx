import "./style.css";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  isTerminalOperation,
  type BrowserAvailability,
  type OptionView,
  type PluginSurfaceProps,
  type Receipt,
} from "@crawler-media/plugin-ui-sdk";
import {
  ActionBar,
  Card,
  DataTable,
  EmptyState,
  PageHeader,
  StatBadge,
  StatusPill,
} from "@crawler-media/plugin-ui-kit";

import {
  formatIsoShort,
  loadCheckinContext,
  receiptLabel,
  receiptTone,
} from "./shared";

function CheckinBody({
  sites,
  siteId,
  receipts,
  availability,
  busyAction,
  error,
  onSelectSite,
  onRunTask,
}: {
  sites: OptionView[];
  siteId: string;
  receipts: Receipt[];
  availability: BrowserAvailability | null;
  busyAction: string | null;
  error: string | null;
  onSelectSite: (nextId: string) => void;
  onRunTask: () => void;
}) {
  const latest = receipts[0] ?? null;
  return (
    <>
      <Card title="目标站点与环境就绪度">
        <div className="cm-checkin-controls">
          <select
            aria-label="选择签到站点"
            value={siteId}
            onChange={(e) => onSelectSite(e.target.value)}
            className="cm-checkin-select"
          >
            {sites.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </select>

          {availability && (
            <StatusPill
              label={
                availability.status === "ready"
                  ? "浏览器维护可用"
                  : `浏览器不可用 (${availability.missing_requirements.join(", ") || "未启用"})`
              }
              tone={availability.status === "ready" ? "ok" : "warn"}
            />
          )}

          <button
            type="button"
            disabled={busyAction !== null}
            onClick={onRunTask}
            className="cm-plugin-btn"
          >
            {busyAction === "task-run" ? "执行中..." : "立即运行调度任务"}
          </button>
        </div>
      </Card>

      <div className="cm-checkin-stats">
        <StatBadge label="已接入站点" value={sites.length} />
        <StatBadge label="最近回执数" value={receipts.length} />
        <StatBadge
          label="最近状态"
          value={latest ? receiptLabel(latest.state) : "无记录"}
          tone={latest ? receiptTone(latest.state) : "neutral"}
        />
      </div>

      {error && (
        <Card title="提示">
          <p role="alert">{error}</p>
        </Card>
      )}

      <Card title="签到与会话回执记录">
        <DataTable
          rows={receipts}
          getRowKey={(row, idx) => `${row.site_id}-${row.observed_at}-${idx}`}
          emptyMessage="该站点暂无签到回执记录"
          columns={[
            {
              key: "state",
              header: "结果",
              render: (row) => (
                <StatusPill label={receiptLabel(row.state)} tone={receiptTone(row.state)} />
              ),
            },
            {
              key: "detail",
              header: "详情 / 凭证证据",
              render: (row) => row.detail ?? "-",
            },
            {
              key: "observed_at",
              header: "观测时间",
              render: (row) => formatIsoShort(row.observed_at),
            },
          ]}
        />
      </Card>
    </>
  );
}

function useCheckinActions(
  host: PluginSurfaceProps["host"],
  siteId: string,
  refresh: (targetSiteId?: string) => Promise<void>,
  setBusyAction: (v: string | null) => void,
  setError: (v: string | null) => void,
) {
  const unwatchRef = useRef<(() => void) | null>(null);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;

  useEffect(() => {
    return () => {
      unwatchRef.current?.();
      unwatchRef.current = null;
    };
  }, []);

  async function triggerMaintenance(action: "check-in" | "verify-session") {
    if (!siteId) return;
    setBusyAction(action);
    setError(null);
    try {
      const resolved = await host.objects.resolve("site", siteId);
      const op = await host.commands.submit(
        "site.maintenance",
        { site_handle: resolved.handle, action },
        {
          expectedRevision: resolved.revision,
          idempotencyKey: `checkin-${action}-${siteId}-${Date.now()}`,
        },
      );
      host.ui.toast({
        message: action === "check-in" ? "已提交签到指令" : "已提交会话校验指令",
        level: "info",
      });
      unwatchRef.current?.();
      unwatchRef.current = host.operations.watch(op.operation_id, (snap) => {
        if (isTerminalOperation(snap.state)) {
          setBusyAction(null);
          unwatchRef.current?.();
          unwatchRef.current = null;
          if (snap.state === "succeeded") {
            host.ui.toast({ message: "站点维护执行完成", level: "info" });
          } else {
            host.ui.toast({
              message: snap.error ?? `执行结束: ${snap.state} (${snap.phase ?? ""})`,
              level: "warning",
            });
          }
          void refreshRef.current();
        }
      });
    } catch (err) {
      setBusyAction(null);
      const msg = (err as Error).message || "提交维护指令失败";
      setError(msg);
      host.ui.toast({ message: msg, level: "error" });
    }
  }

  async function runScheduledCheckin() {
    setBusyAction("task-run");
    setError(null);
    try {
      const op = await host.tasks.run(
        "check-in",
        siteId ? { site_id: siteId } : {},
        { expectedRevision: "0", idempotencyKey: `checkin-task-${Date.now()}` },
      );
      host.ui.toast({ message: `调度任务已触发 (${op.operation_id})`, level: "info" });
      await refresh(siteId);
    } catch (err) {
      const msg = (err as Error).message || "触发调度任务失败";
      setError(msg);
      host.ui.toast({ message: msg, level: "error" });
    } finally {
      setBusyAction(null);
    }
  }

  return { triggerMaintenance, runScheduledCheckin };
}

export default function CheckinPage({ host }: PluginSurfaceProps) {
  const [sites, setSites] = useState<OptionView[]>([]);
  const [siteId, setSiteId] = useState<string>("");
  const [receipts, setReceipts] = useState<Receipt[]>([]);
  const [availability, setAvailability] = useState<BrowserAvailability | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reqSeqRef = useRef(0);
  const siteIdRef = useRef(siteId);
  siteIdRef.current = siteId;

  const refresh = useCallback(
    async (targetSiteId?: string) => {
      const seq = ++reqSeqRef.current;
      setLoading(true);
      setError(null);
      try {
        const res = await loadCheckinContext(host, targetSiteId ?? siteIdRef.current);
        if (seq !== reqSeqRef.current) return;
        siteIdRef.current = res.activeSiteId;
        setSites(res.sites);
        setSiteId(res.activeSiteId);
        setReceipts(res.receipts);
        setAvailability(res.availability);
      } catch (err) {
        if (seq !== reqSeqRef.current) return;
        setError((err as Error).message || "读取签到状态失败");
      } finally {
        if (seq === reqSeqRef.current) {
          setLoading(false);
        }
      }
    },
    [host],
  );

  useEffect(() => {
    void refresh();
    return () => {
      reqSeqRef.current += 1;
    };
  }, [refresh]);

  const { triggerMaintenance, runScheduledCheckin } = useCheckinActions(
    host,
    siteId,
    refresh,
    setBusyAction,
    setError,
  );
  const latest = receipts[0] ?? null;

  return (
    <div className="cm-checkin-shell">
      <PageHeader
        title="站点签到控制台"
        subtitle="按站点查看签到回执、浏览器可用性与手动触发维护"
        badge={
          latest ? (
            <StatusPill label={receiptLabel(latest.state)} tone={receiptTone(latest.state)} />
          ) : undefined
        }
        actions={
          <ActionBar
            items={[
              { id: "refresh", label: "刷新", loading, onClick: () => void refresh(siteId) },
              {
                id: "verify",
                label: "校验会话",
                disabled: !siteId || busyAction !== null,
                loading: busyAction === "verify-session",
                onClick: () => void triggerMaintenance("verify-session"),
              },
              {
                id: "checkin",
                label: "立即签到",
                tone: "primary",
                disabled: !siteId || busyAction !== null,
                loading: busyAction === "check-in",
                onClick: () => void triggerMaintenance("check-in"),
              },
            ]}
          />
        }
      />

      {sites.length === 0 && !loading && !error ? (
        <EmptyState
          title="尚未配置 PT 站点"
          description="请先在「设置 → 资源站点」中添加站点，再回到此页面执行签到。"
        />
      ) : (
        <CheckinBody
          sites={sites}
          siteId={siteId}
          receipts={receipts}
          availability={availability}
          busyAction={busyAction}
          error={error}
          onSelectSite={(nextId) => {
            siteIdRef.current = nextId;
            setSiteId(nextId);
            void refresh(nextId);
          }}
          onRunTask={() => void runScheduledCheckin()}
        />
      )}
    </div>
  );
}
