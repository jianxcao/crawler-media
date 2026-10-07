use serde_json::{Value, json};

use super::Transport;
use super::parse::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterAtomSpec {
    pub kind: String,
    pub value: Option<String>,
    pub priority: i32,
}

pub fn parse_filters(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Filters);
    }
    let mut add = false;
    let mut default = false;
    let mut name = None;
    let mut id = None;
    let mut atoms: Vec<FilterAtomSpec> = Vec::new();
    let mut next_priority = 0;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--add" => {
                add = true;
                rest = tail;
            }
            "--default" => {
                default = true;
                rest = tail;
            }
            "--name" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--name needs a value".to_string())?;
                name = Some((*value).to_string());
                rest = next;
            }
            "--id" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--id needs a value".to_string())?;
                id = Some((*value).to_string());
                rest = next;
            }
            "--atom" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--atom needs a value".to_string())?;
                atoms.push(parse_atom(value, next_priority)?);
                rest = next;
            }
            "--priority" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--priority needs a value".to_string())?;
                let priority = value
                    .parse::<i32>()
                    .map_err(|_| "--priority needs an integer".to_string())?;
                next_priority = priority;
                if let Some(atom) = atoms.last_mut() {
                    atom.priority = priority;
                }
                rest = next;
            }
            other => return Err(format!("unknown filters flag {other}")),
        }
    }
    if add && default {
        return Err("use --add or --default, not both".into());
    }
    if default {
        return Ok(Command::FiltersDefault {
            id: id.ok_or_else(|| "default needs --id".to_string())?,
        });
    }
    if !add {
        return Err(
            "usage: filters | filters --add --name <name> --atom <kind[=value]> [--priority <n>] | filters --default --id <id>"
                .into(),
        );
    }
    Ok(Command::FiltersAdd {
        name: name.ok_or_else(|| "add needs --name".to_string())?,
        atoms: if atoms.is_empty() {
            return Err("add needs --atom".into());
        } else {
            atoms
        },
    })
}

fn parse_atom(raw: &str, priority: i32) -> Result<FilterAtomSpec, String> {
    let (kind, value) = match raw.split_once('=') {
        Some((kind, value)) => (kind, Some(value.to_string())),
        None => (raw, None),
    };
    match kind {
        "resolution" | "source" | "title_match" => {
            let value = value
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{kind} atom needs a value"))?;
            Ok(FilterAtomSpec {
                kind: kind.into(),
                value: Some(value),
                priority,
            })
        }
        "free" | "hr" => {
            if value.is_some() {
                return Err(format!("{kind} atom does not take a value"));
            }
            Ok(FilterAtomSpec {
                kind: kind.into(),
                value: None,
                priority,
            })
        }
        other => Err(format!("unknown Filter atom {other}")),
    }
}

pub async fn create_filter(
    transport: &impl Transport,
    name: &str,
    atoms: &[FilterAtomSpec],
) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("name is required".into());
    }
    if atoms.is_empty() {
        return Err("at least one atom is required".into());
    }
    let body = json!({
        "name": name,
        "atoms": atoms
            .iter()
            .map(|atom| {
                json!({
                    "kind": atom.kind,
                    "value": atom.value,
                    "priority": atom.priority,
                })
            })
            .collect::<Vec<Value>>(),
    });
    let (status, body) = transport
        .send("POST", "/api/v1/rule-sets", Some(body))
        .await?;
    if !(200..300).contains(&status) {
        return Err(format!("filters add failed: {status} {body}"));
    }
    let name = body["name"].as_str().unwrap_or(name);
    let atoms = body["atoms"].as_array().map(|a| a.len()).unwrap_or(0);
    Ok(format!("{name}\tatoms={atoms}\n"))
}

pub async fn set_default_filter(transport: &impl Transport, id: &str) -> Result<String, String> {
    if id.trim().is_empty() {
        return Err("id is required".into());
    }
    let body = json!({ "id": id });
    let (status, body) = transport
        .send("PUT", "/api/v1/rule-sets/default", Some(body))
        .await?;
    if status != 200 {
        return Err(format!("filters default failed: {status} {body}"));
    }
    let id = body["default_rule_set_id"].as_str().unwrap_or(id);
    Ok(format!("default_filter_id={id}\n"))
}
