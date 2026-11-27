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

export function newUuid(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  if (typeof crypto !== "undefined" && typeof crypto.getRandomValues === "function") {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < 16; i += 1) bytes[i] = Math.floor(Math.random() * 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

export function extractHttpErrorCode(err: unknown): string | null {
  if (!(err instanceof HttpError) || !err.details || typeof err.details !== "object") return null;
  const body = err.details as { error?: { code?: unknown } };
  return typeof body.error?.code === "string" ? body.error.code : null;
}

export function buildKeepSecrets(config: PluginConfigView | null | undefined): Record<string, SecretWriteAction> {
  if (!config?.secrets) return {};
  const out: Record<string, SecretWriteAction> = {};
  for (const [k, st] of Object.entries(config.secrets)) {
    if (st?.configured) out[k] = { action: "keep" };
  }
  return out;
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

function writeJson<T>(
  method: "POST" | "PUT",
  path: string,
  body: unknown,
  options?: {
    expectedRevision?: number | string;
    idempotencyKey?: string;
    extraHeaders?: Record<string, string>;
    timeoutMs?: number;
  },
): Promise<T> {
  return unwrap(
    request<ApiEnvelope<T>>(
      path,
      {
        method,
        headers: writeHeaders(options?.expectedRevision, options?.idempotencyKey, options?.extraHeaders),
        body: JSON.stringify(body),
      },
      options?.timeoutMs ? { timeoutMs: options.timeoutMs } : undefined,
    ),
  );
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

export interface ReauthTokenView { reauth_token: string; expires_at: string; purpose: string }
export interface PluginRepositoryView { id: string; name: string; index_url: string; enabled: boolean; builtin: boolean }
export interface PluginRepositoryInput { id: string; name: string; index_url: string; enabled: boolean; builtin?: boolean }
export interface PluginRepositoriesResponse { repositories: PluginRepositoryView[] }

export interface PluginCatalogEntryView {
  id: string; name: string; version: string; description: string;
  authors: string[]; license: string; package_url: string; signature_url: string | null;
  sha256: string; capabilities: string[]; homepage: string | null;
  installed_version: string | null; lifecycle: string | null; update_available: boolean;
}
export interface PluginRepositoryCatalogView { repo_id: string; repo_name: string; plugins: PluginCatalogEntryView[] }
export interface StagedUploadView {
  upload_id: string; hash: string; signature_status: string;
  signing_key_id: string | null; fingerprint: string | null; expires_at: string;
}
export interface GrantDiffView { added: PluginGrantSpec[]; kept: PluginGrantSpec[]; removed: PluginGrantSpec[]; required: string[] }
export interface PreparationImpactView { summary: string; requires_drain: boolean; requires_reauth: boolean; resets_state: boolean }
export interface InstallationPreparationView {
  preparation_id: string; plugin_id: string; version: string; package_hash: string; base_revision: number;
  signature_status: string; signing_key_id: string | null; fingerprint: string | null;
  required_capabilities: string[]; grant_diff: GrantDiffView; config_schema: Record<string, unknown>;
  config_schema_revision: string; state_schema: number; impact: PreparationImpactView;
  consent_token: string; expires_at: string;
}
export interface SchemaFieldErrorView { path?: string; field?: string; message: string }
export interface ConfigDiffView {
  old_schema_revision: string | null; new_schema_revision: string;
  target_config_digest: string; existing_valid: boolean; validation_errors: SchemaFieldErrorView[];
}
export interface StateResetPreviewView { kv_entries: number; blob_entries: number; kv_bytes: number; blob_bytes: number; state_revision: number }
export interface StateDiffView {
  old_state_schema: number; new_state_schema: number; schema_changed: boolean;
  preserve_allowed: boolean; reset_preview: StateResetPreviewView | null;
}
export interface UpgradePreparationView {
  preparation_id: string; plugin_id: string; version: string; package_hash: string; base_revision: number;
  signature_status: string; signing_key_id: string | null; state_policy: string; target_config_digest: string;
  grant_diff: GrantDiffView; config_diff: ConfigDiffView; state_diff: StateDiffView;
  impact: PreparationImpactView; consent_token: string; expires_at: string;
}
export interface PrepareInstallRequest { upload_id?: string; repo_id?: string; plugin_id?: string; version?: string }
export interface ExecuteInstallRequest { preparation_id: string; consent_token: string; grants: PluginGrantSpec[] }
export interface PrepareUpgradeRequest {
  version?: string; upload_id?: string; package_hash?: string; repo_id?: string;
  target_config?: { schema_revision: string; values: Record<string, unknown>; secrets?: Record<string, SecretWriteAction> };
  site_id?: string; state_policy: "preserve" | "reset";
}
export interface ExecuteUpgradeRequest {
  preparation_id: string; consent_token: string; grants: PluginGrantSpec[];
  state_policy: "preserve" | "reset"; target_config_digest: string; reauth_token?: string;
}
export interface PluginSettingsView { developer_mode: boolean; revision: number; updated_at: string | null; updated_by: string | null }
export interface TrustKeyView {
  key_id: string; label: string; public_key: string; fingerprint: string; status: string;
  revision: number; associated_package_count: number; created_at: string; created_by: string;
  revoked_at: string | null; revoked_by: string | null; revoked_reason: string | null;
}
export interface TrustKeyPreparationView {
  preparation_id: string; key_id: string; label: string; public_key: string;
  fingerprint: string; consent_token: string; expires_at: string;
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

function attrHeaders(attribution?: { packageHash: string; generation: number }): Record<string, string> | undefined {
  return attribution
    ? { "X-Plugin-Package": attribution.packageHash, "X-Plugin-Generation": String(attribution.generation) }
    : undefined;
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
  if (body.site_id?.trim()) payload.site_id = body.site_id.trim();
  return writeJson("PUT", `/plugins/${encodeURIComponent(pluginId)}/config`, payload, {
    expectedRevision,
    extraHeaders: attrHeaders(attribution),
  });
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
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/permissions/grant`, body, { expectedRevision });
}

export function revokePermissions(
  pluginId: string,
  body: { grants: PluginGrantSpec[]; reason: string },
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/permissions/revoke`, body, { expectedRevision });
}

export function enablePlugin(pluginId: string, expectedRevision: number | string): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/enable`, {}, { expectedRevision });
}

export function disablePlugin(pluginId: string, expectedRevision: number | string, reason?: string): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/disable`, { reason: reason ?? null }, { expectedRevision });
}

export function uninstallPlugin(
  pluginId: string,
  removePrivateData: boolean,
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/uninstall`, { remove_private_data: removePrivateData }, { expectedRevision });
}

export function listPluginTasks(pluginId: string): Promise<{ items: PluginTaskView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<PluginTaskView[]>>(`/plugins/${encodeURIComponent(pluginId)}/tasks?limit=50`));
}

export function updatePluginTask(
  pluginId: string,
  taskId: string,
  body: { enabled: boolean; schedule: { interval_secs: number }; config: Record<string, unknown> },
  expectedRevision: number | string,
  idempotencyKey?: string,
  attribution?: { packageHash: string; generation: number },
): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/update`, body, {
    expectedRevision,
    idempotencyKey,
    extraHeaders: attrHeaders(attribution),
  });
}

export function runPluginTask(
  pluginId: string,
  taskId: string,
  input: Record<string, unknown>,
  expectedRevision: number | string,
  idempotencyKey?: string,
  attribution?: { packageHash: string; generation: number },
): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/run`, { input }, {
    expectedRevision,
    idempotencyKey,
    extraHeaders: attrHeaders(attribution),
  });
}

export function listPluginTaskReceipts(pluginId: string, taskId: string): Promise<{ items: TaskReceiptView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<TaskReceiptView[]>>(`/plugins/${encodeURIComponent(pluginId)}/tasks/${encodeURIComponent(taskId)}/receipts?limit=30`));
}

export function listPluginAudit(pluginId: string): Promise<{ items: AuditEventView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<AuditEventView[]>>(`/plugins/${encodeURIComponent(pluginId)}/audit?limit=50`));
}

export async function listPluginEvents(pluginId: string, status?: string): Promise<PluginEventDeliveryView[]> {
  const qs = status ? `?status=${encodeURIComponent(status)}` : "";
  const data = await unwrap(request<ApiEnvelope<{ items: PluginEventDeliveryView[] }>>(`/plugins/${encodeURIComponent(pluginId)}/events${qs}`));
  return data.items;
}

export function requeuePluginEvent(pluginId: string, eventId: string): Promise<{ event_id: string; status: string }> {
  return unwrap(request<ApiEnvelope<{ event_id: string; status: string }>>(`/plugins/${encodeURIComponent(pluginId)}/events/${encodeURIComponent(eventId)}/requeue`, { method: "POST", body: "{}" }));
}

export function getPluginOperation(operationId: string): Promise<OperationAdminView> {
  return unwrap(request<ApiEnvelope<OperationAdminView>>(`/plugin-operations/${encodeURIComponent(operationId)}`));
}

export function reauthPluginAdmin(password: string, purpose: string): Promise<ReauthTokenView> {
  return unwrap(request<ApiEnvelope<ReauthTokenView>>("/plugin-reauth", { method: "POST", body: JSON.stringify({ password, purpose }) }));
}

export function preparePluginUiTrust(pluginId: string, packageHash: string): Promise<UiTrustPreparationView> {
  return unwrap(request<ApiEnvelope<UiTrustPreparationView>>(`/plugins/${encodeURIComponent(pluginId)}/ui-trust/prepare`, { method: "POST", body: JSON.stringify({ package_hash: packageHash }) }));
}

export function grantPluginUiTrust(
  pluginId: string,
  body: { preparation_id: string; package_hash: string; consent_token: string; reauth_token: string },
  expectedRevision: number | string,
): Promise<UiTrustActionView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/ui-trust/grant`, body, { expectedRevision });
}

export function revokePluginUiTrust(
  pluginId: string,
  body: { package_hash: string; reason?: string },
  expectedRevision: number | string,
): Promise<UiTrustActionView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/ui-trust/revoke`, body, { expectedRevision });
}

export function listPluginRepositories(init?: RequestInit): Promise<PluginRepositoriesResponse> {
  return unwrap(request<ApiEnvelope<PluginRepositoriesResponse>>("/plugin-repositories", init));
}

export function savePluginRepositories(repositories: PluginRepositoryInput[]): Promise<PluginRepositoriesResponse> {
  return writeJson("PUT", "/plugin-repositories", { repositories });
}

export function getPluginRepositoryCatalog(repoId: string, init?: RequestInit): Promise<PluginRepositoryCatalogView> {
  return unwrap(request<ApiEnvelope<PluginRepositoryCatalogView>>(`/plugin-repositories/${encodeURIComponent(repoId)}/catalog`, init, { timeoutMs: 30_000 }));
}

export function stagePluginUpload(
  packageFile: File,
  signatureFile?: File,
  options?: { reauthToken?: string; expectedSha256?: string; uploadId?: string },
): Promise<StagedUploadView> {
  const form = new FormData();
  form.append("upload_id", options?.uploadId ?? newUuid());
  form.append("file", packageFile);
  if (signatureFile) form.append("signature", signatureFile);
  if (options?.reauthToken) form.append("reauth_token", options.reauthToken);
  if (options?.expectedSha256?.trim()) form.append("expected_sha256", options.expectedSha256.trim());
  return unwrap(
    request<ApiEnvelope<StagedUploadView>>(
      "/plugin-uploads",
      { method: "POST", headers: writeHeaders("0"), body: form },
      { timeoutMs: 60_000 },
    ),
  );
}

export function preparePluginInstall(body: PrepareInstallRequest): Promise<OperationAdminView> {
  return writeJson("POST", "/plugin-installations/prepare", body, { expectedRevision: "0", timeoutMs: 60_000 });
}

export function executePluginInstall(body: ExecuteInstallRequest): Promise<OperationAdminView> {
  return writeJson("POST", "/plugin-installations", body, { expectedRevision: "0", timeoutMs: 60_000 });
}

export function preparePluginUpgrade(pluginId: string, body: PrepareUpgradeRequest, expectedRevision: number | string): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/upgrades/prepare`, body, { expectedRevision, timeoutMs: 60_000 });
}

export function executePluginUpgrade(pluginId: string, body: ExecuteUpgradeRequest, expectedRevision: number | string): Promise<OperationAdminView> {
  return writeJson("POST", `/plugins/${encodeURIComponent(pluginId)}/upgrade`, body, { expectedRevision, timeoutMs: 60_000 });
}

export function getPluginSettings(init?: RequestInit): Promise<PluginSettingsView> {
  return unwrap(request<ApiEnvelope<PluginSettingsView>>("/settings/plugins", init));
}

export function enableDeveloperMode(reauthToken: string, expectedRevision: number | string): Promise<PluginSettingsView> {
  return writeJson("POST", "/settings/plugins/developer/enable", { reauth_token: reauthToken }, { expectedRevision });
}

export function disableDeveloperMode(expectedRevision: number | string): Promise<OperationAdminView> {
  return writeJson("POST", "/settings/plugins/developer/disable", {}, { expectedRevision });
}

export function listTrustKeys(init?: RequestInit): Promise<{ items: TrustKeyView[]; next_cursor: string | null }> {
  return unwrapPaged(request<ApiEnvelope<TrustKeyView[]>>("/plugin-trust-keys?limit=100", init));
}

export function prepareTrustKey(publicKey: string, label: string): Promise<TrustKeyPreparationView> {
  return unwrap(request<ApiEnvelope<TrustKeyPreparationView>>("/plugin-trust-keys/prepare", { method: "POST", body: JSON.stringify({ public_key: publicKey, label }) }));
}

export function createTrustKey(body: {
  label?: string;
  public_key: string;
  fingerprint: string;
  consent_token: string;
  reauth_token: string;
}): Promise<TrustKeyView> {
  return writeJson("POST", "/plugin-trust-keys", body, { expectedRevision: "0" });
}

export function revokeTrustKey(
  keyId: string,
  body: { reason: string; reauth_token: string },
  expectedRevision: number | string,
): Promise<OperationAdminView> {
  return writeJson("POST", `/plugin-trust-keys/${encodeURIComponent(keyId)}/revoke`, body, { expectedRevision });
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
