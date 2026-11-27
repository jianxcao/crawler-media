use rusqlite::params;

use super::*;
use crate::{App, Store, StoreError};

/// Terminal redacted audit entry for plugin hook and event execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordPluginAudit {
    pub plugin_id: String,
    pub generation: Option<i64>,
    pub action: String,
    pub method: String,
    pub path: String,
    pub cause_id: String,
    pub state: PluginOperationState,
    pub phase: String,
    pub effect: PluginEffectObservation,
}

fn sanitize_phase(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "recorded".to_string();
    }
    trimmed.chars().take(256).collect()
}

fn sanitize_cause_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_graphic())
        .take(128)
        .collect();
    if cleaned.is_empty() {
        "audit".to_string()
    } else {
        cleaned
    }
}

fn effect_name(effect: PluginEffectObservation) -> &'static str {
    match effect {
        PluginEffectObservation::NoEffect => "none",
        PluginEffectObservation::Confirmed => "confirmed",
        PluginEffectObservation::Unknown => "unknown",
    }
}

impl App<'_> {
    pub fn append_plugin_audit(&self, entry: &RecordPluginAudit) -> Result<uuid::Uuid, StoreError> {
        if entry.plugin_id.trim().is_empty() || entry.action.trim().is_empty() {
            return Err(PluginError::Invalid("plugin_id and action must not be empty").into());
        }
        if !entry.state.is_terminal() {
            return Err(PluginError::Invalid("audit entry state must be terminal").into());
        }
        let op_id = uuid::Uuid::new_v4();
        let now = crate::unix_now();
        let plugin_scope = serde_json::to_string(&Some(entry.plugin_id.as_str()))?;
        let subject = serde_json::json!({ "System": entry.action }).to_string();
        let idempotency_key = format!("audit:{op_id}");
        let method = if entry.method.trim().is_empty() {
            "HOOK"
        } else {
            entry.method.trim()
        };
        let path = if entry.path.starts_with('/') {
            entry.path.clone()
        } else {
            format!("/{}", entry.path.trim_start_matches('/'))
        };
        let phase = sanitize_phase(&entry.phase);
        let cause_id = sanitize_cause_id(&entry.cause_id);
        let state_str = super::operation_rows::state_name(entry.state);
        let effect_str = effect_name(entry.effect);
        let empty_digest = [0u8; 32];

        let tx = crate::write_tx(self.app)?;
        let plugin_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM plugins WHERE plugin_id=?1)",
            params![&entry.plugin_id],
            |r| r.get(0),
        )?;
        let plugin_id_col = plugin_exists.then_some(entry.plugin_id.as_str());
        tx.execute(
            "INSERT INTO plugin_operations VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?11,?12,NULL,?13,?13)",
            params![
                op_id.to_string(),
                plugin_id_col,
                plugin_scope,
                subject,
                idempotency_key,
                &entry.action,
                method,
                path,
                empty_digest.as_slice(),
                entry.generation,
                state_str,
                phase,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO plugin_commands VALUES (?1,'management',NULL,NULL,NULL,NULL,'[]',?2,0,'[]',1,0,NULL,?3)",
            params![op_id.to_string(), cause_id, effect_str],
        )?;
        tx.commit()?;
        Ok(op_id)
    }
}

impl Store {
    pub fn append_plugin_audit(&self, entry: &RecordPluginAudit) -> Result<uuid::Uuid, StoreError> {
        self.with_app(|app| app.append_plugin_audit(entry))
    }
}
