"use client";

import React, { useEffect, useMemo, useRef, useState } from "react";

import {
  ObjectFieldsGroup,
  type SchemaOptionItem,
} from "@/components/plugins/PluginSchemaFields";
import type { SecretConfiguredStatus, SecretWriteAction } from "@/lib/api/plugins";
import {
  buildInitialValues,
  computeSecretPaths,
  extractSchemaDefs,
  isPlainObject,
  type JsonSchemaProperty,
  jsonValuesEqual,
  mergeSchemaNode,
  resolveSchemaPropAtPath,
  schemaHasSecrets,
  setSchemaValueAtPath,
  stripSecretPaths,
  validateAndNormalizeObject,
  validateStringConstraints,
} from "@/lib/plugins/schema-helpers";

export { schemaHasSecrets, type SchemaOptionItem };

function stripSitePrefix(raw: string): string {
  return raw.trim().replace(/^site:/i, "").trim();
}

function extractBoundSites(obj: Record<string, unknown>): string[] | undefined {
  if (typeof obj.site_id === "string" && obj.site_id.trim()) {
    return [obj.site_id.trim()];
  }
  if (Array.isArray(obj.sites)) {
    return obj.sites
      .filter((s): s is string => typeof s === "string" && s.trim().length > 0)
      .map((s) => s.trim());
  }
  if (typeof obj.site_id === "string") {
    return [];
  }
  return undefined;
}

function syncSecretSiteId(prev: string, bound: string[] | undefined): string {
  if (bound === undefined) return prev;
  if (bound.length === 1) return bound[0];
  if (bound.length === 0) return "";
  return bound.includes(prev) ? prev : "";
}

function isSecretRequiredForWrite(
  mergedRoot: JsonSchemaProperty,
  defs: Record<string, JsonSchemaProperty>,
  segs: readonly string[],
  outValues: Record<string, unknown>,
  secretActions: Record<string, "keep" | "replace" | "clear">,
  secretPaths: ReadonlyMap<string, string[]>,
  secrets: Record<string, SecretConfiguredStatus>,
): boolean {
  if (segs.length === 0) return false;
  const parentSegs = segs.slice(0, -1);
  const leafKey = segs[segs.length - 1];
  const parentNode = resolveSchemaPropAtPath(mergedRoot, parentSegs, defs);
  const parentReq = Array.isArray(parentNode.required) ? parentReqMap(parentNode.required) : [];
  if (!parentReq.includes(leafKey)) return false;
  if (segs.length === 1) return true;

  let curVal: unknown = outValues;
  let curSchema: JsonSchemaProperty = mergedRoot;
  for (let depth = 0; depth < segs.length - 1; depth += 1) {
    const seg = segs[depth];
    const prefix = segs.slice(0, depth + 1);
    const inValues = isPlainObject(curVal) && seg in curVal;
    const reqList = Array.isArray(curSchema.required) ? curSchema.required.map(String) : [];
    const inRequired = reqList.includes(seg);
    let hasSuppliedChildSecret = false;
    for (const [actKey, otherSegs] of secretPaths.entries()) {
      const act = secretActions[actKey] ?? "keep";
      const isSupplied =
        act === "replace" || (act === "keep" && Boolean(secrets[actKey]?.configured));
      if (!isSupplied) continue;
      if (
        otherSegs.length >= prefix.length &&
        prefix.every((p, idx) => otherSegs[idx] === p)
      ) {
        hasSuppliedChildSecret = true;
        break;
      }
    }
    if (!(inValues || inRequired || hasSuppliedChildSecret)) {
      return false;
    }
    curVal = isPlainObject(curVal) ? curVal[seg] : undefined;
    const props = isPlainObject(curSchema.properties)
      ? (curSchema.properties as Record<string, JsonSchemaProperty>)
      : undefined;
    curSchema = mergeSchemaNode(props?.[seg], defs, depth + 1);
  }
  return true;
}

function parentReqMap(req: unknown[]): string[] {
  return req.map(String);
}

export interface PluginSchemaFormProps {
  schema: Record<string, unknown>;
  values: Record<string, unknown>;
  secrets: Record<string, SecretConfiguredStatus>;
  editable: boolean;
  sites: SchemaOptionItem[];
  downloaders: SchemaOptionItem[];
  onSave: (
    values: Record<string, unknown>,
    secrets: Record<string, SecretWriteAction>,
    siteId?: string,
  ) => Promise<void>;
  externalError?: string | null;
  hideNonSecrets?: boolean;
  credentialBound?: boolean;
}

export function PluginSchemaForm({
  schema,
  values,
  secrets,
  editable,
  sites,
  downloaders,
  onSave,
  externalError,
  hideNonSecrets = false,
  credentialBound = false,
}: PluginSchemaFormProps) {
  const defs = useMemo(() => extractSchemaDefs(schema), [schema]);
  const mergedRoot = useMemo(
    () => mergeSchemaNode(schema as JsonSchemaProperty, defs),
    [schema, defs],
  );
  const secretPaths = useMemo(
    () => computeSecretPaths(mergedRoot, defs, secrets),
    [mergedRoot, defs, secrets],
  );
  const secretKeys = useMemo(() => Array.from(secretPaths.keys()), [secretPaths]);
  const secretKeySet = useMemo(() => new Set(secretKeys), [secretKeys]);

  const [draft, setDraft] = useState<Record<string, unknown>>(() =>
    buildInitialValues(mergedRoot, defs, values, secretPaths, secretKeySet, hideNonSecrets),
  );
  const [secretActions, setSecretActions] = useState<Record<string, "keep" | "replace" | "clear">>({});
  const [secretValues, setSecretValues] = useState<Record<string, string>>({});
  const [secretSiteId, setSecretSiteId] = useState<string>(() => {
    const draftBound = !hideNonSecrets ? extractBoundSites(draft) : undefined;
    if (draftBound !== undefined) {
      const fromDraft = syncSecretSiteId("", draftBound);
      if (fromDraft) return fromDraft;
    }
    const valBound = extractBoundSites(values);
    return valBound !== undefined ? syncSecretSiteId("", valBound) : "";
  });
  const [saving, setSaving] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);

  const userClearedSecretSiteRef = useRef(false);
  const prevValuesBoundRef = useRef<string | undefined>(
    (() => {
      const b = extractBoundSites(values);
      return b !== undefined ? JSON.stringify(b) : undefined;
    })(),
  );
  const prevDraftBoundRef = useRef<string | undefined>(
    (() => {
      const b = !hideNonSecrets ? extractBoundSites(draft) : undefined;
      return b !== undefined ? JSON.stringify(b) : undefined;
    })(),
  );

  const schemaSig = useMemo(() => JSON.stringify({ mergedRoot, defs }), [mergedRoot, defs]);
  const prevInputsRef = useRef({
    schemaSig,
    values,
    secrets,
    hideNonSecrets,
  });

  useEffect(() => {
    const prev = prevInputsRef.current;
    if (
      prev.schemaSig === schemaSig &&
      prev.values === values &&
      prev.secrets === secrets &&
      prev.hideNonSecrets === hideNonSecrets
    ) {
      return;
    }
    const schemaChanged = prev.schemaSig !== schemaSig;
    prevInputsRef.current = { schemaSig, values, secrets, hideNonSecrets };

    const nextDraft = buildInitialValues(
      mergedRoot,
      defs,
      values,
      secretPaths,
      secretKeySet,
      hideNonSecrets,
    );
    setDraft(nextDraft);
    if (schemaChanged) {
      setSecretActions({});
      setSecretValues({});
    }

    const nextValBound = extractBoundSites(values);
    const nextDraftBound = !hideNonSecrets ? extractBoundSites(nextDraft) : undefined;
    const valKey = nextValBound !== undefined ? JSON.stringify(nextValBound) : undefined;
    const draftKey = nextDraftBound !== undefined ? JSON.stringify(nextDraftBound) : undefined;

    if (!hideNonSecrets && nextDraftBound !== undefined) {
      if (draftKey !== prevDraftBoundRef.current || valKey !== prevValuesBoundRef.current) {
        userClearedSecretSiteRef.current = false;
        setSecretSiteId((prevSite) => syncSecretSiteId(prevSite, nextDraftBound));
      } else if (nextDraftBound.length === 1 && !userClearedSecretSiteRef.current) {
        setSecretSiteId((prevSite) => prevSite || nextDraftBound[0]);
      }
    } else if (nextValBound !== undefined) {
      if (valKey !== prevValuesBoundRef.current) {
        userClearedSecretSiteRef.current = false;
        setSecretSiteId((prevSite) => syncSecretSiteId(prevSite, nextValBound));
      } else if (nextValBound.length === 1 && !userClearedSecretSiteRef.current) {
        setSecretSiteId((prevSite) => prevSite || nextValBound[0]);
      }
    } else if (
      prevValuesBoundRef.current !== undefined ||
      prevDraftBoundRef.current !== undefined
    ) {
      userClearedSecretSiteRef.current = false;
      setSecretSiteId("");
    }

    prevValuesBoundRef.current = valKey;
    prevDraftBoundRef.current = draftKey;
  }, [schemaSig, mergedRoot, defs, values, secrets, secretPaths, secretKeySet, hideNonSecrets]);

  const hasSecretMutation = useMemo(
    () =>
      secretKeys.some((k) => {
        const act = secretActions[k] ?? "keep";
        return act === "replace" || act === "clear";
      }),
    [secretKeys, secretActions],
  );

  const siteOptions = useMemo(() => {
    const map = new Map<string, SchemaOptionItem>();
    for (const s of sites) {
      if (s.id) map.set(s.id, s);
    }
    const extraCandidates: unknown[] = [
      values.site_id,
      draft.site_id,
      ...(Array.isArray(values.sites) ? values.sites : []),
      ...(Array.isArray(draft.sites) ? draft.sites : []),
    ];
    for (const candidate of extraCandidates) {
      if (typeof candidate === "string" && candidate.trim() && !map.has(candidate.trim())) {
        const id = candidate.trim();
        map.set(id, { id, name: id });
      }
    }
    return Array.from(map.values());
  }, [sites, values.site_id, values.sites, draft.site_id, draft.sites]);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setLocalError(null);
    const activeSecretKeySet = new Set<string>();
    for (const key of secretKeys) {
      const action = secretActions[key] ?? "keep";
      if (action === "replace" || (action === "keep" && Boolean(secrets[key]?.configured))) {
        activeSecretKeySet.add(key);
      }
    }
    const strippedDraft = stripSecretPaths(draft, secretPaths);
    const strippedInitial = stripSecretPaths(values, secretPaths);
    const normalized = validateAndNormalizeObject(
      mergedRoot,
      strippedDraft,
      strippedInitial,
      [],
      defs,
      secretKeySet,
      hideNonSecrets,
      0,
      false,
      activeSecretKeySet,
    );
    if (!normalized.ok) {
      setLocalError(normalized.error);
      return;
    }
    const outValues = stripSecretPaths(normalized.value, secretPaths);

    const outSecrets: Record<string, SecretWriteAction> = {};
    for (const key of secretKeys) {
      const action = secretActions[key] ?? "keep";
      const segs = secretPaths.get(key) ?? [key];
      const secretProp = resolveSchemaPropAtPath(mergedRoot, segs, defs);
      if (action === "replace") {
        const val = secretValues[key] ?? "";
        if (!val) {
          setLocalError(`请输入 "${key}" 的新密钥值或改为保持不变`);
          return;
        }
        if (new TextEncoder().encode(val).length > 65536) {
          setLocalError(`字段 "${key}" 密钥长度不能超过 65536 字节`);
          return;
        }
        const strErr = validateStringConstraints(secretProp, val, key);
        if (strErr) {
          setLocalError(strErr);
          return;
        }
        if (
          Array.isArray(secretProp.enum) &&
          !secretProp.enum.some((ev) => jsonValuesEqual(ev, val))
        ) {
          setLocalError(`字段 "${key}" 的值不在允许的枚举范围内`);
          return;
        }
        outSecrets[key] = { action: "replace", value: val };
      } else if (action === "clear") {
        outSecrets[key] = { action: "clear" };
      }
    }

    for (const key of secretKeys) {
      const action = secretActions[key] ?? "keep";
      const segs = secretPaths.get(key) ?? [key];
      if (
        !isSecretRequiredForWrite(
          mergedRoot,
          defs,
          segs,
          outValues,
          secretActions,
          secretPaths,
          secrets,
        )
      ) {
        continue;
      }
      if (action === "clear") {
        setLocalError(`字段 "${key}" 为必填密钥，不能清空`);
        return;
      }
      if (action === "keep" && !credentialBound && !secrets[key]?.configured) {
        setLocalError(`字段 "${key}" 为必填项`);
        return;
      }
    }

    let outSiteId: string | undefined;
    if (credentialBound) {
      if (Object.keys(outSecrets).length > 0) {
        const candidateSite = stripSitePrefix(secretSiteId);
        if (!candidateSite) {
          setLocalError("凭证绑定插件更新密钥时必须选择绑定站点 (site_id)");
          return;
        }
        outSiteId = candidateSite;
      } else if (!hideNonSecrets) {
        const outBound = extractBoundSites(outValues);
        if (outBound && outBound.length === 1) {
          const normalized = stripSitePrefix(outBound[0]);
          if (normalized) outSiteId = normalized;
        }
      }
    }

    setSaving(true);
    try {
      await onSave(outValues, outSecrets, outSiteId);
      setSecretActions({});
      setSecretValues({});
    } catch (err) {
      setLocalError((err as Error).message || "保存配置失败");
    } finally {
      setSaving(false);
    }
  }

  return (
    <form noValidate onSubmit={(e) => void handleSubmit(e)} className="space-y-4">
      {!hideNonSecrets && (
        <ObjectFieldsGroup
          objNode={mergedRoot}
          objDraft={draft}
          parentSegs={[]}
          defs={defs}
          secretKeySet={secretKeySet}
          editable={editable}
          saving={saving}
          sites={sites}
          downloaders={downloaders}
          onSetPath={(segs, val) => {
            setDraft((prev) =>
              setSchemaValueAtPath(prev, mergedRoot, defs, secretKeySet, segs, val),
            );
            if (segs.length === 1 && segs[0] === "site_id") {
              userClearedSecretSiteRef.current = false;
              const nextSite = typeof val === "string" ? val.trim() : "";
              prevDraftBoundRef.current = JSON.stringify(nextSite ? [nextSite] : []);
              setSecretSiteId(nextSite);
            } else if (segs.length === 1 && segs[0] === "sites") {
              userClearedSecretSiteRef.current = false;
              const list = Array.isArray(val)
                ? val
                    .filter((s): s is string => typeof s === "string" && s.trim().length > 0)
                    .map((s) => s.trim())
                : [];
              prevDraftBoundRef.current = JSON.stringify(list);
              setSecretSiteId((prev) => syncSecretSiteId(prev, list));
            }
          }}
        />
      )}

      {secretKeys.length > 0 && (
        <div className="space-y-3 rounded-xl border border-white/10 bg-white/[0.02] p-4">
          <h4 className="text-sub font-semibold text-[var(--text)]">密钥字段（仅宿主写入）</h4>
          {secretKeys.map((key) => {
            const status = secrets[key];
            const action = secretActions[key] ?? "keep";
            return (
              <div key={key} className="space-y-1.5">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="text-caption font-medium text-[var(--text)]">
                    {key} · {status?.configured ? "已配置" : "未配置"}
                  </span>
                  <select
                    aria-label={`${key} 操作`}
                    value={action}
                    disabled={!editable || saving}
                    onChange={(e) =>
                      setSecretActions((prev) => ({
                        ...prev,
                        [key]: e.target.value as "keep" | "replace" | "clear",
                      }))
                    }
                    className="rounded-lg border border-white/10 bg-white/[0.06] px-2.5 py-1 text-caption text-[var(--text)]"
                  >
                    <option value="keep">保持不变</option>
                    <option value="replace">替换密钥</option>
                    <option value="clear">清空密钥</option>
                  </select>
                </div>
                {action === "replace" && (
                  <input
                    aria-label={`${key} 新密钥`}
                    type="password"
                    value={secretValues[key] ?? ""}
                    disabled={!editable || saving}
                    onChange={(e) =>
                      setSecretValues((prev) => ({ ...prev, [key]: e.target.value }))
                    }
                    placeholder={`输入新的 ${key}`}
                    className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
                  />
                )}
              </div>
            );
          })}
          {credentialBound && hasSecretMutation && (
            <div className="space-y-1.5">
              <label
                htmlFor="plugin-cfg-secret-site-id"
                className="block text-caption font-medium text-[var(--text-muted)]"
              >
                凭证绑定站点 (site_id)
              </label>
              <select
                id="plugin-cfg-secret-site-id"
                aria-label="凭证绑定站点 (site_id)"
                value={secretSiteId}
                disabled={!editable || saving}
                onChange={(e) => {
                  const v = e.target.value;
                  userClearedSecretSiteRef.current = v === "";
                  setSecretSiteId(v);
                }}
                className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
              >
                <option value="">请选择站点</option>
                {siteOptions.map((opt) => (
                  <option key={opt.id} value={opt.id}>
                    {opt.name}
                  </option>
                ))}
              </select>
            </div>
          )}
        </div>
      )}

      {(localError || externalError) && (
        <div role="alert" className="rounded-lg border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-3 py-2 text-caption text-[#ff6b6b]">
          {localError || externalError}
        </div>
      )}

      <div className="flex justify-end">
        <button
          type="submit"
          disabled={!editable || saving}
          className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-50"
        >
          {saving ? "保存中…" : "保存配置"}
        </button>
      </div>
    </form>
  );
}
