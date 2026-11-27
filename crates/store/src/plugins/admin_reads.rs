use super::operation_rows;
use super::staged_operations::{
    PluginAdminOperation, active_install_lock_op, load_intent, load_result,
};
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug, PartialEq)]
pub struct PluginSecretMetadata {
    pub scope: PluginSecurityScope,
    pub key: String,
    pub configured: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginConfigSummary {
    pub revision: i64,
    pub config_revision: i64,
    pub config: Option<PluginConfig>,
    pub secrets: Vec<PluginSecretMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTaskReceiptSummary {
    pub operation_id: uuid::Uuid,
    pub kind: String,
    pub generation: Option<i64>,
    pub state: PluginOperationState,
    pub phase: String,
    pub effect: PluginEffectObservation,
    pub targets: Vec<PluginGrantScope>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginAuditEvent {
    pub operation_id: uuid::Uuid,
    pub plugin_id: Option<String>,
    pub action: String,
    pub subject_kind: String,
    pub canonical_method: String,
    pub canonical_path: String,
    pub state: PluginOperationState,
    pub phase: String,
    pub effect: PluginEffectObservation,
    pub created_at: i64,
    pub updated_at: i64,
}

fn lifecycle_name(lifecycle: PluginLifecycle) -> &'static str {
    match lifecycle {
        PluginLifecycle::InstalledDisabled => "installed-disabled",
        PluginLifecycle::Enabling => "enabling",
        PluginLifecycle::Enabled => "enabled",
        PluginLifecycle::Draining => "draining",
        PluginLifecycle::Disabled => "disabled",
    }
}

fn subject_kind_label(subject_json: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(subject_json) {
        if val.get("User").is_some() {
            return "user".into();
        }
        if val.get("EndpointKey").is_some() {
            return "endpoint-key".into();
        }
        if val.get("Task").is_some() {
            return "task".into();
        }
        if val.get("System").is_some() {
            return "system".into();
        }
    }
    "unknown".into()
}

fn parse_audit_cursor(after: Option<&str>) -> Result<Option<(i64, String)>, StoreError> {
    let Some(raw) = after else {
        return Ok(None);
    };
    let Some((ts_str, id_str)) = raw.split_once(':') else {
        return Err(PluginError::Invalid("invalid audit cursor").into());
    };
    let ts: i64 = ts_str
        .parse()
        .map_err(|_| PluginError::Invalid("invalid audit cursor timestamp"))?;
    let id = uuid::Uuid::parse_str(id_str)
        .map_err(|_| PluginError::Invalid("invalid audit cursor id"))?;
    Ok(Some((ts, id.to_string())))
}

fn parse_effect(raw: &str) -> rusqlite::Result<PluginEffectObservation> {
    match raw {
        "none" => Ok(PluginEffectObservation::NoEffect),
        "confirmed" => Ok(PluginEffectObservation::Confirmed),
        "unknown" => Ok(PluginEffectObservation::Unknown),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_state(raw: &str) -> rusqlite::Result<PluginOperationState> {
    match raw {
        "queued" => Ok(PluginOperationState::Queued),
        "running" => Ok(PluginOperationState::Running),
        "waiting" => Ok(PluginOperationState::Waiting),
        "blocked" => Ok(PluginOperationState::Blocked),
        "succeeded" => Ok(PluginOperationState::Succeeded),
        "rejected" => Ok(PluginOperationState::Rejected),
        "deferred" => Ok(PluginOperationState::Deferred),
        "failed" => Ok(PluginOperationState::Failed),
        "cancelled" => Ok(PluginOperationState::Cancelled),
        "unknown" => Ok(PluginOperationState::Unknown),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

impl App<'_> {
    pub fn list_plugins_page(
        &self,
        lifecycle: Option<PluginLifecycle>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginRecord>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugins WHERE (?1 IS NULL OR lifecycle=?1) AND plugin_id>?2 ORDER BY plugin_id LIMIT ?3",
            super::records::COLUMNS
        ))?;
        let items = stmt
            .query_map(
                params![
                    lifecycle.map(lifecycle_name),
                    after.unwrap_or(""),
                    (cap + 1) as i64
                ],
                super::records::plugin_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |p| p.plugin_id.clone()))
    }

    pub fn get_plugin_config_summary(&self, id: &str) -> Result<PluginConfigSummary, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let p = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        let active_ver = self
            .get_plugin_version(id, &p.package_hash)?
            .ok_or(PluginError::NotFound)?;
        let config = self.get_plugin_config(id)?;
        let mut stmt = tx.prepare(
            "SELECT security_scope,key FROM plugin_config_secrets WHERE plugin_id=?1 ORDER BY security_scope,key",
        )?;
        let raw = stmt
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut secrets = Vec::new();
        for (scope_raw, key) in raw {
            let scope = PluginSecurityScope::from_key(&scope_raw)?;
            if scope.site_id().is_some() != active_ver.credential_bound {
                continue;
            }
            match scope.validate(self, false) {
                Ok(()) => secrets.push(PluginSecretMetadata {
                    scope,
                    key,
                    configured: true,
                }),
                Err(StoreError::Plugin(PluginError::StaleAuthority)) => continue,
                Err(err) => return Err(err),
            }
        }
        tx.commit()?;
        Ok(PluginConfigSummary {
            revision: p.revision,
            config_revision: p.config_revision,
            config,
            secrets,
        })
    }

    pub fn get_plugin_operation_by_idempotency(
        &self,
        subject: &PluginSubject,
        plugin_id: Option<&str>,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        kind: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let subject_json = serde_json::to_string(subject)?;
        let plugin_scope = serde_json::to_string(&plugin_id)?;
        let prior: Option<(String, String, String, Vec<u8>, String)> = tx
            .query_row(
                "SELECT operation_id,canonical_method,canonical_path,body_digest,kind FROM plugin_operations WHERE subject=?1 AND plugin_scope=?2 AND idempotency_key=?3",
                params![subject_json, plugin_scope, idempotency_key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((id, method, path, body, stored_kind)) = prior else {
            if super::idempotency::has_idempotency_envelope(self, subject, idempotency_key)?
                || super::idempotency::has_operation_for_subject_key(
                    self,
                    &subject_json,
                    idempotency_key,
                )?
            {
                return Err(PluginError::IdempotencyConflict.into());
            }
            tx.commit()?;
            return Ok(None);
        };
        if method != request.method
            || path != request.path
            || body != request.body_digest
            || stored_kind != kind
        {
            return Err(PluginError::IdempotencyConflict.into());
        }
        let op = operation_rows::get(self, uuid::Uuid::parse_str(&id)?)?;
        tx.commit()?;
        Ok(op)
    }

    pub fn get_plugin_operation_admin(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<PluginAdminOperation>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let Some(operation) = operation_rows::get(self, id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let cmd = operation_rows::command(self, id)?;
        let intent = load_intent(self, id)?;
        let mut result_json = load_result(self, id)?;
        if result_json.is_none()
            && let Some((scope_raw, bytes)) = tx
                .query_row(
                    "SELECT security_scope, result FROM plugin_operation_results WHERE operation_id=?1",
                    [id.to_string()],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?
            && PluginSecurityScope::from_key(&scope_raw)
                .ok()
                .is_some_and(|sc| sc.validate(self, false).is_ok())
        {
            result_json = serde_json::from_slice(&bytes).ok();
        }
        tx.commit()?;
        Ok(Some(PluginAdminOperation {
            operation,
            command_revision: cmd.revision,
            attempt: cmd.attempt,
            intent,
            result_json,
        }))
    }

    pub fn get_active_plugin_operation(
        &self,
        plugin_id: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let op = if let Some(p) = self.get_plugin(plugin_id)? {
            match p.active_operation_id {
                Some(op_id) => operation_rows::get(self, op_id)?,
                None => None,
            }
        } else {
            active_install_lock_op(self, plugin_id)?
        };
        tx.commit()?;
        Ok(op)
    }

    pub fn list_plugin_operations_admin(
        &self,
        plugin_id: Option<&str>,
        state: Option<PluginOperationState>,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginOperation>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let plugin_scope = plugin_id
            .map(|id| serde_json::to_string(&Some(id)))
            .transpose()?;
        let state_str = state.map(operation_rows::state_name);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugin_operations WHERE (?1 IS NULL OR plugin_id=?1 OR plugin_scope=?2) AND (?3 IS NULL OR state=?3) AND operation_id>?4 ORDER BY operation_id LIMIT ?5",
            operation_rows::COLUMNS
        ))?;
        let items = stmt
            .query_map(
                params![
                    plugin_id,
                    plugin_scope,
                    state_str,
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    (cap + 1) as i64
                ],
                operation_rows::row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |op| {
            op.operation_id.to_string()
        }))
    }

    pub fn list_unresolved_staged_plugin_operations(
        &self,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginAdminOperation>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(
            "SELECT operation_id FROM plugin_operations WHERE state NOT IN ('succeeded','rejected','failed','cancelled') AND EXISTS(SELECT 1 FROM settings s WHERE s.key='plugin_op.intent.'||plugin_operations.operation_id) AND operation_id>?1 ORDER BY operation_id LIMIT ?2",
        )?;
        let ids = stmt
            .query_map(
                params![
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    (cap + 1) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut items = Vec::with_capacity(ids.len());
        for raw_id in ids {
            let id = uuid::Uuid::parse_str(&raw_id)?;
            if let Some(op) = operation_rows::get(self, id)? {
                let cmd = operation_rows::command(self, id)?;
                items.push(PluginAdminOperation {
                    operation: op,
                    command_revision: cmd.revision,
                    attempt: cmd.attempt,
                    intent: load_intent(self, id)?,
                    result_json: load_result(self, id)?,
                });
            }
        }
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |op| {
            op.operation.operation_id.to_string()
        }))
    }

    pub fn list_plugin_task_bindings(
        &self,
        plugin_id: &str,
        after_task_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTaskBinding>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugin_task_bindings WHERE plugin_id=?1 AND task_id>?2 ORDER BY task_id LIMIT ?3",
            super::tasks::COLUMNS
        ))?;
        let items = stmt
            .query_map(
                params![plugin_id, after_task_id.unwrap_or(""), (cap + 1) as i64],
                super::tasks::row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |b| b.task_id.clone()))
    }

    pub fn list_plugin_task_receipts(
        &self,
        plugin_id: &str,
        task_id: &str,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTaskReceiptSummary>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let subject = serde_json::to_string(&PluginSubject::Task {
            plugin_id: plugin_id.into(),
            task_id: task_id.into(),
        })?;
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(
            "SELECT o.operation_id,o.kind,o.generation,o.state,o.phase,c.effect,c.target_scopes,o.created_at,o.updated_at FROM plugin_operations o JOIN plugin_commands c ON c.operation_id=o.operation_id WHERE o.subject=?1 AND o.operation_id>?2 ORDER BY o.operation_id LIMIT ?3",
        )?;
        let raw = stmt
            .query_map(
                params![
                    subject,
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    (cap + 1) as i64
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i64>(8)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        let items = raw
            .into_iter()
            .map(
                |(id, kind, generation, state, phase, effect, targets, created_at, updated_at)| {
                    Ok(PluginTaskReceiptSummary {
                        operation_id: uuid::Uuid::parse_str(&id)?,
                        kind,
                        generation,
                        state: parse_state(&state)
                            .map_err(|_| PluginError::Invalid("invalid operation state"))?,
                        phase,
                        effect: parse_effect(&effect)
                            .map_err(|_| PluginError::Invalid("invalid command effect"))?,
                        targets: serde_json::from_str(&targets)?,
                        created_at,
                        updated_at,
                    })
                },
            )
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(super::paging::paginate(items, cap, |r| {
            r.operation_id.to_string()
        }))
    }

    pub fn list_plugin_audit_events(
        &self,
        plugin_id: &str,
        action: Option<&str>,
        from_ts: Option<i64>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginAuditEvent>, StoreError> {
        let cap = super::paging::api_limit(limit);
        let cursor = parse_audit_cursor(after)?;
        let plugin_scope = serde_json::to_string(&Some(plugin_id))?;
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(
            "SELECT o.operation_id,o.plugin_id,o.kind,o.subject,o.canonical_method,o.canonical_path,o.state,o.phase,c.effect,o.created_at,o.updated_at FROM plugin_operations o JOIN plugin_commands c ON c.operation_id=o.operation_id WHERE (o.plugin_id=?1 OR o.plugin_scope=?2) AND (?3 IS NULL OR o.kind=?3) AND (?4 IS NULL OR o.created_at>=?4) AND (?5 IS NULL OR (o.created_at<?5 OR (o.created_at=?5 AND o.operation_id<?6))) ORDER BY o.created_at DESC,o.operation_id DESC LIMIT ?7",
        )?;
        let (after_ts, after_id) = match cursor {
            Some((ts, id)) => (Some(ts), Some(id)),
            None => (None, None),
        };
        let items = stmt
            .query_map(
                params![
                    plugin_id,
                    plugin_scope,
                    action,
                    from_ts,
                    after_ts,
                    after_id,
                    (cap + 1) as i64
                ],
                |r| {
                    let id_str: String = r.get(0)?;
                    let op_id = uuid::Uuid::parse_str(&id_str).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    let subject_raw: String = r.get(3)?;
                    Ok(PluginAuditEvent {
                        operation_id: op_id,
                        plugin_id: r.get(1)?,
                        action: r.get(2)?,
                        subject_kind: subject_kind_label(&subject_raw),
                        canonical_method: r.get(4)?,
                        canonical_path: r.get(5)?,
                        state: parse_state(&r.get::<_, String>(6)?)?,
                        phase: r.get(7)?,
                        effect: parse_effect(&r.get::<_, String>(8)?)?,
                        created_at: r.get(9)?,
                        updated_at: r.get(10)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(super::paging::paginate(items, cap, |e| {
            format!("{}:{}", e.created_at, e.operation_id)
        }))
    }
}

impl Store {
    pub fn list_plugins_page(
        &self,
        lifecycle: Option<PluginLifecycle>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginRecord>, StoreError> {
        self.with_app_read(|app| app.list_plugins_page(lifecycle, after, limit))
    }

    pub fn get_plugin_config_summary(&self, id: &str) -> Result<PluginConfigSummary, StoreError> {
        self.with_app_read(|app| app.get_plugin_config_summary(id))
    }

    pub fn get_plugin_operation_by_idempotency(
        &self,
        subject: &PluginSubject,
        plugin_id: Option<&str>,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        kind: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        self.with_app_read(|app| {
            app.get_plugin_operation_by_idempotency(
                subject,
                plugin_id,
                idempotency_key,
                request,
                kind,
            )
        })
    }

    pub fn get_plugin_operation_admin(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<PluginAdminOperation>, StoreError> {
        self.with_app_read(|app| app.get_plugin_operation_admin(id))
    }

    pub fn get_active_plugin_operation(
        &self,
        plugin_id: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        self.with_app_read(|app| app.get_active_plugin_operation(plugin_id))
    }

    pub fn list_plugin_operations_admin(
        &self,
        plugin_id: Option<&str>,
        state: Option<PluginOperationState>,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginOperation>, StoreError> {
        self.with_app_read(|app| app.list_plugin_operations_admin(plugin_id, state, after, limit))
    }

    pub fn list_unresolved_staged_plugin_operations(
        &self,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginAdminOperation>, StoreError> {
        self.with_app_read(|app| app.list_unresolved_staged_plugin_operations(after, limit))
    }

    pub fn list_plugin_task_bindings(
        &self,
        plugin_id: &str,
        after_task_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTaskBinding>, StoreError> {
        self.with_app_read(|app| app.list_plugin_task_bindings(plugin_id, after_task_id, limit))
    }

    pub fn list_plugin_task_receipts(
        &self,
        plugin_id: &str,
        task_id: &str,
        after: Option<uuid::Uuid>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginTaskReceiptSummary>, StoreError> {
        self.with_app_read(|app| app.list_plugin_task_receipts(plugin_id, task_id, after, limit))
    }

    pub fn list_plugin_audit_events(
        &self,
        plugin_id: &str,
        action: Option<&str>,
        from_ts: Option<i64>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<PluginAuditEvent>, StoreError> {
        self.with_app_read(|app| {
            app.list_plugin_audit_events(plugin_id, action, from_ts, after, limit)
        })
    }
}
