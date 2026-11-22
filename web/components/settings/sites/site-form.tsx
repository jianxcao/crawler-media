"use client";

import { useMemo, useState } from "react";

import {
  type CatalogItem,
  configureSite,
  type Site,
  type SiteAuthType,
  type SiteConfigPayload,
} from "@/lib/api/sites";

/** 授权类型 → 中文名 */
export const AUTH_TYPE_LABEL: Record<SiteAuthType, string> = {
  cookie: "Cookie",
  apikey: "API 密钥",
  credential: "用户名密码",
  none: "无需授权",
};

/** 授权类型 → 表单必填字段（与 FIELD_META 配套） */
export const AUTH_TYPE_FIELDS: Record<SiteAuthType, string[]> = {
  cookie: ["cookie"],
  apikey: ["api_key"],
  credential: ["username", "password"],
  none: [],
};

/** 表单字段名 → 中文标签 & 输入类型 */
export const FIELD_META: Record<string, { label: string; kind: "text" | "password" | "textarea" }> = {
  cookie: { label: "Cookie 字符串", kind: "textarea" },
  api_key: { label: "API 密钥", kind: "password" },
  username: { label: "用户名", kind: "text" },
  password: { label: "密码", kind: "password" },
};

/* —— 添加站点：先选站点（带搜索），再填授权表单 —— */

interface AddSitePanelProps {
  available: CatalogItem[];
  onCreated: (site: Site) => void;
  onCancel: () => void;
  onError: (message: string) => void;
}

export function AddSitePanel({ available, onCreated, onCancel, onError }: AddSitePanelProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<CatalogItem | null>(null);
  const [busy, setBusy] = useState(false);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return available;
    return available.filter((c) => c.profile_id.toLowerCase().includes(q));
  }, [available, query]);

  if (selected) {
    return (
      <div className="rounded-xl border border-white/[0.08] bg-white/[0.03] p-4">
        <div className="mb-3 flex items-center justify-between gap-3">
          <div className="min-w-0">
            <p className="truncate text-body font-semibold text-[var(--text)]">
              {selected.profile_id}
            </p>
          </div>
          <button
            type="button"
            onClick={() => setSelected(null)}
            disabled={busy}
            className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium"
          >
            重新选择
          </button>
        </div>
        <SiteForm
          item={selected}
          site={null}
          busy={busy}
          onSubmit={async (payload) => {
            setBusy(true);
            try {
              onCreated(await configureSite(selected.profile_id, payload));
            } catch (e) {
              onError((e as Error).message);
            } finally {
              setBusy(false);
            }
          }}
        />
      </div>
    );
  }

  return (
    <div className="rounded-xl border border-white/[0.08] bg-white/[0.03] p-4">
      <div className="mb-3 flex items-center justify-between gap-3">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索站点名称 / 地址"
          autoFocus
          className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
        />
        <button
          type="button"
          onClick={onCancel}
          className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium"
        >
          取消
        </button>
      </div>

      <div className="scroll-thin max-h-64 space-y-1 overflow-y-auto">
        {available.length === 0 ? (
          <p className="px-2 py-6 text-center text-body text-[var(--text-muted)]">
            所有支持的站点都已配置。
          </p>
        ) : filtered.length === 0 ? (
          <p className="px-2 py-6 text-center text-body text-[var(--text-muted)]">
            没有匹配「{query}」的站点。
          </p>
        ) : (
          filtered.map((item) => (
            <button
              key={item.profile_id}
              type="button"
              onClick={() => setSelected(item)}
              className="glass-row nav-item w-full items-center justify-between gap-3 px-3 py-2.5 text-left"
            >
              <span className="min-w-0">
                <span className="block truncate text-ui font-medium text-[var(--text)]">
                  {item.profile_id}
                </span>
              </span>
              <span className="shrink-0 text-caption text-[var(--text-muted)]">
                {item.auth_types.map((a) => AUTH_TYPE_LABEL[a]).join(" / ")}
              </span>
            </button>
          ))
        )}
      </div>
    </div>
  );
}

/* —— 授权表单：根据所选授权类型渲染必填字段 —— */

interface SiteFormProps {
  item: CatalogItem;
  site: Site | null;
  busy: boolean;
  onSubmit: (payload: SiteConfigPayload) => void;
}

export function SiteForm({ item, site, busy, onSubmit }: SiteFormProps) {
  const options = item.auth_types.map((authType) => ({
    auth_type: authType,
    required_fields: AUTH_TYPE_FIELDS[authType],
  }));
  const initialAuthType: SiteAuthType =
    site && options.some((o) => o.auth_type === site.auth_type)
      ? site.auth_type
      : (options[0]?.auth_type ?? site?.auth_type ?? "cookie");
  const [authType, setAuthType] = useState<SiteAuthType>(initialAuthType);
  const [siteUrl, setSiteUrl] = useState(site?.url ?? "");
  const [maintenanceVia, setMaintenanceVia] = useState<"http" | "browser">(
    site?.maintenance_via ?? "http",
  );
  const [values, setValues] = useState<Record<string, string>>({});

  const current = options.find((o) => o.auth_type === authType) ?? options[0];
  const fields = current?.required_fields ?? [];
  const hasSavedActiveCredential = Boolean(
    site && site.auth_type !== "none" && site.auth_type === authType,
  );
  const canSubmit =
    hasSavedActiveCredential ||
    fields.length === 0 ||
    fields.every((f) => Boolean(values[f]?.trim()));

  function submit() {
    const payload: SiteConfigPayload = {
      auth_type: authType,
      enabled: site?.enabled ?? true,
      maintenance_via: authType === "apikey" ? "http" : maintenanceVia,
      ...(siteUrl.trim() ? { url: siteUrl.trim() } : {}),
    };
    if (authType === "apikey") {
      payload.cookie = "";
    } else if (authType === "cookie") {
      payload.api_key = "";
    } else if (authType === "none") {
      payload.cookie = "";
      payload.api_key = "";
    }
    for (const f of fields) {
      const trimmed = values[f]?.trim() ?? "";
      if (trimmed.length > 0 || !hasSavedActiveCredential) {
        (payload as unknown as Record<string, unknown>)[f] = trimmed;
      }
    }
    onSubmit(payload);
  }

  const credentialPlaceholder = hasSavedActiveCredential
    ? "留空则保留现有凭证；填写则替换"
    : site
      ? "出于安全，请填写新的授权凭证"
      : "";

  return (
    <div className="space-y-4">
      {authType !== "apikey" && (
        <div>
          <label className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
            登录与签到维护方式
          </label>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              onClick={() => setMaintenanceVia("http")}
              data-active={maintenanceVia === "http"}
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              标准 HTTP (支持代理)
            </button>
            <button
              type="button"
              onClick={() => setMaintenanceVia("browser")}
              data-active={maintenanceVia === "browser"}
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              无头浏览器 (CDP / Obscura)
            </button>
          </div>
          {maintenanceVia === "browser" && (
            <p className="mt-1 text-caption text-[var(--text-faint)]">
              浏览器维护方式不支持配置代理。若浏览器执行异常，系统将自动回退到 HTTP 流程。
            </p>
          )}
        </div>
      )}

      {options.length > 1 && (
        <div>
          <label className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
            授权方式
          </label>
          <div className="flex flex-wrap gap-2">
            {options.map((opt) => (
              <button
                key={opt.auth_type}
                type="button"
                onClick={() => setAuthType(opt.auth_type)}
                data-active={authType === opt.auth_type}
                className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
              >
                {AUTH_TYPE_LABEL[opt.auth_type]}
              </button>
            ))}
          </div>
        </div>
      )}

      <div>
        <label className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
          站点地址 (URL)
        </label>
        <input
          type="text"
          value={siteUrl}
          onChange={(e) => setSiteUrl(e.target.value)}
          placeholder={site?.url || "如留空则使用站点模板内置默认地址"}
          className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
        />
      </div>

      {fields.map((field) => {
        const fm = FIELD_META[field] ?? { label: field, kind: "text" as const };
        return (
          <div key={field}>
            <label className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
              {fm.label}
            </label>
            {field === "cookie" && (
              <p className="mb-1.5 text-caption text-[var(--text-faint)]">
                手动粘贴的 Cookie 过期后需重填；推荐手动粘贴 Cookie。
              </p>
            )}
            {fm.kind === "textarea" ? (
              <textarea
                value={values[field] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [field]: e.target.value }))}
                rows={3}
                autoComplete="off"
                placeholder={credentialPlaceholder}
                className="scroll-thin w-full resize-none rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
              />
            ) : (
              <input
                type={fm.kind}
                value={values[field] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [field]: e.target.value }))}
                placeholder={credentialPlaceholder}
                autoComplete={fm.kind === "password" ? "new-password" : "off"}
                className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
              />
            )}
          </div>
        );
      })}

      <div className="flex items-center justify-end gap-3 pt-1">
        <button
          type="button"
          onClick={submit}
          disabled={busy || !canSubmit}
          className="btn-accent rounded-full px-4.5 py-2 text-ui font-semibold disabled:opacity-40"
        >
          {busy ? "保存中…" : site ? "保存并重新验证" : "保存并验证"}
        </button>
      </div>
    </div>
  );
}
