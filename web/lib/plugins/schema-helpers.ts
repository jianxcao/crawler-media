import type { SecretConfiguredStatus } from "@/lib/api/plugins";
export {
  ARRAY_PARSE_ERROR_SYM,
  formatArrayInputText,
  isJsonArrayItemSchema,
  parseArrayInputText,
  validateAndNormalizeObject,
  validateStringConstraints,
} from "@/lib/plugins/schema-validation";

export interface JsonSchemaProperty {
  $ref?: string;
  type?: string;
  title?: string;
  description?: string;
  default?: unknown;
  minimum?: number;
  maximum?: number;
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  patterns?: string[];
  minItems?: number;
  maxItems?: number;
  uniqueItems?: boolean;
  enum?: unknown[];
  format?: string;
  writeOnly?: boolean;
  required?: unknown[];
  additionalProperties?: boolean | JsonSchemaProperty;
  properties?: Record<string, JsonSchemaProperty>;
  items?: JsonSchemaProperty;
  "x-ui"?: { widget?: string; secret?: boolean };
}

export const MAX_SCHEMA_DEPTH = 16;

export function isPlainObject(val: unknown): val is Record<string, unknown> {
  return Boolean(val && typeof val === "object" && !Array.isArray(val));
}

export function canonicalJson(val: unknown): string {
  if (val === null || typeof val !== "object") {
    return JSON.stringify(val);
  }
  if (Array.isArray(val)) {
    return `[${val.map(canonicalJson).join(",")}]`;
  }
  const obj = val as Record<string, unknown>;
  const keys = Object.keys(obj).sort();
  return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(obj[k])}`).join(",")}}`;
}

export function jsonValuesEqual(a: unknown, b: unknown): boolean {
  return Object.is(a, b) || canonicalJson(a) === canonicalJson(b);
}

export function extractSchemaDefs(
  schema: Record<string, unknown>,
): Record<string, JsonSchemaProperty> {
  const raw = schema.$defs;
  if (!isPlainObject(raw)) {
    return {};
  }
  return raw as Record<string, JsonSchemaProperty>;
}

export function hasOwnSecretFlag(prop: JsonSchemaProperty): boolean {
  return (
    prop.writeOnly === true ||
    prop.format === "password" ||
    prop["x-ui"]?.widget === "password" ||
    prop["x-ui"]?.widget === "secret" ||
    prop["x-ui"]?.secret === true
  );
}

export function mergeSchemaNode(
  rawNode: JsonSchemaProperty | undefined,
  defs: Record<string, JsonSchemaProperty>,
  depth = 0,
): JsonSchemaProperty {
  if (!rawNode || typeof rawNode !== "object" || depth > MAX_SCHEMA_DEPTH) {
    return {};
  }
  const chain: JsonSchemaProperty[] = [];
  const visited = new Set<string>();
  let cur: JsonSchemaProperty | undefined = rawNode;
  while (cur && typeof cur === "object" && chain.length <= MAX_SCHEMA_DEPTH) {
    chain.push(cur);
    if (typeof cur.$ref === "string" && cur.$ref.startsWith("#/$defs/")) {
      const defName = cur.$ref.slice("#/$defs/".length);
      if (!defName || visited.has(defName)) {
        break;
      }
      visited.add(defName);
      cur = defs[defName];
      continue;
    }
    break;
  }

  const merged: JsonSchemaProperty = {};
  for (let i = chain.length - 1; i >= 0; i -= 1) {
    const node = chain[i];
    if (node.type !== undefined) merged.type = node.type;
    if (node.title !== undefined) merged.title = node.title;
    if (node.description !== undefined) merged.description = node.description;
    if (node.default !== undefined) merged.default = node.default;
    if (node.minimum !== undefined) {
      merged.minimum =
        merged.minimum !== undefined ? Math.max(merged.minimum, node.minimum) : node.minimum;
    }
    if (node.maximum !== undefined) {
      merged.maximum =
        merged.maximum !== undefined ? Math.min(merged.maximum, node.maximum) : node.maximum;
    }
    if (node.minLength !== undefined) {
      merged.minLength =
        merged.minLength !== undefined ? Math.max(merged.minLength, node.minLength) : node.minLength;
    }
    if (node.maxLength !== undefined) {
      merged.maxLength =
        merged.maxLength !== undefined ? Math.min(merged.maxLength, node.maxLength) : node.maxLength;
    }
    if (node.minItems !== undefined) {
      merged.minItems =
        merged.minItems !== undefined ? Math.max(merged.minItems, node.minItems) : node.minItems;
    }
    if (node.maxItems !== undefined) {
      merged.maxItems =
        merged.maxItems !== undefined ? Math.min(merged.maxItems, node.maxItems) : node.maxItems;
    }
    if (node.uniqueItems !== undefined) {
      merged.uniqueItems = Boolean(merged.uniqueItems || node.uniqueItems);
    }
    const incomingPatterns = [
      ...(Array.isArray(node.patterns) ? node.patterns : []),
      ...(node.pattern !== undefined ? [node.pattern] : []),
    ];
    if (incomingPatterns.length > 0) {
      if (node.pattern !== undefined) {
        merged.pattern = node.pattern;
      } else if (merged.pattern === undefined) {
        merged.pattern = incomingPatterns[incomingPatterns.length - 1];
      }
      const nextPatterns = [...(merged.patterns ?? [])];
      for (const pat of incomingPatterns) {
        if (!nextPatterns.includes(pat)) {
          nextPatterns.push(pat);
        }
      }
      merged.patterns = nextPatterns;
    }
    if (Array.isArray(node.enum)) {
      if (Array.isArray(merged.enum)) {
        const nextEnum = node.enum;
        merged.enum = merged.enum.filter((v) => nextEnum.some((nv) => jsonValuesEqual(v, nv)));
      } else {
        merged.enum = [...node.enum];
      }
    }
    if (node.format !== undefined) merged.format = node.format;
    if (node.writeOnly !== undefined) {
      merged.writeOnly = Boolean(merged.writeOnly || node.writeOnly);
    }
    if (node.additionalProperties !== undefined) {
      merged.additionalProperties =
        merged.additionalProperties === false || node.additionalProperties === false
          ? false
          : node.additionalProperties;
    }
    if (node["x-ui"] && typeof node["x-ui"] === "object") {
      merged["x-ui"] = { ...merged["x-ui"], ...node["x-ui"] };
    }
    if (hasOwnSecretFlag(node)) {
      merged["x-ui"] = { ...merged["x-ui"], secret: true };
    }
    if (node.items && typeof node.items === "object") {
      merged.items = mergeSchemaNode(node.items, defs, depth + 1);
    }
    if (isPlainObject(node.properties)) {
      merged.properties = {
        ...merged.properties,
        ...(node.properties as Record<string, JsonSchemaProperty>),
      };
    }
    if (Array.isArray(node.required)) {
      merged.required = Array.from(
        new Set([...(merged.required ?? []).map(String), ...node.required.map(String)]),
      );
    }
  }
  return merged;
}

export function isObjectSchemaNode(node: JsonSchemaProperty): boolean {
  return (
    !hasOwnSecretFlag(node) &&
    (node.type === "object" || (node.type === undefined && isPlainObject(node.properties)))
  );
}

export function hasVisibleNonSecretLeaf(
  objNode: JsonSchemaProperty,
  parentSegs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  depth = 0,
): boolean {
  if (depth > MAX_SCHEMA_DEPTH) return false;
  const props = isPlainObject(objNode.properties)
    ? (objNode.properties as Record<string, JsonSchemaProperty>)
    : {};
  for (const [key, rawProp] of Object.entries(props)) {
    if (!rawProp || typeof rawProp !== "object") continue;
    const segs = [...parentSegs, key];
    if (secretKeySet.has(segs.join("."))) continue;
    const merged = mergeSchemaNode(rawProp, defs, depth + 1);
    if (hasOwnSecretFlag(merged)) continue;
    if (isObjectSchemaNode(merged)) {
      if (hasVisibleNonSecretLeaf(merged, segs, defs, secretKeySet, depth + 1)) {
        return true;
      }
      continue;
    }
    return true;
  }
  return false;
}

function collectSecretPathsFromNode(
  node: JsonSchemaProperty,
  defs: Record<string, JsonSchemaProperty>,
  parentSegs: readonly string[],
  depth: number,
  out: Map<string, string[]>,
): void {
  if (depth > MAX_SCHEMA_DEPTH) return;
  const props = isPlainObject(node.properties)
    ? (node.properties as Record<string, JsonSchemaProperty>)
    : {};
  for (const [key, rawProp] of Object.entries(props)) {
    if (!rawProp || typeof rawProp !== "object") continue;
    const merged = mergeSchemaNode(rawProp, defs, depth + 1);
    const segs = [...parentSegs, key];
    const dotted = segs.join(".");
    if (hasOwnSecretFlag(merged)) {
      out.set(dotted, segs);
    } else if (isObjectSchemaNode(merged)) {
      collectSecretPathsFromNode(merged, defs, segs, depth + 1, out);
    }
  }
}

export function schemaHasSecrets(
  schema: Record<string, unknown>,
  secrets: Record<string, SecretConfiguredStatus> = {},
): boolean {
  if (Object.keys(secrets).length > 0) return true;
  const defs = extractSchemaDefs(schema);
  const mergedRoot = mergeSchemaNode(schema as JsonSchemaProperty, defs);
  const found = new Map<string, string[]>();
  collectSecretPathsFromNode(mergedRoot, defs, [], 0, found);
  return found.size > 0;
}

export function computeSecretPaths(
  mergedRoot: JsonSchemaProperty,
  defs: Record<string, JsonSchemaProperty>,
  secrets: Record<string, SecretConfiguredStatus>,
): Map<string, string[]> {
  const map = new Map<string, string[]>();
  collectSecretPathsFromNode(mergedRoot, defs, [], 0, map);
  for (const key of Object.keys(secrets)) {
    if (!map.has(key)) {
      map.set(key, key.split("."));
    }
  }
  return map;
}

function deleteAtPath(
  cur: Record<string, unknown>,
  segs: readonly string[],
): Record<string, unknown> {
  if (segs.length === 0) return cur;
  const [head, ...rest] = segs;
  if (!(head in cur)) return cur;
  if (rest.length === 0) {
    const copy = { ...cur };
    delete copy[head];
    return copy;
  }
  if (!isPlainObject(cur[head])) return cur;
  return {
    ...cur,
    [head]: deleteAtPath(cur[head] as Record<string, unknown>, rest),
  };
}

export function stripSecretPaths(
  raw: Record<string, unknown>,
  secretPaths: ReadonlyMap<string, string[]>,
): Record<string, unknown> {
  let next: Record<string, unknown> = { ...raw };
  for (const [literalKey, segs] of secretPaths.entries()) {
    if (literalKey in next) {
      const copy = { ...next };
      delete copy[literalKey];
      next = copy;
    }
    next = deleteAtPath(next, segs);
  }
  return next;
}

export function setValueAtPath(
  root: Record<string, unknown>,
  segs: readonly string[],
  value: unknown,
): Record<string, unknown> {
  if (segs.length === 0) return root;
  const [head, ...rest] = segs;
  if (rest.length === 0) {
    return { ...root, [head]: value };
  }
  const child = isPlainObject(root[head]) ? (root[head] as Record<string, unknown>) : {};
  return {
    ...root,
    [head]: setValueAtPath(child, rest, value),
  };
}

export function resolveSchemaPropAtPath(
  mergedRoot: JsonSchemaProperty,
  segs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
): JsonSchemaProperty {
  let cur: JsonSchemaProperty = mergedRoot;
  for (let i = 0; i < segs.length && i <= MAX_SCHEMA_DEPTH; i += 1) {
    const props = isPlainObject(cur.properties)
      ? (cur.properties as Record<string, JsonSchemaProperty>)
      : undefined;
    const rawChild = props?.[segs[i]];
    if (!rawChild) return {};
    cur = mergeSchemaNode(rawChild, defs, i + 1);
  }
  return cur;
}

function buildInitialObjectValues(
  objNode: JsonSchemaProperty,
  rawValues: Record<string, unknown>,
  parentSegs: readonly string[],
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  depth = 0,
  hideNonSecrets = false,
): Record<string, unknown> {
  if (depth > MAX_SCHEMA_DEPTH) return {};
  const props = isPlainObject(objNode.properties)
    ? (objNode.properties as Record<string, JsonSchemaProperty>)
    : {};
  const next: Record<string, unknown> = {};
  if (
    !hideNonSecrets &&
    objNode.additionalProperties !== true &&
    !isPlainObject(objNode.additionalProperties)
  ) {
    for (const k of Object.keys(props)) {
      if (k in rawValues) {
        next[k] = rawValues[k];
      }
    }
  } else {
    Object.assign(next, rawValues);
  }
  const reqSet = new Set(Array.isArray(objNode.required) ? objNode.required.map(String) : []);

  for (const [key, rawProp] of Object.entries(props)) {
    const childSegs = [...parentSegs, key];
    if (secretKeySet.has(childSegs.join("."))) {
      delete next[key];
      continue;
    }
    const prop = mergeSchemaNode(rawProp, defs, depth + 1);
    if (isObjectSchemaNode(prop)) {
      const existingChild = isPlainObject(next[key])
        ? (next[key] as Record<string, unknown>)
        : isPlainObject(prop.default)
          ? (prop.default as Record<string, unknown>)
          : undefined;
      if (existingChild !== undefined || reqSet.has(key)) {
        next[key] = buildInitialObjectValues(
          prop,
          existingChild ?? {},
          childSegs,
          defs,
          secretKeySet,
          depth + 1,
          hideNonSecrets,
        );
      } else {
        delete next[key];
      }
      continue;
    }
    if (next[key] === undefined && prop.default !== undefined) {
      next[key] = Array.isArray(prop.default) ? structuredClone(prop.default) : prop.default;
    } else if (next[key] === undefined && prop.type === "boolean" && reqSet.has(key)) {
      next[key] = false;
    }
  }
  return next;
}

export function setSchemaValueAtPath(
  root: Record<string, unknown>,
  objNode: JsonSchemaProperty,
  defs: Record<string, JsonSchemaProperty>,
  secretKeySet: ReadonlySet<string>,
  segs: readonly string[],
  value: unknown,
  parentSegs: readonly string[] = [],
  depth = 0,
): Record<string, unknown> {
  if (segs.length === 0 || depth > MAX_SCHEMA_DEPTH) return root;
  const [head, ...rest] = segs;
  if (rest.length === 0) {
    return { ...root, [head]: value };
  }
  const props = isPlainObject(objNode.properties)
    ? (objNode.properties as Record<string, JsonSchemaProperty>)
    : {};
  const childProp = mergeSchemaNode(props[head], defs, depth + 1);
  const childSegs = [...parentSegs, head];
  let baseChild: Record<string, unknown>;
  if (isPlainObject(root[head])) {
    baseChild = root[head] as Record<string, unknown>;
  } else if (
    value === undefined ||
    value === null ||
    (value === "" &&
      !(() => {
        const leafProp = resolveSchemaPropAtPath(childProp, rest, defs);
        return Array.isArray(leafProp.enum) && leafProp.enum.includes("");
      })())
  ) {
    return root;
  } else {
    baseChild = isObjectSchemaNode(childProp)
      ? buildInitialObjectValues(childProp, {}, childSegs, defs, secretKeySet, depth + 1, false)
      : {};
  }
  return {
    ...root,
    [head]: setSchemaValueAtPath(
      baseChild,
      childProp,
      defs,
      secretKeySet,
      rest,
      value,
      childSegs,
      depth + 1,
    ),
  };
}

export function buildInitialValues(
  mergedRoot: JsonSchemaProperty,
  defs: Record<string, JsonSchemaProperty>,
  values: Record<string, unknown>,
  secretPaths: ReadonlyMap<string, string[]>,
  secretKeySet: ReadonlySet<string>,
  hideNonSecrets = false,
): Record<string, unknown> {
  const stripped = stripSecretPaths(values, secretPaths);
  return buildInitialObjectValues(mergedRoot, stripped, [], defs, secretKeySet, 0, hideNonSecrets);
}
