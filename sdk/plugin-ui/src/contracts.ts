import type { ComponentType } from "react";

export const PLUGIN_UI_CONTRACT_VERSION = "1.0.0" as const;
export const PLUGIN_UI_REACT_VERSION = "19.3.0" as const;
export const PLUGIN_UI_REACT_DOM_VERSION = "19.3.0" as const;

export type Revision = string;

declare const uiHandleBrand: unique symbol;

/** Opaque server-issued capability handle preventing accidental ID substitution. */
export type UiHandle = string & { readonly [uiHandleBrand]: true };

export type Dispose = () => void;

export type OperationState =
  | "queued"
  | "running"
  | "waiting"
  | "blocked"
  | "succeeded"
  | "rejected"
  | "deferred"
  | "failed"
  | "cancelled"
  | "unknown";

export type OperationRef = {
  operation_id: string;
  state: OperationState;
};

export type Operation = OperationRef & {
  revision: Revision;
  phase?: string | null;
  job_id?: string | null;
  progress?: { finished: number; total: number | null } | null;
  error?: string | null;
  result?: unknown;
  next_run_at?: string | null;
};

export type WriteOptions = {
  expectedRevision: Revision;
  idempotencyKey: string;
};

export type InteractionResult =
  | { status: "opened" }
  | { status: "submitted"; operation: OperationRef }
  | { status: "cancelled" };

export type Opened = { status: "opened" };

export type PageInput = {
  cursor?: string;
  limit?: number;
};

export type Page<T> = {
  items: T[];
  next_cursor?: string | null;
};

export type CatalogKind = "sites" | "downloaders" | "libraries" | "filters";

export type OptionView = {
  id: string;
  name: string;
  capabilities: readonly string[];
};

export type HandleKind =
  | "site"
  | "downloader"
  | "library"
  | "media"
  | "subscribe"
  | "owned_download";

export type ResolvedHandle = {
  handle: UiHandle;
  revision: Revision;
  expires_at: string;
};

export type TorrentView = {
  handle: UiHandle;
  site_id: string;
  title: string;
  size: number | null;
  seeders?: number | null;
  leechers?: number | null;
  published_at?: string | null;
  free?: boolean | null;
  hr?: boolean | null;
  release?: string | null;
  score?: number | null;
  labels: string[];
};

export type Receipt = {
  site_id: string;
  state:
    | "completed"
    | "already-completed"
    | "auth-expired"
    | "challenge"
    | "unknown"
    | "failed";
  observed_at: string;
  operation_id?: string | null;
  detail?: string | null;
};

export type MediaView = {
  id: string;
  title: string;
  revision: Revision;
  kind?: string;
  year?: number | null;
  library_id?: string | null;
  tmdb_id?: string | number | null;
  douban_id?: string | null;
};

export type SubscribeView = {
  id: string;
  media_id: string;
  revision: Revision;
  title: string;
};

export type DownloadView = {
  id: string;
  name: string;
  revision: Revision;
  state: string;
  site_id?: string | null;
  downloader_id: string;
};

export type BoostSnapshot = {
  revision: Revision;
  used_bytes: number | null;
  reserved_bytes: number | null;
  retained_file_bytes: number | null;
  disk: {
    available_bytes?: number | null;
    observed_at?: string | null;
    status: string;
  };
};

export interface QueryMap {
  "config.options": {
    input: PageInput & { kind: CatalogKind };
    result: Page<OptionView>;
  };
  "sites.list": { input: PageInput; result: Page<OptionView> };
  "downloaders.list": { input: PageInput; result: Page<OptionView> };
  "libraries.list": { input: PageInput; result: Page<OptionView> };
  "filters.list": { input: PageInput; result: Page<OptionView> };
  "subscribes.list": {
    input: PageInput & { media_id?: string };
    result: Page<SubscribeView>;
  };
  "media.get": { input: { media_id?: string; id?: string }; result: MediaView };
  "downloads.list": {
    input: PageInput & { site_id?: string; downloader_id?: string };
    result: Page<DownloadView>;
  };
  "checkin.receipts": {
    input: PageInput & { site_id: string };
    result: Page<Receipt>;
  };
  "boost.snapshot": {
    input: { site_id?: string; downloader_id?: string };
    result: BoostSnapshot;
  };
  "handles.resolve": {
    input: { kind: HandleKind; id: string; expected_revision?: Revision };
    result: ResolvedHandle;
  };
  "files.selection": {
    input: {
      library_id: string;
      file_ids: string[];
      expected_revisions: Revision[];
    };
    result: ResolvedHandle;
  };
  "indexer.cached": {
    input: PageInput & { search_operation_id: string };
    result: Page<TorrentView>;
  };
}

export type Json =
  | null
  | boolean
  | number
  | string
  | Json[]
  | { [key: string]: Json };

export interface CommandMap {
  "indexer.search": { sites: UiHandle[]; query: string; page?: number };
  "indexer.rss": { sites: UiHandle[]; page?: number };
  "site.maintenance": { site_handle: UiHandle; action: string };
  "site.create": never;
  "site.update": never;
  "site.delete": { site_handle: UiHandle; revision: Revision };
  "downloader.add": never;
  "downloader.control": never;
  "downloader.delete-task": {
    owned_task_handle: UiHandle;
    revision: Revision;
    reason: string;
  };
  "downloader.prepare-cleanup": {
    owned_task_handles: UiHandle[];
    expected_revision: Revision;
    reason: string;
  };
  "downloader.delete-files": {
    host_file_plan_handle: UiHandle;
    revision: Revision;
    reason: string;
    confirmation_token: string;
  };
  "subscribe.create": never;
  "subscribe.update": never;
  "subscribe.delete": {
    owned_subscribe_handle: UiHandle;
    revision: Revision;
    reason: string;
  };
  "subscribe.complete": {
    owned_subscribe_handle: UiHandle;
    revision: Revision;
    reason: string;
  };
  "library.refresh": { library_handle: UiHandle; selection: UiHandle };
  "transfer.request": never;
  "scrape.request": never;
  "notify.send": {
    title: string;
    body: string;
    level: "info" | "warning" | "error";
    link?: string;
    channel_ids: string[];
  };
  "schedule.set": {
    task_id: string;
    interval_seconds: number;
    enabled: boolean;
    revision: Revision;
  };
  "schedule.disable": { task_id: string; revision: Revision };
}

export interface NavigationMap {
  library: { library_id: string };
  media: {
    media_id: string;
    library_id?: string;
    kind?: string;
    tmdb_id?: string;
    douban_id?: string;
  };
  search: { query?: string };
  plugin: { surface_id: string };
}

export type Capability =
  | "read:sites"
  | "read:downloads"
  | "read:downloaders"
  | "read:history"
  | "read:boost"
  | "read:library"
  | "read:libraries"
  | "read:subscribes"
  | "read:catalog"
  | "command:site.check-in"
  | "command:site.verify-session"
  | `command:${keyof CommandMap}`
  | `task:run:${string}`;

export type Environment = {
  readonly theme: string;
  readonly language: string;
  readonly layout: string;
  readonly user: { readonly id: string; readonly name: string };
};

export type BrowserAvailability = {
  site_id: string;
  purpose: "search" | "maintenance";
  status: string;
  missing_requirements: string[];
  retry_after_ms?: number | null;
};

export type SdkErrorCode =
  | "permission-denied"
  | "host-disposed"
  | "contract-mismatch"
  | "stale-generation"
  | "stale-handle"
  | "operation-not-ready"
  | "revision-conflict"
  | "busy"
  | "request-failed"
  | "result-unknown";

export type SdkError = {
  code: SdkErrorCode;
  message: string;
  httpStatus?: number;
  fieldErrors?: Record<string, string>;
};

export const SDK_ERROR_CODES: readonly SdkErrorCode[] = [
  "permission-denied",
  "host-disposed",
  "contract-mismatch",
  "stale-generation",
  "stale-handle",
  "operation-not-ready",
  "revision-conflict",
  "busy",
  "request-failed",
  "result-unknown",
] as const;

export type PluginContextInfo = Readonly<{
  plugin_id: string;
  package_hash: string;
  generation: Revision;
  surface_id: string;
  login_epoch: string;
}>;

export interface PluginHostSDK {
  readonly context: PluginContextInfo;
  supports(name: Capability): boolean;
  environment: {
    get(): Environment;
    subscribe(listener: () => void): Dispose;
  };
  catalogs: {
    list(kind: CatalogKind, input: PageInput): Promise<Page<OptionView>>;
  };
  query<K extends keyof QueryMap>(
    name: K,
    input: QueryMap[K]["input"],
  ): Promise<QueryMap[K]["result"]>;
  objects: {
    resolve(
      kind: HandleKind,
      id: string,
      revision?: Revision,
    ): Promise<ResolvedHandle>;
  };
  search: {
    results(operationId: string, input: PageInput): Promise<Page<TorrentView>>;
    open(input: {
      query?: string;
      site_ids?: string[];
      media_id?: string;
    }): Promise<Opened>;
  };
  commands: {
    submit<K extends keyof CommandMap>(
      kind: K,
      payload: CommandMap[K],
      options: WriteOptions,
    ): Promise<OperationRef>;
  };
  tasks: {
    list(
      input: PageInput,
    ): Promise<
      Page<{
        task_id: string;
        title: string;
        enabled: boolean;
        definition_revision: Revision;
      }>
    >;
    run(
      taskId: string,
      input: Json,
      options: WriteOptions,
    ): Promise<OperationRef>;
    receipts(taskId: string, input: PageInput): Promise<Page<Receipt>>;
  };
  operations: {
    get(id: string): Promise<Operation>;
    watch(id: string, listener: (operation: Operation) => void): Dispose;
  };
  endpoint: {
    request(
      endpointId: string,
      input: {
        method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
        query?: Record<string, string>;
        body?: Json;
        signal?: AbortSignal;
      },
    ): Promise<Json | OperationRef>;
  };
  media: {
    select(input: { query?: string }): Promise<MediaView | null>;
    detail(input: { media_id: string }): Promise<Opened>;
  };
  subscriptions: {
    create(input: { media_id: string }): Promise<InteractionResult>;
    edit(input: { subscribe_id: string }): Promise<InteractionResult>;
  };
  downloads: {
    add(input: { torrent_handle: UiHandle }): Promise<InteractionResult>;
    cleanup(input: {
      owned_task_handles: UiHandle[];
      mode: "task-only" | "files";
    }): Promise<InteractionResult>;
  };
  libraries: {
    open(input: { library_id: string }): Promise<Opened>;
  };
  files: {
    select(
      input: PageInput & { library_id: string },
    ): Promise<ResolvedHandle | null>;
    transfer(input: {
      file_selection_handle: UiHandle;
    }): Promise<InteractionResult>;
    scrape(input: {
      file_selection_handle: UiHandle;
    }): Promise<InteractionResult>;
  };
  browser: {
    availability(input: {
      site_id: string;
      purpose: "search" | "maintenance";
    }): Promise<BrowserAvailability>;
  };
  ui: {
    toast(input: {
      message: string;
      level?: "info" | "warning" | "error";
    }): void;
    confirm(input: {
      title: string;
      message: string;
      confirmLabel: string;
    }): Promise<boolean>;
    dialog(component: ComponentType<{ close: Dispose }>): Dispose;
  };
  navigation: {
    open<K extends keyof NavigationMap>(
      route: K,
      params: NavigationMap[K],
    ): Promise<Opened>;
  };
  shell: {
    registerAction(action: {
      id: string;
      label: string;
      onSelect: () => void;
    }): Dispose;
    registerTabs(
      tabs: readonly { id: string; label: string; onSelect: () => void }[],
    ): Dispose;
  };
}

export interface PluginConfigController {
  values: Readonly<Record<string, unknown>>;
  schemaRevision: Revision;
  revision: Revision;
  editable: boolean;
  onDraftChange(values: Record<string, unknown>): void;
  save(values: Record<string, unknown>): Promise<void>;
}

export interface PluginSurfaceProps {
  host: PluginHostSDK;
  surfaceId: string;
  config?: PluginConfigController;
}

export function isTerminalOperation(state: OperationState): boolean {
  return (
    state === "succeeded" ||
    state === "rejected" ||
    state === "failed" ||
    state === "cancelled"
  );
}

export function isSdkError(error: unknown): error is SdkError {
  if (typeof error !== "object" || error === null) {
    return false;
  }
  const candidate = error as Partial<SdkError>;
  return (
    typeof candidate.code === "string" &&
    (SDK_ERROR_CODES as readonly string[]).includes(candidate.code) &&
    typeof candidate.message === "string"
  );
}
