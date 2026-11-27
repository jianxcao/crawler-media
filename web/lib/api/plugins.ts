import { publicEnv } from "@/lib/env";
import { HttpError, request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
  next_cursor?: string | null;
}

function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return promise.then((env) => {
    if (env.ok !== true) throw new Error("响应不是 {ok:true, data} 信封");
    return env.data;
  });
}

function unwrapPaged<T>(
  promise: Promise<ApiEnvelope<T[]>>,
): Promise<{ items: T[]; next_cursor: string | null }> {
  return promise.then((env) => {
    if (env.ok !== true) throw new Error("响应不是 {ok:true, data} 信封");
    return { items: env.data, next_cursor: env.next_cursor ?? null };
  });
}

export function ifMatchHeader(revision: number | string): string {
  const s = String(revision).trim();
  return s.startsWith('"') && s.endsWith('"') ? s : `"${s}"`;
}

export function newIdempotencyKey(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `cm-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function writeHeaders(
  expectedRevision?: number | string,
  idempotencyKey?: string,
  extra?: Record<string, string>,
): Record<string, string> {
  const headers: Record<string, string> = {
    "Idempotency-Key": idempotencyKey ?? newIdempotencyKey(),
    ...extra,
  };
  if (expectedRevision !== undefined && expectedRevision !== "") {
    headers["If-Match"] = ifMatchHeader(expectedRevision);
  }
  return headers;
}

export interface PluginUiContract {
  contract: string;
  react: string;
  react_dom: string;
  plugin_ui_sdk: string;
  plugin_ui_kit: string;
  federation_runtime_version: string;
  theme_schema_version?: string;
  sdk?: string;
  sdk_methods: string[];
}

export type UiSurfaceKind = "config" | "page" | "app" | "dashboard";

export interface UiSurfaceDescriptorView {
  id: string;
  kind: UiSurfaceKind;
  expose: string;
  title: string;
  icon: string | null;
  location: string | null;
  mountable: boolean;
  editable: boolean;
  required_role: string;
}

export interface UiTrustStateView {
  status: string;
  revision: number;
  granted_at: string | null;
  granted_by_user_id: string | null;
  revoked_at: string | null;
}

export interface UiDescriptorView {
  plugin_id: string;
  plugin_name: string;
  version: string;
  generation: number;
  lifecycle: string;
  source: string;
  package_hash: string;
  package_sha256: string;
  bundle_sha256: string;
  subject_user_id: string;
  subject_role: string;
  ui_mode: string;
  remote_name: string;
  entry: string;
  entry_url: string;
  asset_base_url: string;
  contract: string;
  react: string;
  react_dom: string;
  ui_trusted: boolean;
  ui_assets_present?: boolean;
  trust: UiTrustStateView;
  capabilities: string[];
  styles?: string[];
  surfaces: UiSurfaceDescriptorView[];
}

export interface UiNavigationItemView {
  plugin_id: string;
  plugin_name: string;
  generation: number;
  remote_name: string;
  entry_url: string;
  id: string;
  surface_id: string;
  kind: UiSurfaceKind;
  title: string;
  icon: string | null;
  expose: string;
  location: string;
  path: string;
  required_role: string;
  subject_user_id: string;
}

export interface PluginGrantSpec {
  capability: string;
  scope: "Global" | { Site: string } | { Object: { kind: string; id: string } };
  sensitive: boolean;
}

export interface PluginSummaryView {
  id: string;
  name: string;
  version: string;
  package_hash: string;
  source: string;
  revision: number;
  config_revision: number;
  generation: number;
  enabled: boolean;
  lifecycle: string;
  health: string;
  required_grants: string[];
  effective_grants: PluginGrantSpec[];
  active_operation_id: string | null;
  failure_reason: string | null;
}

export interface PluginDetailView extends PluginSummaryView {
  signing_key_id: string | null;
  state_schema: number;
  credential_bound: boolean;
  task_count: number;
  version_count: number;
  manifest?: Record<string, unknown>;
}

export interface PluginConfigSchemaView {
  plugin_id: string;
  schema_revision: string;
  schema: Record<string, unknown>;
}

export interface SecretConfiguredStatus {
  configured: boolean;
  updated_at?: string | null;
  updated_by?: string | null;
}

export type SecretWriteAction =
  | { action: "keep" }
  | { action: "clear" }
  | { action: "replace"; value: string };

export interface PluginConfigView {
  plugin_id: string;
  revision: number;
  config_revision: number;
  schema_revision: string;
  values: Record<string, unknown>;
  secrets: Record<string, SecretConfiguredStatus>;
}

export interface GrantView {
  capability: string;
  scope: PluginGrantSpec["scope"];
  sensitive: boolean;
  status: string;
}

export interface PluginPermissionsView {
  plugin_id: string;
  package_hash: string;
  revision: number;
  required_capabilities: string[];
  effective_grants: GrantView[];
}

export interface PluginTaskView {
  id: string;
  binding_id: string;
  plugin_id: string;
  definition_revision: number;
  enabled: boolean;
  schedule: { interval_secs: number | null };
  config: Record<string, unknown>;
  last_job: string | null;
  next_run_after: string | null;
  ownership_summary: string;
}

export interface TaskReceiptView {
  command_id: string;
  operation_id: string;
  job_id: string | null;
  generation: number | null;
  state: string;
  phase: string;
  effect: string;
  evidence: string | null;
  observed_at: string;
}

export interface OperationAdminView {
  operation_id: string;
  plugin_id: string | null;
  kind: string;
  state: string;
  phase: string;
  revision: number;
  generation: number | null;
  job_id: string | null;
  progress: number;
  blocked_reason: string | null;
  error: string | null;
  result: unknown;
  next_run_at: string | null;
  retry_after_ms: number | null;
  created_at: string;
  updated_at: string;
}

export interface AuditEventView {
  operation_id: string;
  plugin_id: string | null;
  action: string;
  subject_kind: string;
  canonical_method: string;
  canonical_path: string;
  state: string;
  phase: string;
  effect: string;
  created_at: string;
  updated_at: string;
}

export interface PluginEventDeliveryView {
  event_id: string;
  db: string;
  plugin_id: string;
  event_domain: string;
  event_kind: string;
  entity_type: string;
  entity_id: string;
  entity_sequence: number;
  entity_revision: number;
  cause_id: string;
  status: string;
  attempt_count: number;
  next_attempt_at: number;
  last_error: string | null;
  delivered_at: number | null;
  dead_lettered_at: number | null;
  revision: number;
  created_at: number;
  updated_at: number;
}

export interface UiTrustPreparationView {
  preparation_id: string;
  plugin_id: string;
  package_hash: string;
  package_sha256: string;
  bundle_sha256: string;
  remote_name: string;
  capabilities: string[];
  surfaces: Array<{ id: string; kind: UiSurfaceKind; title: string }>;
  trust_revision: number;
  consent_token: string;
  expires_at: string;
}

export interface UiTrustActionView {
  plugin_id: string;
  package_hash: string;
  package_sha256: string;
  bundle_sha256: string;
  status: string;
  ui_trusted: boolean;
  revision: number;
  granted_by_user_id: string;
  granted_at: string;
  revoked_at: string | null;
}

export interface ReauthTokenView {
  reauth_token: string;
  expires_at: string;
  purpose: string;
}

/**
 * Cookie-only preflight check BEFORE loading remote federation assets.
 * Deliberately omits Authorization Bearer so browser cookie identity matches subject_user_id.
 */
export async function fetchPluginUiDescriptor(
  pluginId: string,
  signal?: AbortSignal,
): Promise<UiDescriptorView> {
  const res = await fetch(`${publicEnv.apiBaseUrl}/plugins/${encodeURIComponent(pluginId)}/ui`, {
    method: "GET",
    credentials: "include",
    headers: { Accept: "application/json" },
    signal,
  });
  const payload = (await res.json().catch(() => null)) as
    | { ok?: boolean; data?: UiDescriptorView; error?: { code?: string; message?: string } }
    | null;
  if (!res.ok || !payload || payload.ok !== true) {
    throw new HttpError(
      payload?.error?.message ?? `UI preflight failed (${res.status})`,
      res.status,
      payload?.error?.code ?? null,
    );
  }
  return payload.data as UiDescriptorView;
}

export function fetchPluginUiContract(signal?: AbortSignal): Promise<PluginUiContract> {
  return unwrap(request<ApiEnvelope<PluginUiContract>>("/plugin-ui-contract", { signal }));
}

export function fetchPluginUiNavigation(signal?: AbortSignal): Promise<UiNavigationItemView[]> {
  return unwrap(request<ApiEnvelope<UiNavigationItemView[]>>("/plugin-ui-navigation", { signal }));
}

export function listPlugins(init?: RequestInit): Promise<{ items: PluginSummaryView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<PluginSummaryView[]>>("/plugins?limit=100", init));
}

export function getPluginDetail(pluginId: string, init?: RequestInit): Promise<PluginDetailView> {
  return unwrap(request<ApiEnvelope<PluginDetailView>>(`/plugins/${encodeURIComponent(pluginId)}`, init));
}

export function getPluginConfigSchema(pluginId: string, init?: RequestInit): Promise<PluginConfigSchemaView> {
  return unwrap(request<ApiEnvelope<PluginConfigSchemaView>>(`/plugins/${encodeURIComponent(pluginId)}/config-schema`, init));
}

export function getPluginConfig(pluginId: string, init?: RequestInit): Promise<PluginConfigView> {
  return unwrap(request<ApiEnvelope<PluginConfigView>>(`/plugins/${encodeURIComponent(pluginId)}/config`, init));
}

export function updatePluginConfig(
  pluginId: string,
  body: {
    schema_revision: string;
    values: Record<string, unknown>;
    secrets?: Record<string, SecretWriteAction>;
    site_id?: string;
  },
  expectedRevision: number | string,
  attribution?: { packageHash: string; generation: number },
): Promise<PluginConfigView> {
  const payload: Record<string, unknown> = {
    schema_revision: body.schema_revision,
    values: body.values,
    secrets: body.secrets ?? {},
  };
  if (body.site_id && body.site_id.trim()) {
    payload.site_id = body.site_id.trim();
  }
  return unwrap(
    request<ApiEnvelope<PluginConfigView>>(`/plugins/${encodeURIComponent(pluginId)}/config`, {
      method: "PUT",
      headers: writeHeaders(
        expectedRevision,
        undefined,
        attribution
          ? {
              "X-Plugin-Package": attribution.packageHash,
              "X-Plugin-Generation": String(attribution.generation),
            }
          : undefined,
      ),
      body: JSON.stringify(payload),
    }),
  );
}

export function getPluginPermissions(pluginId: string): Promise<PluginPermissionsView> {
  return unwrap(request<ApiEnvelope<PluginPermissionsView>>(`/plugins/${encodeURIComponent(pluginId)}/permissions`));
}

export function preparePermissions(
  pluginId: string,
  requestedGrants: PluginGrantSpec[],
): Promise<{ preparation_id: string; consent_token: string; impact: { summary: string }; grant_diff: unknown }> {
  return unwrap(
    request<ApiEnvelope<{ preparation_id: string; consent_token: string; impact: { summary: string }; grant_diff: unknown }>>(
      `/plugins/${encodeURIComponent(pluginId)}/permissions/prepare`,
      { method: "POST", body: JSON.stringify({ requested_grants: requestedGrants }) },
    ),
  );
}

export function grantPermissions(
  pluginId: string,
  body: { consent_token: string; grants: PluginGrantSpec[] },
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(`/plugins/${encodeURIComponent(pluginId)}/permissions/grant`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify(body),
    }),
  );
}

export function revokePermissions(
  pluginId: string,
  body: { grants: PluginGrantSpec[]; reason: string },
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(`/plugins/${encodeURIComponent(pluginId)}/permissions/revoke`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify(body),
    }),
  );
}

export function enablePlugin(pluginId: string, expectedRevision: number | string): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(`/plugins/${encodeURIComponent(pluginId)}/enable`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: "{}",
    }),
  );
}

export function disablePlugin(pluginId: string, expectedRevision: number | string, reason?: string): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(`/plugins/${encodeURIComponent(pluginId)}/disable`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify({ reason: reason ?? null }),
    }),
  );
}

export function uninstallPlugin(
  pluginId: string,
  removePrivateData: boolean,
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(`/plugins/${encodeURIComponent(pluginId)}/uninstall`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify({ remove_private_data: removePrivateData }),
    }),
  );
}

export function listPluginTasks(pluginId: string): Promise<{ items: PluginTaskView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<PluginTaskView[]>>(`/plugins/${encodeURIComponent(pluginId)}/tasks?limit=50`));
}

export function updatePluginTask(
  pluginId: string,
  taskId: string,
  body: {
    enabled: boolean;
    schedule: { interval_secs: number };
    config: Record<string, unknown>;
  },
  expectedRevision: number | string,
  idempotencyKey?: string,
  attribution?: { packageHash: string; generation: number },
): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(
      `/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/update`,
      {
        method: "POST",
        headers: writeHeaders(
          expectedRevision,
          idempotencyKey,
          attribution
            ? {
                "X-Plugin-Package": attribution.packageHash,
                "X-Plugin-Generation": String(attribution.generation),
              }
            : undefined,
        ),
        body: JSON.stringify(body),
      },
    ),
  );
}

export function runPluginTask(
  pluginId: string,
  taskId: string,
  input: Record<string, unknown>,
  expectedRevision: number | string,
  idempotencyKey?: string,
  attribution?: { packageHash: string; generation: number },
): Promise<OperationAdminView> {
  return unwrap(
    request<ApiEnvelope<OperationAdminView>>(
      `/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/run`,
      {
        method: "POST",
        headers: writeHeaders(
          expectedRevision,
          idempotencyKey,
          attribution
            ? {
                "X-Plugin-Package": attribution.packageHash,
                "X-Plugin-Generation": String(attribution.generation),
              }
            : undefined,
        ),
        body: JSON.stringify({ input }),
      },
    ),
  );
}

export function listPluginTaskReceipts(pluginId: string, taskId: string): Promise<{ items: TaskReceiptView[]; next_cursor: string | null }> {
  return unwrapPaged(
    request<ApiEnvelope<TaskReceiptView[]>>(
      `/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/receipts?limit=30`,
    ),
  );
}

export function listPluginAudit(pluginId: string): Promise<{ items: AuditEventView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<AuditEventView[]>>(`/plugins/${encodeURIComponent(pluginId)}/audit?limit=50`));
}

export async function listPluginEvents(pluginId: string, status?: string): Promise<PluginEventDeliveryView[]> {
  const qs = status ? `?status=${encodeURIComponent(status)}` : "";
  const data = await unwrap(
    request<ApiEnvelope<{ items: PluginEventDeliveryView[] }>>(`/plugins/${encodeURIComponent(pluginId)}/events${qs}`),
  );
  return data.items;
}

export function requeuePluginEvent(pluginId: string, eventId: string): Promise<{ event_id: string; status: string }> {
  return unwrap(
    request<ApiEnvelope<{ event_id: string; status: string }>>(
      `/plugins/${encodeURIComponent(pluginId)}/events/${encodeURIComponent(eventId)}/requeue`,
      { method: "POST", body: "{}" },
    ),
  );
}

export function getPluginOperation(operationId: string): Promise<OperationAdminView> {
  return unwrap(request<ApiEnvelope<OperationAdminView>>(`/plugin-operations/${encodeURIComponent(operationId)}`));
}

export function reauthPluginAdmin(password: string, purpose: string): Promise<ReauthTokenView> {
  return unwrap(
    request<ApiEnvelope<ReauthTokenView>>("/plugin-reauth", {
      method: "POST",
      body: JSON.stringify({ password, purpose }),
    }),
  );
}

export function preparePluginUiTrust(pluginId: string, packageHash: string): Promise<UiTrustPreparationView> {
  return unwrap(
    request<ApiEnvelope<UiTrustPreparationView>>(`/plugins/${encodeURIComponent(pluginId)}/ui-trust/prepare`, {
      method: "POST",
      body: JSON.stringify({ package_hash: packageHash }),
    }),
  );
}

export function grantPluginUiTrust(
  pluginId: string,
  body: {
    preparation_id: string;
    package_hash: string;
    consent_token: string;
    reauth_token: string;
  },
  expectedRevision: number | string,
): Promise<UiTrustActionView> {
  return unwrap(
    request<ApiEnvelope<UiTrustActionView>>(`/plugins/${encodeURIComponent(pluginId)}/ui-trust/grant`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify(body),
    }),
  );
}

export function revokePluginUiTrust(
  pluginId: string,
  body: { package_hash: string; reason?: string },
  expectedRevision: number | string,
): Promise<UiTrustActionView> {
  return unwrap(
    request<ApiEnvelope<UiTrustActionView>>(`/plugins/${encodeURIComponent(pluginId)}/ui-trust/revoke`, {
      method: "POST",
      headers: writeHeaders(expectedRevision),
      body: JSON.stringify(body),
    }),
  );
}

const TERMINAL_OPERATION_STATES = ["succeeded", "failed", "rejected", "cancelled"];

export function ensureOperationSucceeded(op: OperationAdminView): OperationAdminView {
  if (op.state !== "succeeded") {
    throw new Error(op.error || op.blocked_reason || `操作未完成 (${op.state}: ${op.phase})`);
  }
  return op;
}

export function isPluginConfigEditable(detail: {
  lifecycle: string;
  enabled: boolean;
  active_operation_id: string | null;
}): boolean {
  return (
    (detail.lifecycle === "installed-disabled" || detail.lifecycle === "disabled") &&
    !detail.enabled &&
    !detail.active_operation_id
  );
}

export async function waitOperationTerminal(
  operationOrId: string | OperationAdminView,
  timeoutMs = 15_000,
): Promise<OperationAdminView> {
  if (
    typeof operationOrId !== "string" &&
    TERMINAL_OPERATION_STATES.includes(operationOrId.state)
  ) {
    return operationOrId;
  }
  const operationId =
    typeof operationOrId === "string" ? operationOrId : operationOrId.operation_id;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const op = await getPluginOperation(operationId);
    if (TERMINAL_OPERATION_STATES.includes(op.state)) {
      return op;
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  return getPluginOperation(operationId);
}
