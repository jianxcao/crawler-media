import "./style.css";

import { useCallback, useEffect, useRef, useState } from "react";
import type { OptionView, PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { isTerminalOperation } from "@crawler-media/plugin-ui-sdk";
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
  type BoostFullSnapshot,
  type BoostTaskDto,
  formatBytes,
  protectionTone,
} from "./shared";

function isDiskOk(status: string): boolean {
  return status === "available" || status === "ok";
}

function BoostTasksSection({
  tasks,
  reservations,
  loading,
  busy,
  onCleanup,
}: {
  tasks: BoostTaskDto[];
  reservations: BoostFullSnapshot["reservations"];
  loading: boolean;
  busy: string | null;
  onCleanup: (task: BoostTaskDto, mode: "task-only" | "files") => void;
}) {
  return (
    <>
      <Card title={`托管刷流任务 (${tasks.length})`}>
        {tasks.length === 0 && !loading ? (
          <EmptyState
            title="暂无托管刷流任务"
            description="配置好站点与下载器预算后，点击「立即巡检」或等待调度周期开始抓取。"
          />
        ) : (
          <DataTable
            rows={tasks}
            getRowKey={(row) => row.task_id}
            columns={[
              {
                key: "info_hash",
                header: "种子 / Task",
                render: (row) => (
                  <span>
                    {row.info_hash.slice(0, 12)} ({row.state})
                  </span>
                ),
              },
              {
                key: "bytes",
                header: "大小 / 上传 / 保留",
                render: (row) => (
                  <span>
                    {formatBytes(row.task_bytes)} / ↑ {formatBytes(row.uploaded_bytes)} / 保留{" "}
                    {formatBytes(row.retained_bytes)}
                  </span>
                ),
              },
              {
                key: "protection",
                header: "保护状态",
                render: (row) => (
                  <StatusPill
                    label={row.protection_state}
                    tone={protectionTone(row.protection_state)}
                  />
                ),
              },
              {
                key: "actions",
                header: "操作",
                align: "right",
                render: (row) => (
                  <div className="cm-boost-row" style={{ justifyContent: "flex-end" }}>
                    <button
                      type="button"
                      className="cm-plugin-btn"
                      disabled={busy !== null}
                      onClick={() => onCleanup(row, "task-only")}
                    >
                      仅删任务
                    </button>
                    <button
                      type="button"
                      className="cm-plugin-btn"
                      data-tone="danger"
                      disabled={busy !== null}
                      onClick={() => onCleanup(row, "files")}
                    >
                      清理文件
                    </button>
                  </div>
                ),
              },
            ]}
          />
        )}
      </Card>

      {reservations.length > 0 && (
        <Card title={`活跃空间预留 (${reservations.length})`}>
          <DataTable
            rows={reservations}
            getRowKey={(r) => r.operation_id}
            columns={[
              { key: "op", header: "操作 ID", render: (r) => r.operation_id },
              { key: "stage", header: "阶段", render: (r) => r.stage },
              {
                key: "bytes",
                header: "预留大小",
                render: (r) => formatBytes(r.reserved_bytes),
              },
            ]}
          />
        </Card>
      )}
    </>
  );
}

function BoostFiltersAndStats({
  snapshot,
  sites,
  downloaders,
  siteFilter,
  downloaderFilter,
  onSiteFilter,
  onDownloaderFilter,
}: {
  snapshot: BoostFullSnapshot | null;
  sites: OptionView[];
  downloaders: OptionView[];
  siteFilter: string;
  downloaderFilter: string;
  onSiteFilter: (v: string) => void;
  onDownloaderFilter: (v: string) => void;
}) {
  return (
    <>
      <Card title="筛选范围">
        <div className="cm-boost-row">
          <select
            aria-label="按站点筛选"
            value={siteFilter}
            onChange={(e) => onSiteFilter(e.target.value)}
            className="cm-boost-select"
            style={{ width: "auto", minWidth: 180 }}
          >
            <option value="">全部站点</option>
            {sites.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </select>
          <select
            aria-label="按下载器筛选"
            value={downloaderFilter}
            onChange={(e) => onDownloaderFilter(e.target.value)}
            className="cm-boost-select"
            style={{ width: "auto", minWidth: 180 }}
          >
            <option value="">全部下载器</option>
            {downloaders.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </select>
        </div>
      </Card>

      {snapshot && (
        <div className="cm-boost-stats">
          <StatBadge label="已占用配额" value={formatBytes(snapshot.used_bytes)} />
          <StatBadge label="预留中空间" value={formatBytes(snapshot.reserved_bytes)} />
          <StatBadge
            label="待删保留文件"
            value={formatBytes(snapshot.retained_file_bytes)}
            tone={(snapshot.retained_file_bytes ?? 0) > 0 ? "warn" : "neutral"}
          />
          <StatBadge
            label="磁盘可用"
            value={formatBytes(snapshot.disk.available_bytes)}
            hint={`rev #${snapshot.revision}`}
          />
        </div>
      )}
    </>
  );
}

function useBoostOperations(
  host: PluginSurfaceProps["host"],
  refresh: () => Promise<void>,
  setBusy: (v: string | null) => void,
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

  async function runCycleNow() {
    setBusy("cycle");
    try {
      const op = await host.tasks.run(
        "traffic-cycle",
        {},
        { expectedRevision: "0", idempotencyKey: `boost-cycle-${Date.now()}` },
      );
      host.ui.toast({ message: `已调度刷流巡检 (${op.operation_id})`, level: "info" });
      unwatchRef.current?.();
      unwatchRef.current = host.operations.watch(op.operation_id, (snap) => {
        if (isTerminalOperation(snap.state)) {
          unwatchRef.current?.();
          unwatchRef.current = null;
          setBusy(null);
          void refreshRef.current();
        }
      });
    } catch (err) {
      setBusy(null);
      const msg = (err as Error).message || "触发巡检失败";
      setError(msg);
      host.ui.toast({ message: msg, level: "error" });
    }
  }

  async function cleanupTask(task: BoostTaskDto, mode: "task-only" | "files") {
    setBusy(`${mode}:${task.task_id}`);
    try {
      const resolved = await host.objects.resolve("owned_download", task.task_id, String(task.revision));
      const res = await host.downloads.cleanup({ owned_task_handles: [resolved.handle], mode });
      if (res.status === "submitted") {
        host.ui.toast({
          message:
            mode === "files"
              ? `已提交文件彻底清理 (${res.operation.operation_id})`
              : `已提交删除任务 (${res.operation.operation_id})`,
          level: "info",
        });
        await refresh();
      }
    } catch (err) {
      const msg = (err as Error).message || "清理操作失败";
      setError(msg);
      host.ui.toast({ message: msg, level: "error" });
    } finally {
      setBusy(null);
    }
  }

  return { runCycleNow, cleanupTask };
}

export default function BoostPage({ host }: PluginSurfaceProps) {
  const [snapshot, setSnapshot] = useState<BoostFullSnapshot | null>(null);
  const [sites, setSites] = useState<OptionView[]>([]);
  const [downloaders, setDownloaders] = useState<OptionView[]>([]);
  const [siteFilter, setSiteFilter] = useState("");
  const [downloaderFilter, setDownloaderFilter] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reqSeqRef = useRef(0);

  const refresh = useCallback(async () => {
    const seq = ++reqSeqRef.current;
    setLoading(true);
    setError(null);
    try {
      const input: { site_id?: string; downloader_id?: string } = {};
      if (siteFilter) input.site_id = siteFilter;
      if (downloaderFilter) input.downloader_id = downloaderFilter;
      const [snapRes, siteRes, dlRes] = await Promise.all([
        host.query("boost.snapshot", input),
        host.catalogs.list("sites", { limit: 100 }).catch(() => ({ items: [] })),
        host.catalogs.list("downloaders", { limit: 100 }).catch(() => ({ items: [] })),
      ]);
      if (seq !== reqSeqRef.current) return;
      setSnapshot(snapRes as unknown as BoostFullSnapshot);
      setSites(siteRes.items);
      setDownloaders(dlRes.items);
    } catch (err) {
      if (seq !== reqSeqRef.current) return;
      setError((err as Error).message || "读取刷流快照失败");
    } finally {
      if (seq === reqSeqRef.current) {
        setLoading(false);
      }
    }
  }, [host, siteFilter, downloaderFilter]);

  useEffect(() => {
    void refresh();
    return () => {
      reqSeqRef.current += 1;
    };
  }, [refresh]);

  const { runCycleNow, cleanupTask } = useBoostOperations(host, refresh, setBusy, setError);

  return (
    <div className="cm-boost-shell">
      <PageHeader
        title="自动刷流控制台"
        subtitle="实时配额账本、预留字节、H&R 保护状态与安全清理"
        badge={
          snapshot ? (
            <StatusPill
              label={`磁盘: ${snapshot.disk.status}`}
              tone={isDiskOk(snapshot.disk.status) ? "ok" : "warn"}
            />
          ) : undefined
        }
        actions={
          <ActionBar
            items={[
              { id: "refresh", label: "刷新快照", loading, onClick: () => void refresh() },
              {
                id: "run-cycle",
                label: "立即巡检",
                tone: "primary",
                loading: busy === "cycle",
                disabled: busy !== null,
                onClick: () => void runCycleNow(),
              },
            ]}
          />
        }
      />

      <BoostFiltersAndStats
        snapshot={snapshot}
        sites={sites}
        downloaders={downloaders}
        siteFilter={siteFilter}
        downloaderFilter={downloaderFilter}
        onSiteFilter={setSiteFilter}
        onDownloaderFilter={setDownloaderFilter}
      />

      {error && (
        <Card title="操作提示">
          <p role="alert">{error}</p>
        </Card>
      )}

      <BoostTasksSection
        tasks={snapshot?.tasks ?? []}
        reservations={snapshot?.reservations ?? []}
        loading={loading}
        busy={busy}
        onCleanup={(row, mode) => void cleanupTask(row, mode)}
      />
    </div>
  );
}
