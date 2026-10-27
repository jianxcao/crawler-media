//! CLI uses the current Library operation and revisioned Transfer contracts.
use super::Transport;
use serde_json::{Value, json};

pub async fn update_root(
    transport: &impl Transport,
    id: &str,
    path: &str,
    add: bool,
) -> Result<String, String> {
    let endpoint = format!("/api/v1/libraries/{id}");
    let (status, libraries) = transport
        .send("GET", "/api/v1/libraries?scope=admin", None)
        .await?;
    if status != 200 {
        return Err(format!("Library failed: {status} {libraries}"));
    }
    let library = libraries
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .ok_or_else(|| format!("Library {id} not found"))?;
    let mut roots = library["root_paths"]
        .as_array()
        .cloned()
        .ok_or("Library has no root_paths")?;
    if add {
        if roots.iter().any(|root| root.as_str() == Some(path)) {
            return Err("root already exists".into());
        }
        roots.push(json!(path));
    } else {
        let before = roots.len();
        roots.retain(|root| root.as_str() != Some(path));
        if roots.len() == before {
            return Err("root does not exist".into());
        }
    }
    let (status, operation) = transport
        .send("PATCH", &endpoint, Some(json!({ "root_paths": roots })))
        .await?;
    if status != 202 {
        return Err(format!("root operation failed: {status} {operation}"));
    }
    let operation_id = operation["operation_id"]
        .as_str()
        .ok_or("missing operation_id")?;
    let operation_path = format!("{endpoint}/operations/{operation_id}");
    for _ in 0..300 {
        let (status, operation) = transport.send("GET", &operation_path, None).await?;
        if status != 200 {
            return Err(format!("operation failed: {status} {operation}"));
        }
        match operation["state"].as_str() {
            Some("succeeded") => return Ok(format!("library={id} root={path} added={add}\n")),
            Some("failed" | "cancelled") => {
                return Err(format!("root operation failed: {operation}"));
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    Err(format!(
        "root operation {operation_id} is still running; inspect {operation_path}"
    ))
}

pub async fn set_watch_inplace(transport: &impl Transport, path: &str) -> Result<String, String> {
    patch_transfer(transport, "watch_inplace", path_value(path)).await
}
pub async fn set_watch_intake(transport: &impl Transport, path: &str) -> Result<String, String> {
    patch_transfer(transport, "watch_intake", path_value(path)).await
}
pub async fn set_transfer_mode(transport: &impl Transport, mode: &str) -> Result<String, String> {
    patch_transfer(transport, "transfer_mode", json!(mode)).await
}
pub async fn set_scrape(transport: &impl Transport, enabled: bool) -> Result<String, String> {
    patch_transfer(transport, "scrape", json!(enabled)).await
}
fn path_value(path: &str) -> Value {
    if path.trim().is_empty() {
        Value::Null
    } else {
        json!(path.trim())
    }
}

async fn patch_transfer(
    transport: &impl Transport,
    field: &str,
    value: Value,
) -> Result<String, String> {
    let endpoint = "/api/v1/settings/transfer";
    let (status, current) = transport.send("GET", endpoint, None).await?;
    if status != 200 {
        return Err(format!("Transfer settings failed: {status} {current}"));
    }
    let mut patch = json!({ "expected_revision": current["revision"] });
    patch[field] = value;
    let (status, saved) = transport.send("PATCH", endpoint, Some(patch)).await?;
    if status != 200 {
        return Err(format!("{field} failed: {status} {saved}"));
    }
    Ok(format!(
        "{field}={}\n",
        saved[field]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| saved[field].to_string())
    ))
}

pub async fn set_naming(
    transport: &impl Transport,
    field: &str,
    pattern: &str,
) -> Result<String, String> {
    let endpoint = "/api/v1/settings/scrape";
    let (status, current) = transport.send("GET", endpoint, None).await?;
    if status != 200 {
        return Err(format!("Scrape settings failed: {status} {current}"));
    }
    let mut setting = current["setting"].clone();
    setting[field] = json!(pattern);
    let (status, saved) = transport
        .send("PUT", endpoint, Some(json!({ "setting": setting })))
        .await?;
    if status != 200 {
        return Err(format!("Naming template failed: {status} {saved}"));
    }
    Ok(format!(
        "{field}={}\n",
        saved["setting"][field].as_str().unwrap_or_default()
    ))
}
