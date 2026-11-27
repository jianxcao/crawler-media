use crate::PackageError;
use crate::minisign::hex_lower;
use crate::schema_annotations::validate_annotations_and_secret;
use regex_automata::meta::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

const MAX_SCHEMA_DEPTH: usize = 16;
const MAX_SCHEMA_NODES: usize = 1024;
const MAX_PROPERTIES: usize = 128;
const MAX_PATTERN_LEN: usize = 512;

const ALLOWED_KEYWORDS: &[&str] = &[
    "$schema",
    "$defs",
    "$ref",
    "type",
    "title",
    "description",
    "default",
    "enum",
    "writeOnly",
    "format",
    "x-ui",
    "properties",
    "additionalProperties",
    "required",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minLength",
    "maxLength",
    "pattern",
    "minimum",
    "maximum",
];

const ALLOWED_TYPES: &[&str] = &["object", "array", "string", "integer", "number", "boolean"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretFieldSpec {
    pub key: String,
    pub path_segments: Vec<String>,
    pub pointer: String,
    pub required: bool,
}

#[derive(Clone, Debug)]
pub struct ValidatedConfigSchema {
    pub(crate) schema_revision: String,
    pub(crate) canonical_json: String,
    pub(crate) root: Value,
    pub(crate) defs: HashMap<String, Value>,
    pub(crate) secret_fields: Vec<SecretFieldSpec>,
}

impl ValidatedConfigSchema {
    pub fn schema_revision(&self) -> &str {
        &self.schema_revision
    }

    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    pub fn secret_fields(&self) -> &[SecretFieldSpec] {
        &self.secret_fields
    }

    pub fn root_json(&self) -> &Value {
        &self.root
    }

    pub(crate) fn resolve_node_chain<'a>(
        &'a self,
        node: &'a Value,
    ) -> Result<Vec<&'a Value>, PackageError> {
        collect_ref_chain_nodes(node, &self.defs)
    }
}

pub(crate) fn collect_ref_chain_nodes<'a>(
    mut node: &'a Value,
    defs: &'a HashMap<String, Value>,
) -> Result<Vec<&'a Value>, PackageError> {
    let mut chain = vec![node];
    let mut hops = 0usize;
    while let Some(r) = node
        .as_object()
        .and_then(|o| o.get("$ref"))
        .and_then(Value::as_str)
    {
        hops += 1;
        if hops > MAX_SCHEMA_DEPTH {
            return Err(PackageError::InvalidSchema(
                "$ref cycle or depth exceeded".into(),
            ));
        }
        let name = r
            .strip_prefix("#/$defs/")
            .ok_or_else(|| PackageError::InvalidSchema(format!("unsupported $ref `{r}`")))?;
        node = defs
            .get(name)
            .ok_or_else(|| PackageError::InvalidSchema(format!("dangling $ref `{r}`")))?;
        chain.push(node);
    }
    Ok(chain)
}

pub(crate) fn escape_json_pointer_seg(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

pub fn validate_config_schema(json: &Value) -> Result<ValidatedConfigSchema, PackageError> {
    let root_obj = json
        .as_object()
        .ok_or_else(|| PackageError::InvalidSchema("schema root must be a JSON object".into()))?;

    let defs = extract_defs(root_obj)?;
    let mut ctx = ValidateCtx {
        defs: &defs,
        node_count: 0,
        visiting_refs: HashSet::new(),
        secrets: Vec::new(),
    };

    // Validate all $defs independently too (even if unreferenced) for cycles/keywords
    for (def_name, def_val) in &defs {
        ctx.visiting_refs.insert(def_name.clone());
        validate_node(def_val, 0, false, &mut ctx, None, false)?;
        ctx.visiting_refs.remove(def_name);
    }

    // Reset node_count for root traversal & secret collection
    ctx.node_count = 0;
    ctx.secrets.clear();
    let root_segs: Vec<String> = Vec::new();
    validate_node(json, 0, false, &mut ctx, Some(&root_segs), false)?;

    let root_resolved = resolve_ref_chain(json, &defs)?;
    let root_type = root_resolved
        .as_object()
        .and_then(|o| o.get("type"))
        .and_then(Value::as_str);
    if root_type != Some("object") {
        return Err(PackageError::InvalidSchema(
            "root config schema must have type \"object\"".into(),
        ));
    }

    let canonical_json = serde_json::to_string(json)
        .map_err(|e| PackageError::InvalidSchema(format!("failed serializing schema: {e}")))?;
    let digest = Sha256::digest(canonical_json.as_bytes());
    let schema_revision = hex_lower(digest.as_slice());

    let secret_fields = ctx.secrets;
    Ok(ValidatedConfigSchema {
        schema_revision,
        canonical_json,
        root: json.clone(),
        defs,
        secret_fields,
    })
}

struct ValidateCtx<'a> {
    defs: &'a HashMap<String, Value>,
    node_count: usize,
    visiting_refs: HashSet<String>,
    secrets: Vec<SecretFieldSpec>,
}

fn extract_defs(root: &Map<String, Value>) -> Result<HashMap<String, Value>, PackageError> {
    let mut defs = HashMap::new();
    if let Some(raw_defs) = root.get("$defs") {
        let map = raw_defs
            .as_object()
            .ok_or_else(|| PackageError::InvalidSchema("$defs must be an object".into()))?;
        for (k, v) in map {
            if k.is_empty() || k.contains('/') {
                return Err(PackageError::InvalidSchema(format!(
                    "invalid $defs name `{k}`"
                )));
            }
            defs.insert(k.clone(), v.clone());
        }
    }
    Ok(defs)
}

fn resolve_ref_chain<'a>(
    mut node: &'a Value,
    defs: &'a HashMap<String, Value>,
) -> Result<&'a Value, PackageError> {
    let mut hops = 0usize;
    while let Some(r) = node
        .as_object()
        .and_then(|o| o.get("$ref"))
        .and_then(Value::as_str)
    {
        hops += 1;
        if hops > MAX_SCHEMA_DEPTH {
            return Err(PackageError::InvalidSchema("cyclic $ref".into()));
        }
        let name = r
            .strip_prefix("#/$defs/")
            .ok_or_else(|| PackageError::InvalidSchema(format!("unsupported $ref `{r}`")))?;
        node = defs
            .get(name)
            .ok_or_else(|| PackageError::InvalidSchema(format!("dangling $ref `{r}`")))?;
    }
    Ok(node)
}

fn validate_node(
    node: &Value,
    depth: usize,
    inside_array: bool,
    ctx: &mut ValidateCtx<'_>,
    path_segments: Option<&[String]>,
    is_required: bool,
) -> Result<(), PackageError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(PackageError::InvalidSchema(format!(
            "schema nesting depth exceeds max {MAX_SCHEMA_DEPTH}"
        )));
    }
    ctx.node_count += 1;
    if ctx.node_count > MAX_SCHEMA_NODES {
        return Err(PackageError::InvalidSchema(
            "schema exceeds maximum node count".into(),
        ));
    }
    let obj = node
        .as_object()
        .ok_or_else(|| PackageError::InvalidSchema("schema node must be an object".into()))?;

    for key in obj.keys() {
        if !ALLOWED_KEYWORDS.contains(&key.as_str()) {
            return Err(PackageError::InvalidSchema(format!(
                "unsupported JSON Schema keyword `{key}`"
            )));
        }
    }
    if (depth > 0 || !ctx.visiting_refs.is_empty())
        && (obj.contains_key("$defs") || obj.contains_key("$schema"))
    {
        return Err(PackageError::InvalidSchema(
            "$schema and $defs are only allowed at root".into(),
        ));
    }

    let secrets_before = ctx.secrets.len();
    let mut target_chain: Vec<&Value> = Vec::new();
    let has_ref = obj.contains_key("$ref");
    if let Some(ref_val) = obj.get("$ref") {
        let ref_str = ref_val
            .as_str()
            .ok_or_else(|| PackageError::InvalidSchema("$ref must be a string".into()))?;
        let def_name = parse_local_ref(ref_str)?;
        let target = ctx
            .defs
            .get(def_name)
            .ok_or_else(|| PackageError::InvalidSchema(format!("dangling $ref `{ref_str}`")))?;
        if !ctx.visiting_refs.insert(def_name.to_string()) {
            return Err(PackageError::InvalidSchema(format!(
                "recursive $ref cycle detected at `{ref_str}`"
            )));
        }
        validate_node(target, depth, inside_array, ctx, path_segments, is_required)?;
        ctx.visiting_refs.remove(def_name);
        target_chain = collect_ref_chain_nodes(target, ctx.defs)?;
    }

    let own_ty = parse_schema_type(obj)?;
    let target_ty = target_chain.iter().find_map(|n| {
        n.as_object()
            .and_then(|m| m.get("type"))
            .and_then(Value::as_str)
    });
    if let (Some(a), Some(b)) = (own_ty, target_ty)
        && a != b
    {
        return Err(PackageError::InvalidSchema(format!(
            "conflicting schema type `{a}` with $ref target type `{b}`"
        )));
    }
    let effective_ty = own_ty.or(target_ty);

    validate_annotations_and_secret(
        obj,
        effective_ty,
        &target_chain,
        inside_array,
        &mut ctx.secrets,
        path_segments,
        is_required,
        secrets_before,
    )?;

    validate_node_constraints(
        obj,
        effective_ty,
        &target_chain,
        has_ref,
        depth,
        inside_array,
        ctx,
        path_segments,
        secrets_before,
    )?;

    if let Some(en) = obj.get("enum") {
        let arr = en
            .as_array()
            .ok_or_else(|| PackageError::InvalidSchema("enum must be an array".into()))?;
        if arr.is_empty() {
            return Err(PackageError::InvalidSchema("enum must not be empty".into()));
        }
    }
    if let Some(def_val) = obj.get("default") {
        crate::config_values::validate_default_value(node, ctx.defs, def_val)?;
    }
    Ok(())
}

fn parse_schema_type(obj: &Map<String, Value>) -> Result<Option<&str>, PackageError> {
    match obj.get("type") {
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| PackageError::InvalidSchema("type must be a string".into()))?;
            if !ALLOWED_TYPES.contains(&s) {
                return Err(PackageError::InvalidSchema(format!(
                    "unsupported schema type `{s}`"
                )));
            }
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_node_constraints(
    obj: &Map<String, Value>,
    effective_ty: Option<&str>,
    target_chain: &[&Value],
    has_ref: bool,
    depth: usize,
    inside_array: bool,
    ctx: &mut ValidateCtx<'_>,
    path_segments: Option<&[String]>,
    secrets_before: usize,
) -> Result<(), PackageError> {
    match effective_ty {
        Some("object") => {
            validate_object_node(
                obj,
                target_chain,
                depth,
                inside_array,
                ctx,
                path_segments,
                secrets_before,
            )?;
        }
        Some("array") => {
            if !has_ref
                || obj.contains_key("items")
                || obj.contains_key("minItems")
                || obj.contains_key("maxItems")
                || obj.contains_key("uniqueItems")
            {
                validate_array_node(obj, target_chain, depth, ctx)?;
            }
        }
        Some("string") => validate_string_node(obj)?,
        Some("integer") | Some("number") => validate_number_node(obj)?,
        Some("boolean") => {}
        None if obj.get("enum").is_none() && !has_ref => {
            return Err(PackageError::InvalidSchema(
                "schema node must specify `type`, `$ref`, or `enum`".into(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_local_ref(ref_str: &str) -> Result<&str, PackageError> {
    let Some(name) = ref_str.strip_prefix("#/$defs/") else {
        return Err(PackageError::InvalidSchema(format!(
            "remote or non-local $ref `{ref_str}` is forbidden"
        )));
    };
    if name.is_empty() || name.contains('/') {
        return Err(PackageError::InvalidSchema(format!(
            "invalid local $ref `{ref_str}`"
        )));
    }
    Ok(name)
}

fn collect_required_union(
    obj: &Map<String, Value>,
    target_chain: &[&Value],
) -> Result<HashSet<String>, PackageError> {
    let mut req_set = HashSet::new();
    for node_map in std::iter::once(obj).chain(target_chain.iter().filter_map(|n| n.as_object())) {
        if let Some(req) = node_map.get("required") {
            let arr = req
                .as_array()
                .ok_or_else(|| PackageError::InvalidSchema("required must be an array".into()))?;
            for item in arr {
                let s = item.as_str().ok_or_else(|| {
                    PackageError::InvalidSchema("required items must be strings".into())
                })?;
                req_set.insert(s.to_string());
            }
        }
    }
    Ok(req_set)
}

fn collect_chain_prop_keys<'a>(target_chain: &'a [&'a Value]) -> HashSet<&'a str> {
    let mut keys = HashSet::new();
    for node in target_chain {
        if let Some(pmap) = node.get("properties").and_then(Value::as_object) {
            for k in pmap.keys() {
                keys.insert(k.as_str());
            }
        }
    }
    keys
}

fn validate_object_node(
    obj: &Map<String, Value>,
    target_chain: &[&Value],
    depth: usize,
    inside_array: bool,
    ctx: &mut ValidateCtx<'_>,
    path_segments: Option<&[String]>,
    secrets_before: usize,
) -> Result<(), PackageError> {
    if let Some(ap) = obj.get("additionalProperties")
        && ap.as_bool() != Some(false)
    {
        return Err(PackageError::InvalidSchema(
            "additionalProperties must be false for closed config objects".into(),
        ));
    }
    let req_set = collect_required_union(obj, target_chain)?;
    if let Some(segs) = path_segments
        && !req_set.is_empty()
    {
        for spec in &mut ctx.secrets[secrets_before..] {
            if spec.path_segments.len() == segs.len() + 1
                && spec.path_segments.starts_with(segs)
                && spec
                    .path_segments
                    .last()
                    .is_some_and(|k| req_set.contains(k))
            {
                spec.required = true;
            }
        }
    }
    let chain_prop_keys = collect_chain_prop_keys(target_chain);
    if let Some(props) = obj.get("properties") {
        let pmap = props
            .as_object()
            .ok_or_else(|| PackageError::InvalidSchema("properties must be an object".into()))?;
        if pmap.len() > MAX_PROPERTIES {
            return Err(PackageError::InvalidSchema(
                "object properties exceed maximum count".into(),
            ));
        }
        for (prop_name, prop_schema) in pmap {
            if chain_prop_keys.contains(prop_name.as_str()) {
                return Err(PackageError::InvalidSchema(format!(
                    "property `{prop_name}` cannot redeclare property defined in $ref target"
                )));
            }
            let next_segs = path_segments.map(|segs| {
                let mut v = segs.to_vec();
                v.push(prop_name.clone());
                v
            });
            validate_node(
                prop_schema,
                depth + 1,
                inside_array,
                ctx,
                next_segs.as_deref(),
                req_set.contains(prop_name),
            )?;
        }
    }
    Ok(())
}

fn validate_array_node(
    obj: &Map<String, Value>,
    target_chain: &[&Value],
    depth: usize,
    ctx: &mut ValidateCtx<'_>,
) -> Result<(), PackageError> {
    let min_items = parse_opt_u64(obj.get("minItems"), "minItems")?;
    let max_items = parse_opt_u64(obj.get("maxItems"), "maxItems")?;
    if let (Some(min), Some(max)) = (min_items, max_items)
        && min > max
    {
        return Err(PackageError::InvalidSchema(
            "minItems cannot exceed maxItems".into(),
        ));
    }
    if let Some(ui) = obj.get("uniqueItems")
        && !ui.is_boolean()
    {
        return Err(PackageError::InvalidSchema(
            "uniqueItems must be boolean".into(),
        ));
    }
    let chain_has_items = target_chain.iter().any(|n| n.get("items").is_some());
    if let Some(items) = obj.get("items") {
        if chain_has_items {
            return Err(PackageError::InvalidSchema(
                "array schema cannot redeclare `items` defined in $ref target".into(),
            ));
        }
        validate_node(items, depth + 1, true, ctx, None, false)?;
    } else if !chain_has_items {
        return Err(PackageError::InvalidSchema(
            "array schema must declare `items`".into(),
        ));
    }
    Ok(())
}

fn validate_string_node(obj: &Map<String, Value>) -> Result<(), PackageError> {
    let min_len = parse_opt_u64(obj.get("minLength"), "minLength")?;
    let max_len = parse_opt_u64(obj.get("maxLength"), "maxLength")?;
    if let (Some(min), Some(max)) = (min_len, max_len)
        && min > max
    {
        return Err(PackageError::InvalidSchema(
            "minLength cannot exceed maxLength".into(),
        ));
    }
    if let Some(pat) = obj.get("pattern") {
        let p = pat
            .as_str()
            .ok_or_else(|| PackageError::InvalidSchema("pattern must be a string".into()))?;
        if p.len() > MAX_PATTERN_LEN {
            return Err(PackageError::InvalidSchema(
                "regex pattern exceeds 512 bytes".into(),
            ));
        }
        Regex::new(p).map_err(|e| {
            PackageError::InvalidSchema(format!("invalid regex pattern `{p}`: {e}"))
        })?;
    }
    Ok(())
}

fn validate_number_node(obj: &Map<String, Value>) -> Result<(), PackageError> {
    let min = obj
        .get("minimum")
        .map(|v| {
            v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| {
                PackageError::InvalidSchema("minimum must be a finite number".into())
            })
        })
        .transpose()?;
    let max = obj
        .get("maximum")
        .map(|v| {
            v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| {
                PackageError::InvalidSchema("maximum must be a finite number".into())
            })
        })
        .transpose()?;
    if let (Some(a), Some(b)) = (min, max)
        && a > b
    {
        return Err(PackageError::InvalidSchema(
            "minimum cannot exceed maximum".into(),
        ));
    }
    Ok(())
}

fn parse_opt_u64(val: Option<&Value>, name: &str) -> Result<Option<u64>, PackageError> {
    val.map(|v| {
        v.as_u64().ok_or_else(|| {
            PackageError::InvalidSchema(format!("{name} must be a non-negative integer"))
        })
    })
    .transpose()
}
