import { fetchEventSource } from "@microsoft/fetch-event-source";
import { getAuthToken, redirectToLoginOn401, resolveRequestUrl } from "@/lib/http";

const subscribers = new Set<() => void>();
let controller: AbortController | null = null;
let token: string | null = null;
let refreshTimer: ReturnType<typeof setTimeout> | undefined;

function stopStream() {
  controller?.abort();
  controller = null;
  clearTimeout(refreshTimer);
  refreshTimer = undefined;
}

function startStream(nextToken: string | null) {
  stopStream();
  token = nextToken;
  const active = new AbortController();
  controller = active;
  void fetchEventSource(resolveRequestUrl("/jobs/stream"), {
    signal: active.signal,
    headers: {
      Accept: "text/event-stream",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    openWhenHidden: true,
    async onopen(response) {
      if (response.ok && response.headers.get("content-type")?.includes("text/event-stream")) return;
      if (response.status === 401 || response.status === 403) {
        redirectToLoginOn401(response.status);
        active.abort();
      }
      throw new Error(`jobs/stream ${response.status}`);
    },
    onmessage(block) {
      if (active.signal.aborted || (block.event !== "ready" && block.event !== "job")) return;
      if (refreshTimer !== undefined) return;
      refreshTimer = setTimeout(() => {
        refreshTimer = undefined;
        for (const subscriber of subscribers) subscriber();
      }, 120);
    },
    onclose() {
      // A closed stream must reconnect even if the server ended it cleanly.
      throw new Error("jobs/stream disconnected");
    },
    onerror(error) {
      if (active.signal.aborted) throw error;
      // The transport retries transient network failures with its bounded backoff.
      return 3_000;
    },
  }).catch((error: unknown) => {
    if (!active.signal.aborted) console.warn("任务状态连接中断", error);
  });
}

/** All consumers in this tab share one Bearer-authenticated HTTP connection. */
export function subscribeToJobsStream(refresh: () => void): () => void {
  subscribers.add(refresh);
  const nextToken = getAuthToken();
  if (!controller || token !== nextToken) startStream(nextToken);
  return () => {
    subscribers.delete(refresh);
    if (subscribers.size === 0) stopStream();
  };
}
