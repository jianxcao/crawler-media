import type { ComponentType } from "react";
import type {
  BrowserAvailability,
  Capability,
  CatalogKind,
  CommandMap,
  Environment,
  HandleKind,
  InteractionResult,
  Json,
  Operation,
  OperationRef,
  OperationState,
  OptionView,
  Page,
  PageInput,
  PluginHostSDK,
  QueryMap,
  Receipt,
  ResolvedHandle,
  UiHandle,
  WriteOptions,
} from "@crawler-media/plugin-ui-sdk";
import { isTerminalOperation } from "@crawler-media/plugin-ui-sdk";

import type { SessionView } from "@/lib/api/auth";
import type { UiDescriptorView } from "@/lib/api/plugins";
import { publicEnv } from "@/lib/env";
import { getAuthToken } from "@/lib/http";
import {
  extractReceiptSiteId,
  makeSdkError,
  mapHttpToSdkError,
  normalizeQueryResult,
  normalizeReceiptState,
  resolveRoutePath,
} from "@/lib/plugins/sdk-helpers";

export interface PluginHostSdkOptions {
  descriptor: UiDescriptorView;
  surfaceId: string;
  session: SessionView;
  loginEpoch: string;
  theme: string;
  language?: string;
  layout?: string;
  navigate: (to: string) => void;
  notifyToast: (input: { message: string; level?: "info" | "warning" | "error" }) => void;
  openConfirm: (input: { title: string; message: string; confirmLabel: string }) => Promise<boolean>;
  openDialog: (component: ComponentType<{ close: () => void }>) => () => void;
  onRegisterActions?: (actions: Array<{ id: string; label: string; onSelect: () => void }>) => void;
  onRegisterTabs?: (tabs: readonly { id: string; label: string; onSelect: () => void }[]) => void;
  getPluginRevision?: () => string;
  fetchImpl?: typeof fetch;
}

export interface ManagedPluginHostSdk {
  sdk: PluginHostSDK;
  dispose: () => void;
  notifyEnvironmentChange: (patch: Partial<Environment>) => void;
}

interface SdkTransport {
  assertActive: () => void;
  apiJson: <T>(path: string, init?: RequestInit) => Promise<T>;
  apiEndpoint: <T>(path: string, init?: RequestInit) => Promise<T>;
  apiPaged: <T>(path: string, init?: RequestInit) => Promise<{ items: T[]; next_cursor: string | null }>;
  runQuery: <K extends keyof QueryMap>(name: K, input: QueryMap[K]["input"]) => Promise<QueryMap[K]["result"]>;
}

function createSdkTransport(
  options: PluginHostSdkOptions,
  pluginIdEnc: string,
  isDisposed: () => boolean,
): SdkTransport {
  const fetchFn = options.fetchImpl ?? fetch;

  function assertActive(): void {
    if (isDisposed()) {
      throw makeSdkError("host-disposed", "Plugin UI host surface has been disposed");
    }
  }

  function buildHeaders(extra?: Record<string, string>): Headers {
    const headers = new Headers(extra);
    headers.set("Accept", "application/json");
    headers.set("X-Plugin-Package", options.descriptor.package_hash);
    headers.set("X-Plugin-Generation", String(options.descriptor.generation));
    const token = getAuthToken();
    if (token && !headers.has("Authorization")) {
      headers.set("Authorization", `Bearer ${token}`);
    }
    return headers;
  }

  async function apiEnvelope<T>(
    path: string,
    init: RequestInit = {},
  ): Promise<{ data: T; next_cursor: string | null }> {
    assertActive();
    const headers = buildHeaders(init.headers as Record<string, string> | undefined);
    if (init.body && !headers.has("Content-Type")) {
      headers.set("Content-Type", "application/json");
    }
    let res: Response;
    try {
      res = await fetchFn(`${publicEnv.apiBaseUrl}${path}`, {
        ...init,
        credentials: "include",
        headers,
      });
    } catch (err) {
      assertActive();
      throw makeSdkError("request-failed", (err as Error).message || "Network error");
    }
    assertActive();
    const json = (await res.json().catch(() => null)) as
      | {
          ok?: boolean;
          data?: T;
          next_cursor?: string | null;
          error?: { code?: string; message?: string; fields?: unknown };
        }
      | null;
    assertActive();
    if (!res.ok || !json || json.ok !== true) {
      throw mapHttpToSdkError(
        res.status,
        json?.error?.code,
        json?.error?.message ?? `Request failed (${res.status})`,
        json?.error?.fields,
      );
    }
    return { data: json.data as T, next_cursor: json.next_cursor ?? null };
  }

  async function apiJson<T>(path: string, init: RequestInit = {}): Promise<T> {
    const env = await apiEnvelope<T>(path, init);
    return env.data;
  }

  async function apiPaged<T>(
    path: string,
    init: RequestInit = {},
  ): Promise<{ items: T[]; next_cursor: string | null }> {
    const env = await apiEnvelope<T[] | { items?: T[]; next_cursor?: string | null }>(path, init);
    if (Array.isArray(env.data)) {
      return { items: env.data, next_cursor: env.next_cursor };
    }
    if (env.data && typeof env.data === "object" && Array.isArray(env.data.items)) {
      return { items: env.data.items, next_cursor: env.data.next_cursor ?? env.next_cursor };
    }
    return { items: [], next_cursor: env.next_cursor };
  }

  async function apiEndpoint<T>(path: string, init: RequestInit = {}): Promise<T> {
    assertActive();
    const headers = buildHeaders(init.headers as Record<string, string> | undefined);
    headers.set("Accept", "application/json, text/plain;q=0.9");
    if (init.body && !headers.has("Content-Type")) {
      headers.set("Content-Type", "application/json");
    }
    let res: Response;
    try {
      res = await fetchFn(`${publicEnv.apiBaseUrl}${path}`, {
        ...init,
        credentials: "include",
        headers,
      });
    } catch (err) {
      assertActive();
      throw makeSdkError("request-failed", (err as Error).message || "Network error");
    }
    assertActive();
    const rawText = await res.text().catch(() => "");
    assertActive();
    let parsedJson: unknown = undefined;
    const ctEssence = (res.headers.get("content-type") ?? "")
      .toLowerCase()
      .split(";")[0]
      ?.trim() ?? "";
    if (
      rawText.length > 0 &&
      (ctEssence === "application/json" ||
        (!ctEssence && (rawText.startsWith("{") || rawText.startsWith("["))))
    ) {
      try {
        parsedJson = JSON.parse(rawText);
      } catch {
        if (res.ok && ctEssence === "application/json") {
          throw makeSdkError(
            "request-failed",
            "Invalid JSON response from plugin endpoint",
            res.status,
          );
        }
        parsedJson = undefined;
      }
    }
    const envObj =
      parsedJson && typeof parsedJson === "object" && !Array.isArray(parsedJson)
        ? (parsedJson as {
            ok?: boolean;
            data?: T;
            error?: { code?: string; message?: string; fields?: unknown };
          })
        : null;
    const isHostErrorEnvelope =
      envObj?.ok === false && envObj.error != null && typeof envObj.error === "object";
    if (!res.ok || isHostErrorEnvelope) {
      throw mapHttpToSdkError(
        res.status,
        envObj?.error?.code,
        envObj?.error?.message ?? (rawText.trim() || `Request failed (${res.status})`),
        envObj?.error?.fields,
      );
    }
    if (envObj && envObj.ok === true && "data" in envObj) {
      return envObj.data as T;
    }
    if (parsedJson !== undefined) {
      return parsedJson as T;
    }
    return (rawText.length > 0 ? rawText : null) as T;
  }

  async function runQuery<K extends keyof QueryMap>(
    name: K,
    input: QueryMap[K]["input"],
  ): Promise<QueryMap[K]["result"]> {
    const data = await apiJson<unknown>(`/plugins/${pluginIdEnc}/ui/query`, {
      method: "POST",
      body: JSON.stringify({ name, input }),
    });
    return normalizeQueryResult(name, data);
  }

  return { assertActive, apiJson, apiEndpoint, apiPaged, runQuery };
}

function buildTasksSubSdk(
  pluginIdEnc: string,
  transport: SdkTransport,
  getOperation: PluginHostSDK["operations"]["get"],
): PluginHostSDK["tasks"] {
  return {
    async list(input: PageInput) {
      const qs = new URLSearchParams({ limit: String(input.limit ?? 50) });
      if (input.cursor) qs.set("cursor", input.cursor);
      const page = await transport.apiPaged<Record<string, unknown>>(
        `/plugins/${pluginIdEnc}/tasks?${qs.toString()}`,
      );
      return {
        items: page.items.map((t) => ({
          task_id: String(t.task_id ?? t.id ?? ""),
          title: String(t.name ?? t.task_id ?? t.id ?? ""),
          enabled: Boolean(t.enabled),
          definition_revision: String(t.definition_revision ?? 1),
        })),
        next_cursor: page.next_cursor,
      };
    },
    async run(taskId: string, input: Json, writeOpts: WriteOptions): Promise<OperationRef> {
      let realDefRev = String(writeOpts.expectedRevision ?? "").trim();
      if (!realDefRev || realDefRev === "0") {
        const page = await transport.apiPaged<Record<string, unknown>>(
          `/plugins/${pluginIdEnc}/tasks?limit=100`,
        );
        const found = page.items.find((t) => String(t.task_id ?? t.id ?? "") === taskId);
        if (found) {
          const curRev = Number(found.definition_revision ?? 1);
          if (!found.enabled) {
            const sched =
              found.schedule && typeof found.schedule === "object"
                ? (found.schedule as Record<string, unknown>)
                : undefined;
            const rawSecs = Number(sched?.interval_secs ?? found.interval_secs ?? 3600);
            const intervalSecs =
              Number.isFinite(rawSecs) && rawSecs >= 60 && rawSecs <= 86400
                ? Math.trunc(rawSecs)
                : 3600;
            const cfg = (found.config ?? found.public_config ?? {}) as Record<string, unknown>;
            const enOp = await transport.apiJson<{
              operation_id: string;
              state: OperationState;
              error?: string | null;
              blocked_reason?: string | null;
              phase?: string | null;
            }>(`/plugins/${pluginIdEnc}/tasks/${encodeURIComponent(taskId)}/update`, {
              method: "POST",
              headers: {
                "If-Match": `"${curRev}"`,
                "Idempotency-Key": `${writeOpts.idempotencyKey}:enable`,
              },
              body: JSON.stringify({
                enabled: true,
                schedule: { interval_secs: intervalSecs },
                config: cfg,
              }),
            });
            let finalState = enOp.state;
            let finalErr = enOp.error ?? enOp.blocked_reason ?? enOp.phase ?? null;
            if (!isTerminalOperation(finalState)) {
              let polled = await getOperation(enOp.operation_id);
              for (let i = 0; i < 30 && !isTerminalOperation(polled.state); i += 1) {
                await new Promise((r) => setTimeout(r, 200));
                polled = await getOperation(enOp.operation_id);
              }
              finalState = polled.state;
              finalErr = polled.error ?? polled.phase ?? finalErr;
            }
            if (finalState !== "succeeded") {
              throw makeSdkError(
                "request-failed",
                finalErr || `Auto-enabling task "${taskId}" failed (${finalState})`,
              );
            }
            const refreshed = await transport
              .apiPaged<Record<string, unknown>>(`/plugins/${pluginIdEnc}/tasks?limit=100`)
              .catch(() => ({ items: [] as Array<Record<string, unknown>>, next_cursor: null }));
            const nextTask = refreshed.items.find((t) => String(t.task_id ?? t.id ?? "") === taskId);
            const nextRevNum = Number(nextTask?.definition_revision ?? 0);
            realDefRev = String(nextRevNum > curRev ? nextRevNum : curRev + 1);
          } else {
            realDefRev = String(curRev);
          }
        } else {
          realDefRev = "0";
        }
      }
      const op = await transport.apiJson<{ operation_id: string; state: OperationState }>(
        `/plugins/${pluginIdEnc}/tasks/${encodeURIComponent(taskId)}/run`,
        {
          method: "POST",
          headers: {
            "If-Match": `"${realDefRev}"`,
            "Idempotency-Key": writeOpts.idempotencyKey,
          },
          body: JSON.stringify({ input: input ?? {} }),
        },
      );
      return { operation_id: op.operation_id, state: op.state };
    },
    async receipts(taskId: string, input: PageInput): Promise<Page<Receipt>> {
      const qs = new URLSearchParams({ limit: String(input.limit ?? 20) });
      if (input.cursor) qs.set("cursor", input.cursor);
      const page = await transport.apiPaged<Record<string, unknown>>(
        `/plugins/${pluginIdEnc}/tasks/${encodeURIComponent(taskId)}/receipts?${qs.toString()}`,
      );
      return {
        items: page.items.map((r) => ({
          site_id: extractReceiptSiteId(r),
          state: normalizeReceiptState(r.outcome, r.state),
          observed_at: String(r.observed_at ?? r.updated_at ?? ""),
          operation_id: typeof r.operation_id === "string" ? r.operation_id : null,
          detail: typeof r.evidence === "string" ? r.evidence : null,
        })),
        next_cursor: page.next_cursor,
      };
    },
  };
}

function buildDownloadsSubSdk(
  pluginIdEnc: string,
  options: PluginHostSdkOptions,
  transport: SdkTransport,
  handleRevisions: Map<string, string>,
  submitCommand: PluginHostSDK["commands"]["submit"],
  getOperation: PluginHostSDK["operations"]["get"],
): PluginHostSDK["downloads"] {
  return {
    async add(): Promise<InteractionResult> {
      return { status: "cancelled" };
    },
    async cleanup(input): Promise<InteractionResult> {
      transport.assertActive();
      if (input.owned_task_handles.length === 0) {
        return { status: "cancelled" };
      }
      if (input.owned_task_handles.length > 1) {
        throw makeSdkError("request-failed", "cleanup supports one owned_task_handle at a time");
      }
      const keyBase = `cm-${Date.now()}-${Math.random().toString(16).slice(2)}`;
      const firstHandle = input.owned_task_handles[0];
      const targetRevision =
        handleRevisions.get(firstHandle) ?? options.getPluginRevision?.() ?? "1";
      if (input.mode === "task-only") {
        const ok = await options.openConfirm({
          title: "仅删除下载任务",
          message: "将从下载器移除所选任务记录，本地文件暂保留（计入待清理配额）。",
          confirmLabel: "删除任务",
        });
        if (!ok) return { status: "cancelled" };
        const op = await submitCommand(
          "downloader.delete-task",
          {
            owned_task_handle: firstHandle,
            revision: targetRevision,
            reason: "boost-ui-delete-task",
          },
          { expectedRevision: targetRevision, idempotencyKey: keyBase },
        );
        return { status: "submitted", operation: op };
      }

      const prepOp = await submitCommand(
        "downloader.prepare-cleanup",
        {
          owned_task_handles: input.owned_task_handles,
          expected_revision: targetRevision,
          reason: "boost-ui-prepare-cleanup",
        },
        { expectedRevision: targetRevision, idempotencyKey: `${keyBase}-prep` },
      );
      let finalOp = await getOperation(prepOp.operation_id);
      for (let i = 0; i < 30 && !isTerminalOperation(finalOp.state); i += 1) {
        await new Promise((r) => setTimeout(r, 200));
        finalOp = await getOperation(prepOp.operation_id);
      }
      const resObj = (finalOp.result ?? {}) as { plan_handle?: string };
      if (finalOp.state !== "succeeded" || !resObj.plan_handle) {
        throw makeSdkError("request-failed", finalOp.error ?? finalOp.phase ?? "准备清理计划失败");
      }
      const preview = await transport.apiJson<{
        plan_handle?: string | null;
        revision?: number | null;
        confirmation_token?: string | null;
        total_retained_bytes?: number;
        tasks?: Array<{ paths?: string[] }>;
      }>(
        `/plugins/${pluginIdEnc}/brush-cleanup-preview?plan_handle=${encodeURIComponent(resObj.plan_handle)}`,
      );
      const paths = (preview.tasks ?? []).flatMap((t) => t.paths ?? []);
      const ok = await options.openConfirm({
        title: "确认彻底删除刷流文件",
        message: `将永久删除 ${paths.length} 个路径（释放 ${preview.total_retained_bytes ?? 0} B）：${paths.slice(0, 3).join(", ")}`,
        confirmLabel: "确认删除文件",
      });
      if (!ok || !preview.confirmation_token || !preview.plan_handle) {
        return { status: "cancelled" };
      }
      const delOp = await submitCommand(
        "downloader.delete-files",
        {
          host_file_plan_handle: preview.plan_handle as UiHandle,
          revision: String(preview.revision ?? 0),
          reason: "host-cleanup-confirmed",
          confirmation_token: preview.confirmation_token,
        },
        { expectedRevision: String(preview.revision ?? 0), idempotencyKey: `${keyBase}-del` },
      );
      return { status: "submitted", operation: delOp };
    },
  };
}

function buildOperationsSubSdk(
  transport: SdkTransport,
  activeTimers: Set<ReturnType<typeof setInterval>>,
  isDisposed: () => boolean,
): PluginHostSDK["operations"] {
  const get: PluginHostSDK["operations"]["get"] = async (id: string): Promise<Operation> => {
    const raw = await transport.apiJson<Record<string, unknown>>(
      `/plugin-operations/${encodeURIComponent(id)}`,
    );
    return {
      operation_id: String(raw.operation_id ?? id),
      state: (raw.state as OperationState) ?? "unknown",
      revision: String(raw.revision ?? "0"),
      phase: typeof raw.phase === "string" ? raw.phase : null,
      job_id: typeof raw.job_id === "string" ? raw.job_id : null,
      progress: null,
      error: typeof raw.error === "string" ? raw.error : null,
      result: raw.result,
      next_run_at: typeof raw.next_run_at === "string" ? raw.next_run_at : null,
    };
  };

  return {
    get,
    watch(id: string, listener: (operation: Operation) => void) {
      let stopped = false;
      let inflight = false;
      const tick = async () => {
        if (stopped || inflight || isDisposed()) return;
        inflight = true;
        try {
          const op = await get(id);
          if (stopped || isDisposed()) return;
          listener(op);
          if (isTerminalOperation(op.state)) {
            stopped = true;
            clearInterval(timer);
            activeTimers.delete(timer);
          }
        } catch {
          /* transient poll errors stop only when disposed or unsubscribed */
        } finally {
          inflight = false;
        }
      };
      const timer = setInterval(() => void tick(), 500);
      activeTimers.add(timer);
      void tick();
      return () => {
        stopped = true;
        clearInterval(timer);
        activeTimers.delete(timer);
      };
    },
  };
}

function buildBrowserAndShellHelpers(
  pluginIdEnc: string,
  options: PluginHostSdkOptions,
  transport: SdkTransport,
  actionMap: Map<string, { id: string; label: string; onSelect: () => void }>,
  isDisposed: () => boolean,
): Pick<
  PluginHostSDK,
  "endpoint" | "media" | "subscriptions" | "libraries" | "files" | "browser" | "ui" | "navigation" | "shell"
> {
  const emitActions = () => options.onRegisterActions?.(Array.from(actionMap.values()));
  return {
    endpoint: {
      async request(endpointId: string, input) {
        transport.assertActive();
        const trimmed = endpointId.replace(/^\/+/, "");
        const rawSegments = trimmed.split("/");
        const hasControlChar = Array.from(endpointId).some((ch) => {
          const code = ch.charCodeAt(0);
          return code <= 0x1f || code === 0x7f;
        });
        if (
          !trimmed ||
          endpointId.includes("\\") ||
          hasControlChar ||
          rawSegments.some((seg) => seg.length === 0 || seg === "." || seg === "..")
        ) {
          throw makeSdkError("request-failed", "Invalid plugin endpoint path");
        }
        const cleanPath = rawSegments.map(encodeURIComponent).join("/");
        const qs = input.query ? `?${new URLSearchParams(input.query).toString()}` : "";
        return transport.apiEndpoint<Json | OperationRef>(
          `/plugins/${pluginIdEnc}/endpoints/${cleanPath}${qs}`,
          {
            method: input.method ?? (input.body !== undefined ? "POST" : "GET"),
            body: input.body !== undefined ? JSON.stringify(input.body) : undefined,
            signal: input.signal,
          },
        );
      },
    },
    media: {
      select: async () => null,
      async detail(req) {
        transport.assertActive();
        const m = (await transport
          .runQuery("media.get", { id: req.media_id, media_id: req.media_id })
          .catch(() => null)) as Record<string, unknown> | null;
        transport.assertActive();
        options.navigate(
          resolveRoutePath(pluginIdEnc, "media", {
            media_id: req.media_id,
            library_id: typeof m?.library_id === "string" && m.library_id ? m.library_id : undefined,
            kind: typeof m?.kind === "string" && m.kind ? m.kind : undefined,
            tmdb_id: m?.tmdb_id ? String(m.tmdb_id) : undefined,
            douban_id: typeof m?.douban_id === "string" && m.douban_id ? m.douban_id : undefined,
          }),
        );
        return { status: "opened" };
      },
    },
    subscriptions: {
      create: async () => ({ status: "cancelled" }),
      async edit(input) {
        transport.assertActive();
        options.navigate(`/subscriptions/${encodeURIComponent(input.subscribe_id)}`);
        return { status: "opened" };
      },
    },
    libraries: {
      async open(input) {
        transport.assertActive();
        options.navigate(`/library/${encodeURIComponent(input.library_id)}`);
        return { status: "opened" };
      },
    },
    files: {
      select: async () => null,
      transfer: async () => ({ status: "cancelled" }),
      scrape: async () => ({ status: "cancelled" }),
    },
    browser: {
      async availability(input): Promise<BrowserAvailability> {
        const qs = new URLSearchParams({ site_id: input.site_id, purpose: input.purpose });
        const raw = await transport.apiJson<Record<string, unknown>>(
          `/plugins/${pluginIdEnc}/browser-availability?${qs.toString()}`,
        );
        const missing = Array.isArray(raw.missing_requirements)
          ? raw.missing_requirements.map(String)
          : Array.isArray(raw.reasons)
            ? raw.reasons.map(String)
            : [];
        return {
          site_id: String(raw.site_id ?? input.site_id),
          purpose: input.purpose,
          status: String(raw.status ?? (raw.available === true ? "ready" : "unavailable")),
          missing_requirements: missing,
          retry_after_ms: typeof raw.retry_after_ms === "number" ? raw.retry_after_ms : null,
        };
      },
    },
    ui: {
      toast: (input) => {
        if (!isDisposed()) options.notifyToast(input);
      },
      confirm: (input) => (transport.assertActive(), options.openConfirm(input)),
      dialog: (component) => (transport.assertActive(), options.openDialog(component)),
    },
    navigation: {
      async open(route, params) {
        transport.assertActive();
        options.navigate(resolveRoutePath(pluginIdEnc, route, params));
        return { status: "opened" };
      },
    },
    shell: {
      registerAction(action) {
        actionMap.set(action.id, action);
        emitActions();
        return () => {
          actionMap.delete(action.id);
          emitActions();
        };
      },
      registerTabs(tabs) {
        options.onRegisterTabs?.(tabs);
        return () => options.onRegisterTabs?.([]);
      },
    },
  };
}

function buildCoreQueryHelpers(
  options: PluginHostSdkOptions,
  transport: SdkTransport,
  handleRevisions: Map<string, string>,
): Pick<PluginHostSDK, "catalogs" | "query" | "objects" | "search"> {
  return {
    catalogs: {
      list(kind: CatalogKind, input: PageInput): Promise<Page<OptionView>> {
        if (options.descriptor.lifecycle.toLowerCase() !== "enabled") {
          return transport.runQuery("config.options", { kind, ...input });
        }
        return transport.runQuery(`${kind}.list` as keyof QueryMap, input) as Promise<Page<OptionView>>;
      },
    },
    query: transport.runQuery,
    objects: {
      async resolve(kind: HandleKind, id: string, revision?: string): Promise<ResolvedHandle> {
        const input: Record<string, unknown> = { kind, id };
        const revStr = revision !== undefined && revision !== null ? String(revision).trim() : "";
        if (revStr && /^\d+$/.test(revStr)) {
          input.expected_revision = Number(revStr);
        }
        const resolved = await transport.runQuery(
          "handles.resolve",
          input as QueryMap["handles.resolve"]["input"],
        );
        if (resolved?.handle && resolved?.revision !== undefined) {
          handleRevisions.set(resolved.handle, String(resolved.revision));
        }
        return resolved;
      },
    },
    search: {
      results: (operationId: string, input: PageInput) =>
        transport.runQuery("indexer.cached", { ...input, search_operation_id: operationId }),
      async open(input) {
        transport.assertActive();
        const qs = new URLSearchParams();
        if (input.query) qs.set("q", input.query);
        options.navigate(qs.toString() ? `/search?${qs.toString()}` : "/search");
        return { status: "opened" };
      },
    },
  };
}

export function createPluginHostSdk(options: PluginHostSdkOptions): ManagedPluginHostSdk {
  let disposed = false;
  const activeTimers = new Set<ReturnType<typeof setInterval>>();
  const envListeners = new Set<() => void>();
  const actionMap = new Map<string, { id: string; label: string; onSelect: () => void }>();
  const handleRevisions = new Map<string, string>();
  const pluginIdEnc = encodeURIComponent(options.descriptor.plugin_id);
  const capSet = new Set(options.descriptor.capabilities);
  const transport = createSdkTransport(options, pluginIdEnc, () => disposed);
  const operations = buildOperationsSubSdk(transport, activeTimers, () => disposed);
  let currentEnv: Environment = {
    theme: options.theme,
    language: options.language ?? "zh-CN",
    layout: options.layout ?? "desktop",
    user: { id: options.session.id, name: options.session.nickname || options.session.username },
  };

  const submitCommand: PluginHostSDK["commands"]["submit"] = async <K extends keyof CommandMap>(
    kind: K,
    payload: CommandMap[K],
    writeOpts: WriteOptions,
  ): Promise<OperationRef> => {
    const extra: Record<string, string> = { "Idempotency-Key": writeOpts.idempotencyKey };
    const pluginRevision = String(options.getPluginRevision?.() ?? "").trim();
    if (pluginRevision && /^\d+$/.test(pluginRevision)) {
      extra["If-Match"] = `"${pluginRevision}"`;
    }
    const op = await transport.apiJson<{ operation_id: string; state: OperationState }>(
      `/plugins/${pluginIdEnc}/commands`,
      { method: "POST", headers: extra, body: JSON.stringify({ kind, payload }) },
    );
    return { operation_id: op.operation_id, state: op.state };
  };

  const sdk: PluginHostSDK = {
    context: Object.freeze({
      plugin_id: options.descriptor.plugin_id,
      package_hash: options.descriptor.package_hash,
      generation: String(options.descriptor.generation),
      surface_id: options.surfaceId,
      login_epoch: options.loginEpoch,
    }),
    supports: (name: Capability) =>
      capSet.has(name) ||
      ((name === "command:site.check-in" || name === "command:site.verify-session") &&
        capSet.has("command:site.maintenance")),
    environment: {
      get: () => currentEnv,
      subscribe(listener: () => void) {
        envListeners.add(listener);
        return () => void envListeners.delete(listener);
      },
    },
    ...buildCoreQueryHelpers(options, transport, handleRevisions),
    commands: { submit: submitCommand },
    tasks: buildTasksSubSdk(pluginIdEnc, transport, operations.get),
    operations,
    downloads: buildDownloadsSubSdk(
      pluginIdEnc,
      options,
      transport,
      handleRevisions,
      submitCommand,
      operations.get,
    ),
    ...buildBrowserAndShellHelpers(pluginIdEnc, options, transport, actionMap, () => disposed),
  };

  return {
    sdk,
    dispose() {
      disposed = true;
      for (const t of activeTimers) clearInterval(t);
      activeTimers.clear();
      envListeners.clear();
      actionMap.clear();
    },
    notifyEnvironmentChange(patch: Partial<Environment>) {
      if (disposed) return;
      currentEnv = { ...currentEnv, ...patch };
      for (const listener of envListeners) listener();
    },
  };
}
