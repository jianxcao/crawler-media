use crate::schema::{collect_ref_chain_nodes, escape_json_pointer_seg};
use crate::schema_annotations::node_has_secret_flag;
use crate::{PackageError, SchemaFieldError, ValidatedConfigSchema};
use regex_automata::meta::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretConfiguredStatus {
    pub configured: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedactedConfigView {
    pub values: Value,
    pub secrets: BTreeMap<String, SecretConfiguredStatus>,
}

#[derive(Clone, Copy)]
enum SecretLookup<'a> {
    RuntimePaths(&'a HashSet<&'a [String]>),
    SchemaDefault,
}

impl ValidatedConfigSchema {
    pub fn validate_values(
        &self,
        values: &Value,
        materialize_defaults: bool,
    ) -> Result<Value, Vec<SchemaFieldError>> {
        let secret_paths: HashSet<&[String]> = self
            .secret_fields
            .iter()
            .map(|s| s.path_segments.as_slice())
            .collect();
        let mut errors = Vec::new();
        let out = validate_value_node_with_defs(
            &self.defs,
            &self.root,
            values,
            "/values",
            &[],
            materialize_defaults,
            SecretLookup::RuntimePaths(&secret_paths),
            &mut errors,
        );
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    pub fn redact_config(
        &self,
        public_values: &Value,
        configured_secrets: &BTreeSet<String>,
    ) -> RedactedConfigView {
        let secret_paths: HashSet<&[String]> = self
            .secret_fields
            .iter()
            .map(|s| s.path_segments.as_slice())
            .collect();
        let cleaned = strip_secret_keys(public_values, &[], &secret_paths);
        let mut secrets = BTreeMap::new();
        for spec in &self.secret_fields {
            secrets.insert(
                spec.key.clone(),
                SecretConfiguredStatus {
                    configured: configured_secrets.contains(&spec.key),
                },
            );
        }
        RedactedConfigView {
            values: cleaned,
            secrets,
        }
    }
}

pub(crate) fn validate_default_value(
    node: &Value,
    defs: &HashMap<String, Value>,
    default_val: &Value,
) -> Result<(), PackageError> {
    let mut errors = Vec::new();
    validate_value_node_with_defs(
        defs,
        node,
        default_val,
        "/default",
        &[],
        false,
        SecretLookup::SchemaDefault,
        &mut errors,
    );
    if let Some(first) = errors.first() {
        return Err(PackageError::InvalidSchema(format!(
            "invalid default value at `{}`: {}",
            first.path, first.message
        )));
    }
    Ok(())
}

pub fn validate_config_values(
    schema: &ValidatedConfigSchema,
    values: &Value,
) -> Result<Value, PackageError> {
    schema
        .validate_values(values, false)
        .map_err(|fields| PackageError::InvalidConfig { fields })
}

pub fn project_config_with_secrets(
    schema: &ValidatedConfigSchema,
    public_values: &Value,
    configured_keys: &BTreeSet<String>,
) -> Value {
    let view = schema.redact_config(public_values, configured_keys);
    serde_json::json!({
        "values": view.values,
        "secrets": view.secrets,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_value_node_with_defs(
    defs: &HashMap<String, Value>,
    raw_node: &Value,
    val: &Value,
    pointer: &str,
    cur_path: &[String],
    materialize_defaults: bool,
    secret_lookup: SecretLookup<'_>,
    errors: &mut Vec<SchemaFieldError>,
) -> Value {
    let chain = match collect_ref_chain_nodes(raw_node, defs) {
        Ok(c) => c,
        Err(e) => {
            errors.push(field_err(pointer, "schema", e.to_string()));
            return val.clone();
        }
    };
    let objs: Vec<&Map<String, Value>> = chain.iter().filter_map(|v| v.as_object()).collect();
    if objs.is_empty() {
        return val.clone();
    }

    for obj in &objs {
        if let Some(en) = obj.get("enum").and_then(Value::as_array)
            && !en.contains(val)
        {
            errors.push(field_err(
                pointer,
                "enum",
                "value is not in allowed enum set",
            ));
            break;
        }
    }

    let ty = objs
        .iter()
        .find_map(|o| o.get("type").and_then(Value::as_str));
    match ty {
        Some("object") => validate_object_value(
            defs,
            &objs,
            val,
            pointer,
            cur_path,
            materialize_defaults,
            secret_lookup,
            errors,
        ),
        Some("array") => validate_array_value(defs, &objs, val, pointer, secret_lookup, errors),
        Some("string") => {
            if !val.is_string() {
                errors.push(field_err(pointer, "type", "expected string"));
            } else {
                for obj in &objs {
                    validate_string_value(obj, val, pointer, errors);
                }
            }
            val.clone()
        }
        Some("integer") => {
            if val.as_i64().is_none() && val.as_u64().is_none() {
                errors.push(field_err(pointer, "type", "expected integer"));
            } else {
                for obj in &objs {
                    validate_integer_value(obj, val, pointer, errors);
                }
            }
            val.clone()
        }
        Some("number") => {
            if val.as_f64().filter(|f| f.is_finite()).is_none() {
                errors.push(field_err(pointer, "type", "expected number"));
            } else {
                for obj in &objs {
                    validate_number_value(obj, val, pointer, errors);
                }
            }
            val.clone()
        }
        Some("boolean") => {
            if !val.is_boolean() {
                errors.push(field_err(pointer, "type", "expected boolean"));
            }
            val.clone()
        }
        _ => val.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_object_value(
    defs: &HashMap<String, Value>,
    objs: &[&Map<String, Value>],
    val: &Value,
    pointer: &str,
    cur_path: &[String],
    materialize_defaults: bool,
    secret_lookup: SecretLookup<'_>,
    errors: &mut Vec<SchemaFieldError>,
) -> Value {
    let Some(in_map) = val.as_object() else {
        errors.push(field_err(pointer, "type", "expected object"));
        return val.clone();
    };
    let mut props: BTreeMap<&str, &Value> = BTreeMap::new();
    let mut required_keys: BTreeSet<&str> = BTreeSet::new();
    for obj in objs.iter().rev() {
        if let Some(pmap) = obj.get("properties").and_then(Value::as_object) {
            for (k, v) in pmap {
                props.insert(k.as_str(), v);
            }
        }
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            for item in req {
                if let Some(s) = item.as_str() {
                    required_keys.insert(s);
                }
            }
        }
    }

    let mut out_map = in_map.clone();
    if materialize_defaults {
        for (&k, prop_raw) in &props {
            let mut child_path = cur_path.to_vec();
            child_path.push(k.to_string());
            if is_secret_prop(defs, &props, secret_lookup, k, &child_path)
                || out_map.contains_key(k)
            {
                continue;
            }
            if let Ok(chain) = collect_ref_chain_nodes(prop_raw, defs)
                && let Some(def) = chain.iter().find_map(|n| n.get("default"))
            {
                out_map.insert(k.to_string(), def.clone());
            }
        }
    }

    for k in &required_keys {
        let mut child_path = cur_path.to_vec();
        child_path.push((*k).to_string());
        if is_secret_prop(defs, &props, secret_lookup, k, &child_path) {
            continue;
        }
        if !out_map.contains_key(*k) {
            errors.push(field_err(
                &format!("{pointer}/{}", escape_json_pointer_seg(k)),
                "required",
                format!("missing required property `{k}`"),
            ));
        }
    }

    let mut finalized = Map::new();
    for (k, child_val) in &out_map {
        let child_pointer = format!("{pointer}/{}", escape_json_pointer_seg(k));
        let mut child_path = cur_path.to_vec();
        child_path.push(k.clone());
        if is_secret_prop(defs, &props, secret_lookup, k, &child_path) {
            errors.push(field_err(
                &child_pointer,
                "secret_in_values",
                format!("secret field `{k}` cannot be supplied in plaintext values"),
            ));
            continue;
        }
        let Some(prop_raw) = props.get(k.as_str()).copied() else {
            errors.push(field_err(
                &child_pointer,
                "additionalProperties",
                format!("unknown property `{k}`"),
            ));
            continue;
        };
        let validated_child = validate_value_node_with_defs(
            defs,
            prop_raw,
            child_val,
            &child_pointer,
            &child_path,
            materialize_defaults,
            secret_lookup,
            errors,
        );
        finalized.insert(k.clone(), validated_child);
    }
    Value::Object(finalized)
}

fn is_secret_prop(
    defs: &HashMap<String, Value>,
    props: &BTreeMap<&str, &Value>,
    secret_lookup: SecretLookup<'_>,
    key: &str,
    child_path: &[String],
) -> bool {
    match secret_lookup {
        SecretLookup::RuntimePaths(paths) => paths.contains(child_path),
        SecretLookup::SchemaDefault => props.get(key).is_some_and(|prop_raw| {
            collect_ref_chain_nodes(prop_raw, defs)
                .is_ok_and(|chain| chain.iter().any(|n| node_has_secret_flag(n)))
        }),
    }
}

fn validate_array_value(
    defs: &HashMap<String, Value>,
    objs: &[&Map<String, Value>],
    val: &Value,
    pointer: &str,
    secret_lookup: SecretLookup<'_>,
    errors: &mut Vec<SchemaFieldError>,
) -> Value {
    let Some(arr) = val.as_array() else {
        errors.push(field_err(pointer, "type", "expected array"));
        return val.clone();
    };
    for obj in objs {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64)
            && (arr.len() as u64) < min
        {
            errors.push(field_err(
                pointer,
                "minItems",
                format!("array length {} is less than minItems {min}", arr.len()),
            ));
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64)
            && (arr.len() as u64) > max
        {
            errors.push(field_err(
                pointer,
                "maxItems",
                format!("array length {} exceeds maxItems {max}", arr.len()),
            ));
        }
        if obj.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
            let mut seen = HashSet::new();
            for item in arr {
                let key = serde_json::to_string(item).unwrap_or_default();
                if !seen.insert(key) {
                    errors.push(field_err(
                        pointer,
                        "uniqueItems",
                        "array items must be unique",
                    ));
                    break;
                }
            }
        }
    }
    if let Some(item_schema) = objs.iter().find_map(|o| o.get("items")) {
        let mut out = Vec::with_capacity(arr.len());
        for (idx, item) in arr.iter().enumerate() {
            let item_ptr = format!("{pointer}/{idx}");
            out.push(validate_value_node_with_defs(
                defs,
                item_schema,
                item,
                &item_ptr,
                &[],
                false,
                secret_lookup,
                errors,
            ));
        }
        return Value::Array(out);
    }
    val.clone()
}

pub(crate) fn validate_string_value(
    obj: &Map<String, Value>,
    val: &Value,
    pointer: &str,
    errors: &mut Vec<SchemaFieldError>,
) {
    let Some(s) = val.as_str() else {
        errors.push(field_err(pointer, "type", "expected string"));
        return;
    };
    let char_len = s.chars().count() as u64;
    if let Some(min) = obj.get("minLength").and_then(Value::as_u64)
        && char_len < min
    {
        errors.push(field_err(
            pointer,
            "minLength",
            format!("string length {char_len} is less than minLength {min}"),
        ));
    }
    if let Some(max) = obj.get("maxLength").and_then(Value::as_u64)
        && char_len > max
    {
        errors.push(field_err(
            pointer,
            "maxLength",
            format!("string length {char_len} exceeds maxLength {max}"),
        ));
    }
    if let Some(pat) = obj.get("pattern").and_then(Value::as_str)
        && let Ok(re) = Regex::new(pat)
        && !re.is_match(s)
    {
        errors.push(field_err(
            pointer,
            "pattern",
            format!("string does not match pattern `{pat}`"),
        ));
    }
}

fn validate_integer_value(
    obj: &Map<String, Value>,
    val: &Value,
    pointer: &str,
    errors: &mut Vec<SchemaFieldError>,
) {
    let Some(n) = val
        .as_i64()
        .map(|i| i as f64)
        .or_else(|| val.as_u64().map(|u| u as f64))
    else {
        errors.push(field_err(pointer, "type", "expected integer"));
        return;
    };
    check_min_max(obj, n, pointer, errors);
}

fn validate_number_value(
    obj: &Map<String, Value>,
    val: &Value,
    pointer: &str,
    errors: &mut Vec<SchemaFieldError>,
) {
    let Some(n) = val.as_f64().filter(|f| f.is_finite()) else {
        errors.push(field_err(pointer, "type", "expected number"));
        return;
    };
    check_min_max(obj, n, pointer, errors);
}

fn check_min_max(
    obj: &Map<String, Value>,
    n: f64,
    pointer: &str,
    errors: &mut Vec<SchemaFieldError>,
) {
    if let Some(min) = obj.get("minimum").and_then(Value::as_f64)
        && n < min
    {
        errors.push(field_err(
            pointer,
            "minimum",
            format!("value {n} is less than minimum {min}"),
        ));
    }
    if let Some(max) = obj.get("maximum").and_then(Value::as_f64)
        && n > max
    {
        errors.push(field_err(
            pointer,
            "maximum",
            format!("value {n} exceeds maximum {max}"),
        ));
    }
}

fn strip_secret_keys(val: &Value, cur_path: &[String], secret_paths: &HashSet<&[String]>) -> Value {
    let Some(map) = val.as_object() else {
        return val.clone();
    };
    let mut out = Map::new();
    for (k, v) in map {
        let mut next = cur_path.to_vec();
        next.push(k.clone());
        if secret_paths.contains(next.as_slice()) {
            continue;
        }
        out.insert(k.clone(), strip_secret_keys(v, &next, secret_paths));
    }
    Value::Object(out)
}

pub(crate) fn field_err(path: &str, code: &str, message: impl Into<String>) -> SchemaFieldError {
    SchemaFieldError {
        path: path.to_string(),
        code: code.to_string(),
        message: message.into(),
    }
}
