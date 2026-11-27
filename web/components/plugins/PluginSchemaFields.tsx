"use client";

import { useEffect, useRef, useState } from "react";

import {
  formatArrayInputText,
  hasVisibleNonSecretLeaf,
  isJsonArrayItemSchema,
  isObjectSchemaNode,
  isPlainObject,
  type JsonSchemaProperty,
  MAX_SCHEMA_DEPTH,
  mergeSchemaNode,
  parseArrayInputText,
} from "@/lib/plugins/schema-helpers";

export interface SchemaOptionItem {
  id: string;
  name: string;
}

const GIB = 1024 ** 3;

interface ArrayFieldControlProps {
  inputId: string;
  label: string;
  prop: JsonSchemaProperty;
  rawVal: unknown;
  isReq: boolean;
  editable: boolean;
  saving: boolean;
  onChangeValue: (nextVal: unknown) => void;
}

function ArrayFieldControl({
  inputId,
  label,
  prop,
  rawVal,
  isReq,
  editable,
  saving,
  onChangeValue,
}: ArrayFieldControlProps) {
  const [rawText, setRawText] = useState<string>(() => formatArrayInputText(rawVal, prop.items));
  const lastEmittedRef = useRef<unknown>(rawVal);

  useEffect(() => {
    if (rawVal !== lastEmittedRef.current) {
      lastEmittedRef.current = rawVal;
      setRawText(formatArrayInputText(rawVal, prop.items));
    }
  }, [rawVal, prop.items]);

  const jsonMode = isJsonArrayItemSchema(prop.items);

  return (
    <div>
      <label
        htmlFor={inputId}
        className="mb-1 block text-caption font-medium text-[var(--text-muted)]"
      >
        {label}
      </label>
      <textarea
        id={inputId}
        rows={3}
        value={rawText}
        disabled={!editable || saving}
        placeholder={jsonMode ? '[{"key": "value"}]' : "每行或逗号分隔一项"}
        onChange={(e) => {
          const nextText = e.target.value;
          setRawText(nextText);
          const parsed = parseArrayInputText(
            nextText,
            prop.items,
            isReq,
            Array.isArray(prop.default),
            prop.minItems,
          );
          lastEmittedRef.current = parsed;
          onChangeValue(parsed);
        }}
        className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
      />
      {prop.description && (
        <p className="mt-1 text-caption text-[var(--text-faint)]">{prop.description}</p>
      )}
    </div>
  );
}

export interface ObjectFieldsGroupProps {
  objNode: JsonSchemaProperty;
  objDraft: Record<string, unknown>;
  parentSegs: readonly string[];
  defs: Record<string, JsonSchemaProperty>;
  secretKeySet: ReadonlySet<string>;
  editable: boolean;
  saving: boolean;
  sites: SchemaOptionItem[];
  downloaders: SchemaOptionItem[];
  onSetPath: (segs: readonly string[], value: unknown) => void;
  depth?: number;
  inOptionalObject?: boolean;
}

export function ObjectFieldsGroup({
  objNode,
  objDraft,
  parentSegs,
  defs,
  secretKeySet,
  editable,
  saving,
  sites,
  downloaders,
  onSetPath,
  depth = 0,
  inOptionalObject = false,
}: ObjectFieldsGroupProps) {
  if (depth > MAX_SCHEMA_DEPTH) return null;
  const props = isPlainObject(objNode.properties)
    ? (objNode.properties as Record<string, JsonSchemaProperty>)
    : {};
  const reqSet = new Set(Array.isArray(objNode.required) ? objNode.required.map(String) : []);
  const entries = Object.entries(props).filter(
    ([key]) => !secretKeySet.has([...parentSegs, key].join(".")),
  );

  return (
    <>
      {entries.map(([key, rawProp]) => {
        const prop = mergeSchemaNode(rawProp, defs, depth + 1);
        const segs = [...parentSegs, key];
        const dottedKey = segs.join(".");
        const widget = prop["x-ui"]?.widget;
        const label = prop.title ?? key;
        const rawVal = objDraft[key];
        const inputId = `plugin-cfg-${dottedKey}`;
        const isReq = reqSet.has(key);

        if (isObjectSchemaNode(prop)) {
          if (!hasVisibleNonSecretLeaf(prop, segs, defs, secretKeySet, depth + 1)) {
            return null;
          }
          const childDraft = isPlainObject(rawVal) ? (rawVal as Record<string, unknown>) : {};
          return (
            <fieldset
              key={dottedKey}
              className="space-y-3 rounded-xl border border-white/10 bg-white/[0.02] p-3.5"
            >
              <legend className="px-1 text-sub font-semibold text-[var(--text)]">{label}</legend>
              {prop.description && (
                <p className="text-caption text-[var(--text-faint)]">{prop.description}</p>
              )}
              <ObjectFieldsGroup
                objNode={prop}
                objDraft={childDraft}
                parentSegs={segs}
                defs={defs}
                secretKeySet={secretKeySet}
                editable={editable}
                saving={saving}
                sites={sites}
                downloaders={downloaders}
                onSetPath={onSetPath}
                depth={depth + 1}
                inOptionalObject={inOptionalObject || !isReq}
              />
            </fieldset>
          );
        }

        if (prop.type === "boolean") {
          return (
            <label
              key={dottedKey}
              htmlFor={inputId}
              className="flex items-center gap-3 rounded-lg border border-white/10 bg-white/[0.03] px-3 py-2.5"
            >
              <input
                id={inputId}
                type="checkbox"
                checked={Boolean(rawVal)}
                disabled={!editable || saving}
                onChange={(e) => onSetPath(segs, e.target.checked)}
              />
              <span className="text-sub font-medium text-[var(--text)]">{label}</span>
            </label>
          );
        }

        if (prop.type === "array" && prop.items?.["x-ui"]?.widget === "site") {
          const selected = new Set(Array.isArray(rawVal) ? rawVal.map(String) : []);
          const mergedSites: SchemaOptionItem[] = [...sites];
          for (const sid of selected) {
            const trimmed = sid.trim();
            if (trimmed && !mergedSites.some((s) => s.id === trimmed)) {
              mergedSites.push({ id: trimmed, name: `${trimmed} (已失效)` });
            }
          }
          return (
            <div key={dottedKey} className="space-y-1.5">
              <p className="text-caption font-medium text-[var(--text-muted)]">{label}</p>
              <div className="flex flex-wrap gap-2">
                {mergedSites.length === 0 ? (
                  <span className="text-caption text-[var(--text-faint)]">暂无可用站点</span>
                ) : (
                  mergedSites.map((site) => {
                    const checked = selected.has(site.id);
                    return (
                      <label
                        key={site.id}
                        className="flex items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.04] px-2.5 py-1.5 text-caption"
                      >
                        <input
                          type="checkbox"
                          checked={checked}
                          disabled={!editable || saving}
                          onChange={(e) => {
                            const next = new Set(selected);
                            if (e.target.checked) next.add(site.id);
                            else next.delete(site.id);
                            onSetPath(segs, Array.from(next));
                          }}
                        />
                        <span>{site.name}</span>
                      </label>
                    );
                  })
                )}
              </div>
            </div>
          );
        }

        if (prop.type === "array") {
          return (
            <ArrayFieldControl
              key={dottedKey}
              inputId={inputId}
              label={label}
              prop={prop}
              rawVal={rawVal}
              isReq={isReq}
              editable={editable}
              saving={saving}
              onChangeValue={(nextVal) => onSetPath(segs, nextVal)}
            />
          );
        }

        if (widget === "site" || widget === "downloader") {
          const opts = widget === "site" ? sites : downloaders;
          const rawStr = String(rawVal ?? "");
          const hasStale = rawStr.trim().length > 0 && !opts.some((opt) => opt.id === rawStr);
          return (
            <div key={dottedKey}>
              <label
                htmlFor={inputId}
                className="mb-1 block text-caption font-medium text-[var(--text-muted)]"
              >
                {label}
              </label>
              <select
                id={inputId}
                value={rawStr}
                disabled={!editable || saving}
                onChange={(e) => onSetPath(segs, e.target.value)}
                className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
              >
                <option value="">自动 / 默认</option>
                {hasStale && <option value={rawStr}>{`${rawStr} (已失效)`}</option>}
                {opts.map((opt) => (
                  <option key={opt.id} value={opt.id}>
                    {opt.name}
                  </option>
                ))}
              </select>
            </div>
          );
        }

        if (Array.isArray(prop.enum)) {
          const hasEmptyEnum = prop.enum.some((v) => String(v) === "");
          let unsetSentinel = "__cm_unset_enum__";
          while (prop.enum.some((v) => String(v) === unsetSentinel)) {
            unsetSentinel = `_${unsetSentinel}`;
          }
          const canBeBlank = !isReq || inOptionalObject;
          const blankOptionValue = hasEmptyEnum ? unsetSentinel : "";
          const isUnset =
            rawVal === undefined || rawVal === null || (!hasEmptyEnum && rawVal === "");
          const selectValue = isUnset ? blankOptionValue : String(rawVal);
          return (
            <div key={dottedKey}>
              <label
                htmlFor={inputId}
                className="mb-1 block text-caption font-medium text-[var(--text-muted)]"
              >
                {label}
              </label>
              <select
                id={inputId}
                value={selectValue}
                disabled={!editable || saving}
                onChange={(e) => {
                  if (e.target.value === unsetSentinel) {
                    onSetPath(segs, undefined);
                    return;
                  }
                  const matched = prop.enum?.find((v) => String(v) === e.target.value);
                  onSetPath(segs, matched !== undefined ? matched : e.target.value);
                }}
                className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
              >
                {canBeBlank && <option value={blankOptionValue}>自动 / 默认</option>}
                {!canBeBlank && isUnset && (
                  <option value={blankOptionValue} disabled>
                    请选择…
                  </option>
                )}
                {prop.enum.map((v) => (
                  <option key={String(v)} value={String(v)}>
                    {String(v)}
                  </option>
                ))}
              </select>
            </div>
          );
        }

        const isNumber = prop.type === "integer" || prop.type === "number";
        const bytesGibHint =
          widget === "bytes" && Number.isFinite(Number(rawVal)) && Number(rawVal) > 0
            ? `≈ ${(Number(rawVal) / GIB).toFixed(2)} GiB`
            : null;

        return (
          <div key={dottedKey}>
            <label
              htmlFor={inputId}
              className="mb-1 block text-caption font-medium text-[var(--text-muted)]"
            >
              {label}
            </label>
            <input
              id={inputId}
              type={isNumber ? "number" : "text"}
              step={isNumber ? (prop.type === "integer" ? 1 : "any") : undefined}
              min={prop.minimum}
              max={prop.maximum}
              value={rawVal === undefined || rawVal === null ? "" : String(rawVal)}
              disabled={!editable || saving}
              onChange={(e) =>
                onSetPath(
                  segs,
                  isNumber ? (e.target.value === "" ? "" : Number(e.target.value)) : e.target.value,
                )
              }
              className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
            />
            {bytesGibHint && (
              <p className="mt-1 text-caption text-[var(--text-faint)]">{bytesGibHint}</p>
            )}
            {prop.description && (
              <p className="mt-1 text-caption text-[var(--text-faint)]">{prop.description}</p>
            )}
          </div>
        );
      })}
    </>
  );
}
