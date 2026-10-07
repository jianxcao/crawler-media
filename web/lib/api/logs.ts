import { request, resolveRequestUrl, getAuthToken } from "@/lib/http";
import { fetchEventSource } from "@microsoft/fetch-event-source";

export interface SystemLogEntry {
  id: number;
  timestamp: number;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE" | string;
  target: string;
  message: string;
}

export interface ListLogsQuery {
  level?: string;
  module?: string;
  q?: string;
  limit?: number;
}

export interface ListLogsResult {
  total: number;
  entries: SystemLogEntry[];
}

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

export async function listSystemLogs(query: ListLogsQuery = {}): Promise<ListLogsResult> {
  const params = new URLSearchParams();
  if (query.level && query.level !== "ALL") params.set("level", query.level);
  if (query.module && query.module !== "ALL") params.set("module", query.module);
  if (query.q) params.set("q", query.q);
  if (query.limit) params.set("limit", String(query.limit));

  const qs = params.toString();
  return unwrap(request<ApiEnvelope<ListLogsResult>>(`/system/logs${qs ? `?${qs}` : ""}`));
}

export async function clearSystemLogs(): Promise<{ cleared: boolean }> {
  return unwrap(request<ApiEnvelope<{ cleared: boolean }>>("/system/logs/clear", { method: "POST" }));
}

export function subscribeSystemLogs(options: {
  onLog: (entry: SystemLogEntry) => void;
  onError?: (err: unknown) => void;
  signal?: AbortSignal;
}): () => void {
  const ctrl = new AbortController();
  const signal = options.signal || ctrl.signal;
  const token = getAuthToken();

  const headers: Record<string, string> = {};
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }

  void fetchEventSource(resolveRequestUrl("/system/logs/stream"), {
    method: "GET",
    headers,
    signal,
    onmessage(msg) {
      if (msg.event === "log" && msg.data) {
        try {
          const entry: SystemLogEntry = JSON.parse(msg.data);
          options.onLog(entry);
        } catch {
          // ignore parse error
        }
      }
    },
    onerror(err) {
      options.onError?.(err);
    },
  });

  return () => {
    ctrl.abort();
  };
}

export function exportLogsUrl(): string {
  return resolveRequestUrl("/system/logs/export");
}
