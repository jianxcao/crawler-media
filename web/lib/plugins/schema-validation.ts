import {
  canonicalJson,
  isObjectSchemaNode,
  isPlainObject,
  type JsonSchemaProperty,
  jsonValuesEqual,
  MAX_SCHEMA_DEPTH,
  mergeSchemaNode,
} from "@/lib/plugins/schema-helpers";

export const ARRAY_PARSE_ERROR_SYM = Symbol("cm.arrayParseError");

export function isJsonArrayItemSchema(items: JsonSchemaProperty | undefined): boolean {
  if (!items) return false;
  return isObjectSchemaNode(items) || items.type === "object" || items.type === "array";
}

export function formatArrayInputText(
  val: unknown,
  items: JsonSchemaProperty | undefined,
): string {
  if (!Array.isArray(val) || val.length === 0) return "";
  if (isJsonArrayItemSchema(items)) {
    return JSON.stringify(val, null, 2);
  }
  return val.map((item) => String(item)).join("\n");
}

function makeInvalidArrayDraft(message: string): unknown[] {
  const arr: unknown[] = [];
  Object.defineProperty(arr, ARRAY_PARSE_ERROR_SYM, {
    value: message,
    enumerable: false,
  });
  return arr;
}

function parsePrimitiveArrayToken(tok: string, items: JsonSchemaProperty | undefined): unknown {
  const itemType = items?.type;
  if (itemType === "integer") {
    if (/^[+-]?\d+$/.test(tok)) {
      const parsed = Number(tok);
      if (Number.isSafeInteger(parsed)) return parsed;
    }
    return tok;
  }
  if (itemType === "number") {
    const parsed = Number(tok);
    return Number.isFinite(parsed) ? parsed : tok;
  }
  if (itemType === "boolean") {
    if (tok === "true") return true;
    if (tok === "false") return false;
    return tok;
  }
  if (Array.isArray(items?.enum)) {
    const matched = items.enum.find((v) => String(v) === tok);
    if (matched !== undefined) return matched;
  }
  return tok;
}

export function parseArrayInputText(
  rawText: string,
  items: JsonSchemaProperty | undefined,
  isRequired: boolean,
  hasArrayDefault: boolean,
  minItems?: number,
): unknown[] | undefined {
  const trimmed = rawText.trim();
  if (!trimmed) {
    if (!isRequired && (hasArrayDefault || (minItems ?? 0) > 0)) {
      return undefined;
    }
    return [];
  }
  if (isJsonArrayItemSchema(items)) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (!Array.isArray(parsed)) {
        return makeInvalidArrayDraft("必须是有效的 JSON 数组");
      }
      return parsed;
    } catch {
      return makeInvalidArrayDraft("必须是有效的 JSON 数组");
    }
  }
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (Array.isArray(parsed)) {
        return parsed.map((v) =>
          typeof v === "string" ? parsePrimitiveArrayToken(v, items) : v,
        );
      }
    } catch {
      // Fall back to newline/comma splitting below
    }
  }
  return rawText
    .split(/[\r\n,]+/)
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .map((tok) => parsePrimitiveArrayToken(tok, items));
}

const EMPTY_SECRET_SET: ReadonlySet<string> = new Set();

function hasActiveDescendantSecret(
  parentSegs: readonly string[],
  activeSecretKeySet: ReadonlySet<string>,
): boolean {
  if (activeSecretKeySet.size === 0 || parentSegs.length === 0) return false;
  const prefix = `${parentSegs.join(".")}.`;
  for (const sk of activeSecretKeySet) {
    if (sk.startsWith(prefix)) return true;
  }
  return false;
}

function isChildFieldEmpty(
  cv: unknown,
  cp: JsonSchemaProperty,
  initialChild: unknown,
  childSegs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  depth: number,
  activeSecretKeySet: ReadonlySet<string> = EMPTY_SECRET_SET,
): boolean {
  if (cv === undefined || cv === null) return true;
  if (cv === "") {
    return !(Array.isArray(cp.enum) && cp.enum.includes(""));
  }
  if (isObjectSchemaNode(cp)) {
    return (
      isPlainObject(cv) &&
      isEffectivelyEmptyOptionalObject(
        cv,
        cp,
        isPlainObject(initialChild) ? (initialChild as Record<string, unknown>) : undefined,
        childSegs,
        defs,
        secretKeySet,
        depth + 1,
        activeSecretKeySet,
      )
    );
  }
  if (
    cp.type === "array" &&
    Array.isArray(cv) &&
    cv.length === 0 &&
    !(cv as unknown as Record<symbol, unknown>)[ARRAY_PARSE_ERROR_SYM]
  ) {
    return true;
  }
  return false;
}

function isEffectivelyEmptyOptionalObject(
  obj: Record<string, unknown>,
  objSchema: JsonSchemaProperty,
  initialObj: Record<string, unknown> | undefined,
  parentSegs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  depth: number,
  activeSecretKeySet: ReadonlySet<string> = EMPTY_SECRET_SET,
): boolean {
  if (depth > MAX_SCHEMA_DEPTH) return true;
  if (hasActiveDescendantSecret(parentSegs, activeSecretKeySet)) {
    return false;
  }
  const props = isPlainObject(objSchema.properties)
    ? (objSchema.properties as Record<string, JsonSchemaProperty>)
    : {};
  for (const [extraKey, extraVal] of Object.entries(obj)) {
    if (!(extraKey in props) && extraVal !== undefined && extraVal !== null && extraVal !== "") {
      return false;
    }
  }

  const childReqSet = new Set(Array.isArray(objSchema.required) ? objSchema.required.map(String) : []);
  const nonBoolReqKeys: string[] = [];
  for (const [ck, rawChildProp] of Object.entries(props)) {
    if (!childReqSet.has(ck)) continue;
    const childSegs = [...parentSegs, ck];
    if (secretKeySet.has(childSegs.join("."))) continue;
    const cp = mergeSchemaNode(rawChildProp, defs, depth + 1);
    if (cp.type !== "boolean") {
      nonBoolReqKeys.push(ck);
    }
  }

  const hasEmptyRequiredNonBool = nonBoolReqKeys.some((rk) =>
    isChildFieldEmpty(
      obj[rk],
      mergeSchemaNode(props[rk], defs, depth + 1),
      undefined,
      [...parentSegs, rk],
      defs,
      secretKeySet,
      depth,
      activeSecretKeySet,
    ),
  );

  let hasClearedField = false;
  if (initialObj !== undefined) {
    if (nonBoolReqKeys.length > 0) {
      let clearedRequiredAnchor = false;
      let allRequiredCollapsedOrUnchangedDefaultEnum = true;
      for (const rk of nonBoolReqKeys) {
        const childSegs = [...parentSegs, rk];
        const cp = mergeSchemaNode(props[rk], defs, depth + 1);
        const cv = obj[rk];
        const initVal = initialObj[rk];
        const wasEmpty = isChildFieldEmpty(
          initVal,
          cp,
          undefined,
          childSegs,
          defs,
          secretKeySet,
          depth,
          activeSecretKeySet,
        );
        const nowEmpty = isChildFieldEmpty(
          cv,
          cp,
          initVal,
          childSegs,
          defs,
          secretKeySet,
          depth,
          activeSecretKeySet,
        );
        if (!wasEmpty && nowEmpty) {
          clearedRequiredAnchor = true;
        }
        const unchangedEnumOrDefault =
          jsonValuesEqual(cv, initVal) &&
          (Array.isArray(cp.enum) || (cp.default !== undefined && jsonValuesEqual(cv, cp.default)));
        if (!nowEmpty && !unchangedEnumOrDefault) {
          allRequiredCollapsedOrUnchangedDefaultEnum = false;
        }
      }
      hasClearedField = clearedRequiredAnchor && allRequiredCollapsedOrUnchangedDefaultEnum;
    } else {
      let clearedAnyValue = false;
      let allNonBoolEmpty = true;
      for (const [ck, rawChildProp] of Object.entries(props)) {
        const childSegs = [...parentSegs, ck];
        if (secretKeySet.has(childSegs.join("."))) continue;
        const cp = mergeSchemaNode(rawChildProp, defs, depth + 1);
        if (cp.type === "boolean") continue;
        const cv = obj[ck];
        const initVal = initialObj[ck];
        const wasEmpty = isChildFieldEmpty(
          initVal,
          cp,
          undefined,
          childSegs,
          defs,
          secretKeySet,
          depth,
          activeSecretKeySet,
        );
        const nowEmpty = isChildFieldEmpty(
          cv,
          cp,
          initVal,
          childSegs,
          defs,
          secretKeySet,
          depth,
          activeSecretKeySet,
        );
        if (!wasEmpty && nowEmpty) {
          clearedAnyValue = true;
        }
        if (!nowEmpty) {
          allNonBoolEmpty = false;
        }
      }
      hasClearedField = clearedAnyValue && allNonBoolEmpty;
    }
  }

  for (const [ck, rawChildProp] of Object.entries(props)) {
    const childSegs = [...parentSegs, ck];
    if (secretKeySet.has(childSegs.join("."))) continue;
    const cp = mergeSchemaNode(rawChildProp, defs, depth + 1);
    const cv = obj[ck];
    const initVal = initialObj?.[ck];
    if (
      isChildFieldEmpty(
        cv,
        cp,
        initVal,
        childSegs,
        defs,
        secretKeySet,
        depth,
        activeSecretKeySet,
      )
    ) {
      continue;
    }
    if (cp.type === "boolean") {
      if (initialObj === undefined) {
        if (
          (cv === false && cp.default !== true) ||
          (nonBoolReqKeys.length > 0 &&
            hasEmptyRequiredNonBool &&
            cp.default !== undefined &&
            jsonValuesEqual(cv, cp.default))
        ) {
          continue;
        }
        return false;
      }
      if (
        (cv === false && (cp.default !== true || hasClearedField)) ||
        (hasClearedField &&
          jsonValuesEqual(cv, initVal) &&
          (nonBoolReqKeys.length > 0 || cp.default === true))
      ) {
        continue;
      }
      return false;
    }
    if (initialObj === undefined) {
      if (
        nonBoolReqKeys.length > 0 &&
        hasEmptyRequiredNonBool &&
        cp.default !== undefined &&
        jsonValuesEqual(cv, cp.default)
      ) {
        continue;
      }
      return false;
    }
    if (
      hasClearedField &&
      jsonValuesEqual(cv, initVal) &&
      ((nonBoolReqKeys.length > 0 && Array.isArray(cp.enum)) ||
        (cp.default !== undefined && jsonValuesEqual(cv, cp.default)))
    ) {
      continue;
    }
    return false;
  }
  return true;
}

export function validateStringConstraints(
  prop: JsonSchemaProperty,
  val: string,
  fieldPath: string,
): string | null {
  const charLen = Array.from(val).length;
  if (prop.minLength !== undefined && charLen < prop.minLength) {
    return `字段 "${fieldPath}" 长度不能小于 ${prop.minLength}`;
  }
  if (prop.maxLength !== undefined && charLen > prop.maxLength) {
    return `字段 "${fieldPath}" 长度不能大于 ${prop.maxLength}`;
  }
  const patterns = prop.patterns ?? (prop.pattern !== undefined ? [prop.pattern] : []);
  for (const pat of patterns) {
    try {
      const re = new RegExp(pat);
      if (!re.test(val)) {
        return `字段 "${fieldPath}" 格式不匹配 "${pat}"`;
      }
    } catch {
      // Ignore unsupported JS RegExp syntax; backend validates authoritative regex
    }
  }
  return null;
}

function validateAndNormalizeItem(
  itemSchema: JsonSchemaProperty,
  rawItem: unknown,
  itemPath: string,
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  hideNonSecrets: boolean,
  depth: number,
): { ok: true; value: unknown } | { ok: false; error: string } {
  if (depth > MAX_SCHEMA_DEPTH) {
    return { ok: true, value: rawItem };
  }
  if (
    Array.isArray(itemSchema.enum) &&
    !itemSchema.enum.some((ev) => jsonValuesEqual(ev, rawItem))
  ) {
    return { ok: false, error: `字段 "${itemPath}" 的值不在允许的枚举范围内` };
  }
  if (isObjectSchemaNode(itemSchema)) {
    if (!isPlainObject(rawItem)) {
      return { ok: false, error: `字段 "${itemPath}" 必须是对象` };
    }
    return validateAndNormalizeObject(
      itemSchema,
      rawItem,
      undefined,
      [itemPath],
      defs,
      secretKeySet,
      hideNonSecrets,
      depth + 1,
      true,
    );
  }
  if (itemSchema.type === "array") {
    return validateAndNormalizeArrayValue(
      itemSchema,
      rawItem,
      itemPath,
      defs,
      secretKeySet,
      hideNonSecrets,
      depth + 1,
    );
  }
  if (itemSchema.type === "string") {
    if (typeof rawItem !== "string") {
      return { ok: false, error: `字段 "${itemPath}" 必须是字符串` };
    }
    const err = validateStringConstraints(itemSchema, rawItem, itemPath);
    if (err) return { ok: false, error: err };
    return { ok: true, value: rawItem };
  }
  if (itemSchema.type === "integer") {
    if (typeof rawItem !== "number" || !Number.isFinite(rawItem) || !Number.isInteger(rawItem)) {
      return { ok: false, error: `字段 "${itemPath}" 必须是有效整数` };
    }
    if (itemSchema.minimum !== undefined && rawItem < itemSchema.minimum) {
      return { ok: false, error: `字段 "${itemPath}" 不能小于 ${itemSchema.minimum}` };
    }
    if (itemSchema.maximum !== undefined && rawItem > itemSchema.maximum) {
      return { ok: false, error: `字段 "${itemPath}" 不能大于 ${itemSchema.maximum}` };
    }
    return { ok: true, value: rawItem };
  }
  if (itemSchema.type === "number") {
    if (typeof rawItem !== "number" || !Number.isFinite(rawItem)) {
      return { ok: false, error: `字段 "${itemPath}" 必须是有效数字` };
    }
    if (itemSchema.minimum !== undefined && rawItem < itemSchema.minimum) {
      return { ok: false, error: `字段 "${itemPath}" 不能小于 ${itemSchema.minimum}` };
    }
    if (itemSchema.maximum !== undefined && rawItem > itemSchema.maximum) {
      return { ok: false, error: `字段 "${itemPath}" 不能大于 ${itemSchema.maximum}` };
    }
    return { ok: true, value: rawItem };
  }
  if (itemSchema.type === "boolean") {
    if (typeof rawItem !== "boolean") {
      return { ok: false, error: `字段 "${itemPath}" 必须是布尔值 (true/false)` };
    }
    return { ok: true, value: rawItem };
  }
  return { ok: true, value: rawItem };
}

function validateAndNormalizeArrayValue(
  prop: JsonSchemaProperty,
  val: unknown,
  fieldPath: string,
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  hideNonSecrets: boolean,
  depth: number,
): { ok: true; value: unknown[] } | { ok: false; error: string } {
  const parseErr =
    typeof val === "object" && val !== null
      ? (val as Record<symbol, string | undefined>)[ARRAY_PARSE_ERROR_SYM]
      : undefined;
  if (parseErr) {
    return { ok: false, error: `字段 "${fieldPath}" ${parseErr}` };
  }
  if (!Array.isArray(val)) {
    return { ok: false, error: `字段 "${fieldPath}" 必须是有效数组` };
  }
  if (prop.minItems !== undefined && val.length < prop.minItems) {
    return { ok: false, error: `字段 "${fieldPath}" 至少需要 ${prop.minItems} 项` };
  }
  if (prop.maxItems !== undefined && val.length > prop.maxItems) {
    return { ok: false, error: `字段 "${fieldPath}" 最多允许 ${prop.maxItems} 项` };
  }
  const resolvedItems = prop.items ? mergeSchemaNode(prop.items, defs, depth + 1) : undefined;
  const normalizedItems: unknown[] = [];
  for (let i = 0; i < val.length; i += 1) {
    if (resolvedItems) {
      const itemRes = validateAndNormalizeItem(
        resolvedItems,
        val[i],
        `${fieldPath}[${i}]`,
        defs,
        secretKeySet,
        hideNonSecrets,
        depth + 1,
      );
      if (!itemRes.ok) return itemRes;
      normalizedItems.push(itemRes.value);
    } else {
      normalizedItems.push(val[i]);
    }
  }
  if (prop.uniqueItems === true) {
    const seen = new Set<string>();
    for (const item of normalizedItems) {
      const key = canonicalJson(item);
      if (seen.has(key)) {
        return { ok: false, error: `字段 "${fieldPath}" 中的元素不能重复` };
      }
      seen.add(key);
    }
  }
  if (Array.isArray(prop.enum) && !prop.enum.some((ev) => jsonValuesEqual(ev, normalizedItems))) {
    return { ok: false, error: `字段 "${fieldPath}" 的值不在允许的枚举范围内` };
  }
  return { ok: true, value: normalizedItems };
}

export function validateAndNormalizeObject(
  objNode: JsonSchemaProperty,
  draftObj: Record<string, unknown>,
  initialObj: Record<string, unknown> | undefined,
  parentSegs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  hideNonSecrets: boolean,
  depth = 0,
  fromJsonLiteral = false,
  activeSecretKeySet: ReadonlySet<string> = EMPTY_SECRET_SET,
): { ok: true; value: Record<string, unknown> } | { ok: false; error: string } {
  if (depth > MAX_SCHEMA_DEPTH) {
    return { ok: true, value: {} };
  }
  const out: Record<string, unknown> = { ...draftObj };
  const props = isPlainObject(objNode.properties)
    ? (objNode.properties as Record<string, JsonSchemaProperty>)
    : {};
  if (
    !hideNonSecrets &&
    objNode.additionalProperties !== true &&
    !isPlainObject(objNode.additionalProperties)
  ) {
    for (const extraKey of Object.keys(draftObj)) {
      const extraPath = [...parentSegs, extraKey].join(".");
      if (!(extraKey in props) && !secretKeySet.has(extraPath)) {
        return { ok: false, error: `字段 "${extraPath}" 包含未声明的未知属性` };
      }
    }
  }
  const reqSet = new Set(Array.isArray(objNode.required) ? objNode.required.map(String) : []);

  for (const [key, rawProp] of Object.entries(props)) {
    const childSegs = [...parentSegs, key];
    const fieldPath = childSegs.join(".");
    if (secretKeySet.has(fieldPath)) {
      delete out[key];
      continue;
    }
    const prop = mergeSchemaNode(rawProp, defs, depth + 1);
    const isReq = reqSet.has(key);
    const val = out[key];

    if (isObjectSchemaNode(prop)) {
      const initChild = isPlainObject(initialObj?.[key])
        ? (initialObj[key] as Record<string, unknown>)
        : undefined;
      if (
        !isReq &&
        !hasActiveDescendantSecret(childSegs, activeSecretKeySet) &&
        (val === undefined || (!fromJsonLiteral && !isPlainObject(val)))
      ) {
        delete out[key];
        continue;
      }
      const canSynthesizeForSecret =
        !fromJsonLiteral &&
        val === undefined &&
        hasActiveDescendantSecret(childSegs, activeSecretKeySet);
      if (!hideNonSecrets && !isPlainObject(val) && !canSynthesizeForSecret) {
        return {
          ok: false,
          error: val === undefined ? `字段 "${fieldPath}" 为必填项` : `字段 "${fieldPath}" 必须是对象`,
        };
      }
      const childDraft = isPlainObject(val) ? (val as Record<string, unknown>) : {};
      if (
        !fromJsonLiteral &&
        !isReq &&
        !isPlainObject(prop.default) &&
        isEffectivelyEmptyOptionalObject(
          childDraft,
          prop,
          initChild,
          childSegs,
          defs,
          secretKeySet,
          depth + 1,
          activeSecretKeySet,
        )
      ) {
        delete out[key];
        continue;
      }
      const sub = validateAndNormalizeObject(
        prop,
        childDraft,
        initChild,
        childSegs,
        defs,
        secretKeySet,
        hideNonSecrets,
        depth + 1,
        fromJsonLiteral,
        activeSecretKeySet,
      );
      if (!sub.ok) return sub;
      if (
        !fromJsonLiteral &&
        !isReq &&
        !isPlainObject(prop.default) &&
        initChild === undefined &&
        Object.keys(sub.value).length === 0
      ) {
        delete out[key];
      } else {
        out[key] = sub.value;
      }
      continue;
    }

    if (prop.type === "array") {
      if (!isReq && (val === undefined || (!fromJsonLiteral && val === null))) {
        delete out[key];
        continue;
      }
      if (!hideNonSecrets && isReq && (val === undefined || val === null)) {
        return { ok: false, error: `字段 "${fieldPath}" 为必填项` };
      }
      if (!hideNonSecrets && fromJsonLiteral && val === null) {
        return { ok: false, error: `字段 "${fieldPath}" 必须是数组` };
      }
      if (!hideNonSecrets && val !== undefined && val !== null) {
        const arrRes = validateAndNormalizeArrayValue(
          prop,
          val,
          fieldPath,
          defs,
          secretKeySet,
          hideNonSecrets,
          depth + 1,
        );
        if (!arrRes.ok) return arrRes;
        out[key] = arrRes.value;
      }
      continue;
    }

    const isHandleSelect =
      prop["x-ui"]?.widget === "site" ||
      prop["x-ui"]?.widget === "downloader" ||
      key === "site_id" ||
      key === "downloader_id";
    const emptyStringAllowed =
      val === "" &&
      ((Array.isArray(prop.enum) && prop.enum.includes("")) ||
        (!Array.isArray(prop.enum) &&
          !isHandleSelect &&
          prop.type === "string" &&
          (prop.minLength ?? 0) === 0 &&
          (fromJsonLiteral || prop.default === "" || initialObj?.[key] === "")));
    if (
      !hideNonSecrets &&
      isReq &&
      (val === undefined || val === null || (val === "" && !emptyStringAllowed))
    ) {
      return { ok: false, error: `字段 "${fieldPath}" 为必填项` };
    }
    if (!isReq && val === undefined) {
      delete out[key];
      continue;
    }
    if (
      !fromJsonLiteral &&
      (prop.type === "integer" || prop.type === "number") &&
      !isReq &&
      (val === "" || val === null)
    ) {
      delete out[key];
      continue;
    }
    if (
      !fromJsonLiteral &&
      !isReq &&
      isHandleSelect &&
      (val === "" || val === null)
    ) {
      delete out[key];
      continue;
    }
    if (
      !fromJsonLiteral &&
      Array.isArray(prop.enum) &&
      !isReq &&
      (val === "" || val === null) &&
      !prop.enum.includes("")
    ) {
      delete out[key];
      continue;
    }
    if (!fromJsonLiteral && prop.type === "boolean" && !isReq && val === null) {
      delete out[key];
      continue;
    }
    if (!fromJsonLiteral && prop.type === "string" && !isReq && val === null) {
      delete out[key];
      continue;
    }
    if (
      !fromJsonLiteral &&
      prop.type === "string" &&
      !isReq &&
      val === "" &&
      ((prop.minLength ?? 0) > 0 ||
        prop.pattern !== undefined ||
        (prop.patterns?.length ?? 0) > 0 ||
        prop.default !== undefined)
    ) {
      delete out[key];
      continue;
    }
    if (!hideNonSecrets && Array.isArray(prop.enum) && val !== undefined) {
      if (!prop.enum.some((ev) => jsonValuesEqual(ev, val))) {
        return { ok: false, error: `字段 "${fieldPath}" 的值不在允许的枚举范围内` };
      }
    }
    if (!hideNonSecrets && prop.type === "boolean" && val !== undefined) {
      if (typeof val !== "boolean") {
        return { ok: false, error: `字段 "${fieldPath}" 必须是布尔值 (true/false)` };
      }
    }
    if (!hideNonSecrets && prop.type === "string" && val !== undefined) {
      if (typeof val !== "string") {
        return { ok: false, error: `字段 "${fieldPath}" 必须是字符串` };
      }
      const strErr = validateStringConstraints(prop, val, fieldPath);
      if (strErr) return { ok: false, error: strErr };
    }
    if (
      !hideNonSecrets &&
      (prop.type === "integer" || prop.type === "number") &&
      val !== undefined &&
      (val !== "" || fromJsonLiteral)
    ) {
      if (typeof val !== "number" || !Number.isFinite(val)) {
        return { ok: false, error: `字段 "${fieldPath}" 必须是有效数字` };
      }
      if (prop.type === "integer" && !Number.isInteger(val)) {
        return { ok: false, error: `字段 "${fieldPath}" 必须是整数` };
      }
      if (prop.minimum !== undefined && val < prop.minimum) {
        return { ok: false, error: `字段 "${fieldPath}" 不能小于 ${prop.minimum}` };
      }
      if (prop.maximum !== undefined && val > prop.maximum) {
        return { ok: false, error: `字段 "${fieldPath}" 不能大于 ${prop.maximum}` };
      }
      out[key] = val;
    }
  }
  return { ok: true, value: out };
}
