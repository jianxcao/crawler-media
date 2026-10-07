import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

export interface MetadataSourceStatus {
  id: string;
  name: string;
  needs_key: boolean;
  configured: boolean;
}

export interface MetadataSettings {
  tmdb: { configured: boolean; api_key_hint: string | null; env_override?: boolean };
  tvdb: { configured: boolean; api_key_hint: string | null };
  sources: MetadataSourceStatus[];
}

export function getMetadataSettings(): Promise<MetadataSettings> {
  return unwrap(request<ApiEnvelope<MetadataSettings>>("/settings/metadata"));
}

export function saveMetadataSettings(body: {
  tmdb_api_key?: string;
  tvdb_api_key?: string;
}): Promise<MetadataSettings> {
  return unwrap(
    request<ApiEnvelope<MetadataSettings>>("/settings/metadata", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  );
}

export function testMetadataSettings(): Promise<{
  ok: boolean;
  result_count?: number;
  sample?: string;
  error?: string;
}> {
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; result_count?: number; sample?: string; error?: string }>>(
      "/settings/metadata/test",
      { method: "POST", body: "{}" },
    ),
  );
}

export interface ProxySettings {
  proxy_url: string;
  username: string;
  has_password: boolean;
  douban_bypass: boolean;
  allowed_domains?: string;
  active_proxy: string | null;
  env_override?: boolean;
}

export function getProxySettings(): Promise<ProxySettings> {
  return unwrap(request<ApiEnvelope<ProxySettings>>("/settings/proxy"));
}

export function saveProxySettings(body: {
  proxy_url?: string;
  username?: string;
  password?: string;
  douban_bypass?: boolean;
  allowed_domains?: string;
}): Promise<ProxySettings> {
  return unwrap(
    request<ApiEnvelope<ProxySettings>>("/settings/proxy", {
      method: "PUT",
      body: JSON.stringify(body),
    }),
  );
}

export function testProxySettings(body: {
  proxy_url?: string;
  username?: string;
  password?: string;
  target?: string;
}): Promise<{
  ok: boolean;
  latency_ms: number;
  error?: string | null;
}> {
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; latency_ms: number; error?: string | null }>>(
      "/settings/proxy/test",
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    ),
  );
}

export interface ProxyDomainDiagnostic {
  name: string;
  url: string;
  is_proxied: boolean;
  route_label: string;
  ok: boolean;
  latency_ms: number;
  error?: string | null;
}

export interface ProxyDiagnosticResult {
  active_proxy: string | null;
  douban_bypass: boolean;
  items: ProxyDomainDiagnostic[];
}

export function diagnoseProxySettings(): Promise<ProxyDiagnosticResult> {
  return unwrap(
    request<ApiEnvelope<ProxyDiagnosticResult>>("/settings/proxy/diagnose", {
      method: "POST",
      body: "{}",
    }),
  );
}
