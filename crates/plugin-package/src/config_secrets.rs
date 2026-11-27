use crate::config_values::{field_err, validate_string_value};
use crate::{PackageError, SchemaFieldError, SecretFieldSpec, ValidatedConfigSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "value", rename_all = "lowercase")]
pub enum SecretWriteAction {
    Keep,
    Replace(String),
    Clear,
}

pub type SecretFieldInput = SecretWriteAction;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSecretUpdate {
    pub key: String,
    pub change: SecretWriteAction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedConfigWrite {
    pub public_values: Value,
    pub secret_updates: Vec<ResolvedSecretUpdate>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SplitConfigResult {
    pub public_values: Value,
    pub secret_updates: Vec<ResolvedSecretUpdate>,
}

impl ValidatedConfigSchema {
    pub fn resolve_config_write(
        &self,
        values: &Value,
        secret_actions: &BTreeMap<String, SecretWriteAction>,
        existing_configured: &BTreeSet<String>,
    ) -> Result<ValidatedConfigWrite, Vec<SchemaFieldError>> {
        let mut errors = Vec::new();
        let (materialized_values, synthetic_paths) =
            materialize_active_secret_ancestors(self, values, secret_actions, existing_configured);
        let mut public_values = match self.validate_values(&materialized_values, false) {
            Ok(v) => v,
            Err(mut errs) => {
                errors.append(&mut errs);
                values.clone()
            }
        };
        if errors.is_empty() && !synthetic_paths.is_empty() {
            prune_empty_synthetic_ancestors(&mut public_values, &synthetic_paths);
        }

        let spec_map: BTreeMap<&str, &SecretFieldSpec> = self
            .secret_fields
            .iter()
            .map(|s| (s.key.as_str(), s))
            .collect();

        for key in secret_actions.keys() {
            if !spec_map.contains_key(key.as_str()) {
                errors.push(SchemaFieldError {
                    path: format!("/secrets/{key}"),
                    code: "unknown_secret".into(),
                    message: format!("unknown secret field `{key}`"),
                });
            }
        }

        for spec in &self.secret_fields {
            let action = secret_actions
                .get(&spec.key)
                .unwrap_or(&SecretWriteAction::Keep);
            let resulting_configured = match action {
                SecretWriteAction::Keep => existing_configured.contains(&spec.key),
                SecretWriteAction::Clear => false,
                SecretWriteAction::Replace(secret_val) => {
                    validate_secret_replacement(self, spec, secret_val, &mut errors);
                    true
                }
            };
            if !resulting_configured
                && is_secret_required_for_write(
                    self,
                    spec,
                    values,
                    secret_actions,
                    existing_configured,
                    &spec_map,
                )
            {
                errors.push(SchemaFieldError {
                    path: format!("/secrets/{}", spec.key),
                    code: "required".into(),
                    message: format!("required secret `{}` must be configured", spec.key),
                });
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        let secret_updates = secret_actions
            .iter()
            .map(|(key, change)| ResolvedSecretUpdate {
                key: key.clone(),
                change: change.clone(),
            })
            .collect();

        Ok(ValidatedConfigWrite {
            public_values,
            secret_updates,
        })
    }
}

pub fn split_config_secrets(
    schema: &ValidatedConfigSchema,
    values: &Value,
    secret_actions: &BTreeMap<String, SecretFieldInput>,
) -> Result<SplitConfigResult, PackageError> {
    schema
        .resolve_config_write(values, secret_actions, &BTreeSet::new())
        .map(|w| SplitConfigResult {
            public_values: w.public_values,
            secret_updates: w.secret_updates,
        })
        .map_err(|fields| PackageError::InvalidConfig { fields })
}

fn materialize_active_secret_ancestors(
    schema: &ValidatedConfigSchema,
    values: &Value,
    secret_actions: &BTreeMap<String, SecretWriteAction>,
    existing_configured: &BTreeSet<String>,
) -> (Value, Vec<Vec<String>>) {
    let mut cloned = values.clone();
    let mut synthetic: Vec<Vec<String>> = Vec::new();
    let Some(_) = cloned.as_object() else {
        return (cloned, synthetic);
    };
    for spec in &schema.secret_fields {
        if spec.path_segments.len() <= 1 {
            continue;
        }
        let active = match secret_actions
            .get(&spec.key)
            .unwrap_or(&SecretWriteAction::Keep)
        {
            SecretWriteAction::Replace(_) => true,
            SecretWriteAction::Clear => false,
            SecretWriteAction::Keep => existing_configured.contains(&spec.key),
        };
        if !active {
            continue;
        }
        let mut cur = &mut cloned;
        let mut schema_chain = schema.resolve_node_chain(&schema.root).ok();
        for depth in 0..spec.path_segments.len() - 1 {
            let seg = &spec.path_segments[depth];
            let Some(map) = cur.as_object_mut() else {
                break;
            };
            let in_required = schema_chain.as_ref().is_some_and(|chain| {
                chain.iter().any(|n| {
                    n.get("required")
                        .and_then(Value::as_array)
                        .is_some_and(|arr| arr.iter().any(|r| r.as_str() == Some(seg.as_str())))
                })
            });
            if !map.contains_key(seg) {
                if in_required {
                    break;
                }
                map.insert(seg.clone(), Value::Object(serde_json::Map::new()));
                let path = spec.path_segments[..=depth].to_vec();
                if !synthetic.contains(&path) {
                    synthetic.push(path);
                }
            }
            let Some(next) = map.get_mut(seg) else {
                break;
            };
            cur = next;
            schema_chain = schema_chain.and_then(|chain| {
                let next_node = chain.iter().find_map(|n| n.get("properties")?.get(seg))?;
                schema.resolve_node_chain(next_node).ok()
            });
        }
    }
    synthetic.sort_by_key(|b| std::cmp::Reverse(b.len()));
    (cloned, synthetic)
}

fn prune_empty_synthetic_ancestors(root: &mut Value, synthetic_paths: &[Vec<String>]) {
    for path in synthetic_paths {
        let _ = prune_path_if_empty(root, path);
    }
}

fn prune_path_if_empty(cur: &mut Value, path: &[String]) -> Option<()> {
    let (leaf, parents) = path.split_last()?;
    let mut target = cur;
    for seg in parents {
        target = target.as_object_mut()?.get_mut(seg)?;
    }
    let parent_map = target.as_object_mut()?;
    if parent_map
        .get(leaf)
        .and_then(Value::as_object)
        .is_some_and(serde_json::Map::is_empty)
    {
        parent_map.remove(leaf);
    }
    Some(())
}

fn validate_secret_replacement(
    schema: &ValidatedConfigSchema,
    spec: &SecretFieldSpec,
    val: &str,
    errors: &mut Vec<SchemaFieldError>,
) {
    let pointer = format!("/secrets/{}", spec.key);
    if val.len() > 65536 {
        errors.push(field_err(
            &pointer,
            "maxLength",
            "secret value exceeds 65536 bytes",
        ));
        return;
    }
    if val.is_empty() {
        errors.push(field_err(
            &pointer,
            "minLength",
            "secret replacement value cannot be empty; use clear to remove",
        ));
        return;
    }
    if let Some(chain) = lookup_secret_schema_chain(schema, &spec.path_segments) {
        let jval = Value::String(val.to_string());
        for node in chain {
            if let Some(obj) = node.as_object() {
                validate_string_value(obj, &jval, &pointer, errors);
                if let Some(en) = obj.get("enum").and_then(Value::as_array)
                    && !en.contains(&jval)
                {
                    errors.push(field_err(&pointer, "enum", "secret value not in enum"));
                }
            }
        }
    }
}

fn lookup_secret_schema_chain<'a>(
    schema: &'a ValidatedConfigSchema,
    segments: &[String],
) -> Option<Vec<&'a Value>> {
    let mut chain = schema.resolve_node_chain(&schema.root).ok()?;
    for seg in segments {
        let prop = chain.iter().find_map(|n| n.get("properties")?.get(seg))?;
        chain = schema.resolve_node_chain(prop).ok()?;
    }
    Some(chain)
}

fn is_secret_required_for_write(
    schema: &ValidatedConfigSchema,
    spec: &SecretFieldSpec,
    values: &Value,
    secret_actions: &BTreeMap<String, SecretWriteAction>,
    existing_configured: &BTreeSet<String>,
    spec_map: &BTreeMap<&str, &SecretFieldSpec>,
) -> bool {
    if !spec.required {
        return false;
    }
    if spec.path_segments.len() <= 1 {
        return true;
    }
    let mut cur_val = Some(values);
    let mut schema_chain = schema.resolve_node_chain(&schema.root).ok();
    for depth in 0..spec.path_segments.len() - 1 {
        let seg = &spec.path_segments[depth];
        let prefix = &spec.path_segments[..=depth];
        let in_values = cur_val
            .and_then(Value::as_object)
            .and_then(|m| m.get(seg))
            .is_some();
        let in_required = schema_chain.as_ref().is_some_and(|chain| {
            chain.iter().any(|n| {
                n.get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|arr| arr.iter().any(|r| r.as_str() == Some(seg.as_str())))
            })
        });
        let has_active_child_secret = spec_map.values().any(|s| {
            if !s.path_segments.starts_with(prefix) {
                return false;
            }
            match secret_actions
                .get(&s.key)
                .unwrap_or(&SecretWriteAction::Keep)
            {
                SecretWriteAction::Replace(_) => true,
                SecretWriteAction::Clear => false,
                SecretWriteAction::Keep => existing_configured.contains(&s.key),
            }
        });
        if !(in_values || in_required || has_active_child_secret) {
            return false;
        }
        cur_val = cur_val.and_then(Value::as_object).and_then(|m| m.get(seg));
        schema_chain = schema_chain.and_then(|chain| {
            let next_node = chain.iter().find_map(|n| n.get("properties")?.get(seg))?;
            schema.resolve_node_chain(next_node).ok()
        });
    }
    true
}
