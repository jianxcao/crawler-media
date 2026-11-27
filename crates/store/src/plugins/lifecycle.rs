use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;

#[derive(Clone, Debug, PartialEq)]
pub struct UninstalledPlugin {
    pub plugin_id: String,
    pub removed_task_bindings: Vec<PluginTaskBinding>,
    pub unreferenced_package_hashes: Vec<String>,
    pub private_data_removed: bool,
}

pub(super) struct ActivationPolicy {
    pub developer_mode_enabled: bool,
    pub signing_key_active: Option<bool>,
}

pub(super) fn check_activation_policy(
    app: &App<'_>,
    version: &PluginVersionMetadata,
) -> Result<ActivationPolicy, StoreError> {
    super::ui_trust::ensure_unique_manifest_remote_name(
        app,
        &version.plugin_id,
        &version.manifest_json,
    )?;
    let dev_mode = super::developer_mode::is_enabled(app)?;
    match version.signature_status.as_str() {
        "signed" | "minisign" => {
            let key_id = version
                .signing_key_id
                .as_deref()
                .ok_or(PluginError::Denied)?;
            if !super::trust_keys::is_key_active(app, key_id)? {
                return Err(PluginError::Denied.into());
            }
            Ok(ActivationPolicy {
                developer_mode_enabled: dev_mode,
                signing_key_active: Some(true),
            })
        }
        "unsigned" | "development" => {
            if version.signing_key_id.is_some() {
                return Err(
                    PluginError::Invalid("unsigned package cannot carry signing_key_id").into(),
                );
            }
            if !dev_mode {
                return Err(PluginError::Denied.into());
            }
            Ok(ActivationPolicy {
                developer_mode_enabled: true,
                signing_key_active: None,
            })
        }
        "builtin" => Ok(ActivationPolicy {
            developer_mode_enabled: dev_mode,
            signing_key_active: None,
        }),
        _ => Err(PluginError::Denied.into()),
    }
}

fn validate_enable_operation(
    app: &App<'_>,
    id: &str,
    operation: uuid::Uuid,
) -> Result<(), StoreError> {
    if super::staged_operations::load_intent(app, operation)?.is_some() {
        return Err(PluginError::Conflict.into());
    }
    if let Some(owner) = super::operation_rows::get(app, operation)? {
        let command = super::operation_rows::command(app, operation)?;
        if owner.plugin_id.as_deref() != Some(id)
            || owner.generation.is_some()
            || command.class != "management"
            || owner.state.is_terminal()
        {
            return Err(PluginError::Conflict.into());
        }
        if owner.state == PluginOperationState::Unknown
            || command.effect == PluginEffectObservation::Unknown
        {
            return Err(PluginError::UnknownEffect.into());
        }
        if command.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
    }
    Ok(())
}

impl App<'_> {
    pub fn begin_plugin_lifecycle(
        &self,
        id: &str,
        revision: i64,
        operation: uuid::Uuid,
        enabled: bool,
    ) -> Result<PluginRecord, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        let mut staged_drain = false;
        if let Some(owner) = super::operation_rows::get(self, operation)? {
            let command = super::operation_rows::command(self, operation)?;
            if owner.plugin_id.as_deref() != Some(id)
                || owner.generation.is_some()
                || command.class != "management"
                || owner.state.is_terminal()
            {
                return Err(PluginError::Conflict.into());
            }
            if owner.state == PluginOperationState::Unknown
                || command.effect == PluginEffectObservation::Unknown
            {
                return Err(PluginError::UnknownEffect.into());
            }
            if command.effect == PluginEffectObservation::Confirmed {
                return Err(PluginError::EffectDispositionRequired.into());
            }
            if !enabled
                && p.active_operation_id == Some(operation)
                && super::staged_operations::load_intent(self, operation)?
                    .is_some_and(|i| i.target_plugin_id == id)
                && matches!(owner.kind.as_str(), "upgrade" | "rollback" | "uninstall")
            {
                staged_drain = true;
            }
        }
        if p.active_operation_id.is_some() && !staged_drain {
            return Err(PluginError::Conflict.into());
        }
        if enabled {
            super::config::drained(&p)?;
        } else if p.lifecycle != PluginLifecycle::Enabled {
            return Err(PluginError::Conflict.into());
        }
        let lifecycle = if enabled { "enabling" } else { "draining" };
        tx.execute("UPDATE plugins SET lifecycle=?2,desired_enabled=?3,revision=revision+1,generation=generation+1,active_operation_id=?4,failure_reason=NULL WHERE plugin_id=?1",params![id,lifecycle,enabled,operation.to_string()])?;
        if !enabled {
            super::operations::cancel_pending(self, id)?;
            super::tasks::disable(self, id, p.generation + 1)?;
        }
        let updated = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
    pub fn finalize_plugin_lifecycle(
        &self,
        observed: &PluginLifecycleObservation,
    ) -> Result<PluginRecord, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, &observed.plugin_id, observed.revision)?;
        if p.generation != observed.generation
            || p.active_operation_id != Some(observed.operation_id)
            || !matches!(
                p.lifecycle,
                PluginLifecycle::Enabling | PluginLifecycle::Draining
            )
        {
            return Err(PluginError::Conflict.into());
        }
        let staged = super::staged_operations::load_intent(self, observed.operation_id)?.is_some();
        if staged && p.desired_enabled {
            return Err(PluginError::Conflict.into());
        }
        let unresolved = if staged {
            super::operations::has_unresolved_excluding(
                self,
                &p.plugin_id,
                Some(observed.operation_id),
            )?
        } else {
            super::operations::has_unresolved(self, &p.plugin_id)?
        };
        if !observed.workers_exited || !observed.unknown_effects_reconciled || unresolved {
            return Err(PluginError::NotDrained.into());
        }
        let op_active = staged
            && super::operation_rows::get(self, observed.operation_id)?
                .is_some_and(|op| !op.state.is_terminal());
        let enabled = p.desired_enabled && observed.validation_succeeded;
        let reason =
            (p.desired_enabled && !observed.validation_succeeded).then_some("validation-rejected");
        let next_active_op = (op_active && !enabled).then(|| observed.operation_id.to_string());
        tx.execute("UPDATE plugins SET lifecycle=?2,desired_enabled=?3,revision=revision+1,active_operation_id=?4,failure_reason=?5 WHERE plugin_id=?1",params![p.plugin_id,if enabled {"enabled"} else {"disabled"},enabled,next_active_op,reason])?;
        if staged && !op_active {
            super::staged_operations::release_locks(self, observed.operation_id)?;
        }
        let updated = self
            .get_plugin(&p.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn begin_plugin_enable_checked(
        &self,
        id: &str,
        revision: i64,
        operation: uuid::Uuid,
    ) -> Result<PluginActivationSnapshot, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        validate_enable_operation(self, id, operation)?;
        if let Some(active) = p.active_operation_id
            && active != operation
        {
            return Err(PluginError::OperationActive(active).into());
        }
        if !matches!(
            p.lifecycle,
            PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
        ) || p.desired_enabled
        {
            return Err(PluginError::NotDrained.into());
        }
        let version = self
            .get_plugin_version(id, &p.package_hash)?
            .ok_or(PluginError::NotFound)?;
        let policy = check_activation_policy(self, &version)?;
        tx.execute(
            "UPDATE plugins SET lifecycle='enabling',desired_enabled=1,revision=revision+1,generation=generation+1,active_operation_id=?2,failure_reason=NULL WHERE plugin_id=?1",
            params![id, operation.to_string()],
        )?;
        let plugin = self.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        let config = self.get_plugin_config(id)?;
        let grants = self.get_plugin_grants(id)?;
        tx.commit()?;
        Ok(PluginActivationSnapshot {
            plugin,
            version,
            config,
            grants,
            developer_mode_enabled: policy.developer_mode_enabled,
            signing_key_active: policy.signing_key_active,
        })
    }

    pub fn finalize_plugin_lifecycle_checked(
        &self,
        observed: &PluginLifecycleObservation,
        failure_reason: Option<&str>,
    ) -> Result<PluginRecord, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, &observed.plugin_id, observed.revision)?;
        if p.generation != observed.generation
            || p.active_operation_id != Some(observed.operation_id)
            || !matches!(
                p.lifecycle,
                PluginLifecycle::Enabling | PluginLifecycle::Draining
            )
        {
            return Err(PluginError::Conflict.into());
        }
        let staged = super::staged_operations::load_intent(self, observed.operation_id)?.is_some();
        if staged && p.desired_enabled {
            return Err(PluginError::Conflict.into());
        }
        let unresolved = if staged {
            super::operations::has_unresolved_excluding(
                self,
                &p.plugin_id,
                Some(observed.operation_id),
            )?
        } else {
            super::operations::has_unresolved(self, &p.plugin_id)?
        };
        if !observed.workers_exited || !observed.unknown_effects_reconciled || unresolved {
            return Err(PluginError::NotDrained.into());
        }
        let op_active = staged
            && super::operation_rows::get(self, observed.operation_id)?
                .is_some_and(|op| !op.state.is_terminal());
        let (enabled, reason): (bool, Option<&str>) = if p.desired_enabled {
            if !observed.validation_succeeded {
                (false, Some(failure_reason.unwrap_or("validation-rejected")))
            } else {
                let version = self
                    .get_plugin_version(&p.plugin_id, &p.package_hash)?
                    .ok_or(PluginError::NotFound)?;
                match check_activation_policy(self, &version) {
                    Ok(_) => (true, None),
                    Err(_) => (
                        false,
                        Some(
                            failure_reason.unwrap_or(if version.signing_key_id.is_some() {
                                "signing-key-revoked"
                            } else {
                                "developer-mode-disabled"
                            }),
                        ),
                    ),
                }
            }
        } else {
            (false, failure_reason)
        };
        let next_active_op = (op_active && !enabled).then(|| observed.operation_id.to_string());
        tx.execute(
            "UPDATE plugins SET lifecycle=?2,desired_enabled=?3,revision=revision+1,active_operation_id=?4,failure_reason=?5 WHERE plugin_id=?1",
            params![
                p.plugin_id,
                if enabled { "enabled" } else { "disabled" },
                enabled,
                next_active_op,
                reason
            ],
        )?;
        if staged && !op_active {
            super::staged_operations::release_locks(self, observed.operation_id)?;
        }
        let updated = self
            .get_plugin(&p.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn list_active_plugins_for_trust_key(
        &self,
        key_id: &str,
    ) -> Result<Vec<PluginRecord>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugins p JOIN plugin_versions v ON v.plugin_id=p.plugin_id AND v.package_hash=p.active_package_hash WHERE p.lifecycle IN ('enabling','enabled') AND v.signing_key_id=?1 ORDER BY p.plugin_id",
            super::records::COLUMNS
                .split(',')
                .map(|c| format!("p.{c}"))
                .collect::<Vec<_>>()
                .join(",")
        ))?;
        let items = stmt
            .query_map([key_id], super::records::plugin_row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(items)
    }

    pub fn list_active_unsigned_plugins(&self) -> Result<Vec<PluginRecord>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugins p JOIN plugin_versions v ON v.plugin_id=p.plugin_id AND v.package_hash=p.active_package_hash WHERE p.lifecycle IN ('enabling','enabled') AND v.signature_status IN ('unsigned','development') ORDER BY p.plugin_id",
            super::records::COLUMNS
                .split(',')
                .map(|c| format!("p.{c}"))
                .collect::<Vec<_>>()
                .join(",")
        ))?;
        let items = stmt
            .query_map([], super::records::plugin_row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        tx.commit()?;
        Ok(items)
    }

    pub fn uninstall_plugin(
        &self,
        id: &str,
        revision: i64,
        operation: Option<uuid::Uuid>,
        remove_private_data: bool,
    ) -> Result<UninstalledPlugin, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let p = super::config::current(self, id, revision)?;
        if !matches!(
            p.lifecycle,
            PluginLifecycle::InstalledDisabled | PluginLifecycle::Disabled
        ) || p.desired_enabled
        {
            return Err(PluginError::NotDrained.into());
        }
        if let Some(active) = p.active_operation_id {
            if operation != Some(active) {
                return Err(if operation.is_some() {
                    PluginError::OperationActive(active).into()
                } else {
                    PluginError::NotDrained.into()
                });
            }
        } else if operation.is_some() {
            return Err(PluginError::Conflict.into());
        }
        if super::operations::has_unresolved(self, id)? {
            return Err(PluginError::NotDrained.into());
        }
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM plugin_task_bindings WHERE plugin_id=?1 ORDER BY binding_id",
            super::tasks::COLUMNS
        ))?;
        let removed_task_bindings = stmt
            .query_map([id], super::tasks::row)?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut hstmt =
            tx.prepare("SELECT package_hash FROM plugin_versions WHERE plugin_id=?1")?;
        let mut candidates: std::collections::BTreeSet<String> = hstmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        drop(hstmt);
        candidates.insert(p.package_hash);

        for table in [
            "plugin_grants",
            "plugin_task_bindings",
            "plugin_configs",
            "plugin_config_secrets",
            "plugin_ui_trust",
            "plugin_versions",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE plugin_id=?1"), [id])?;
        }
        if remove_private_data {
            for table in ["plugin_kv", "plugin_blobs", "plugin_quota"] {
                tx.execute(&format!("DELETE FROM {table} WHERE plugin_id=?1"), [id])?;
            }
            let plugin_scope = serde_json::to_string(&Some(id))?;
            for table in ["plugin_command_payloads", "plugin_operation_results"] {
                tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE operation_id IN \
                         (SELECT operation_id FROM plugin_operations WHERE plugin_id=?1 OR plugin_scope=?2)"
                    ),
                    params![id, plugin_scope],
                )?;
            }
        }
        tx.execute(
            "UPDATE plugin_operations SET plugin_id=NULL WHERE plugin_id=?1",
            [id],
        )?;
        let floor_key = super::records::generation_floor_key(id);
        let prev_floor = super::records::load_generation_floor(self, id)?;
        let next_floor = prev_floor.max(p.generation.saturating_add(1));
        self.put_setting(&floor_key, &next_floor.to_string())?;
        tx.execute("DELETE FROM plugins WHERE plugin_id=?1", [id])?;

        let referenced = self.referenced_plugin_package_hashes()?;
        let unreferenced_package_hashes = candidates
            .into_iter()
            .filter(|h| !referenced.contains(h))
            .collect();
        tx.commit()?;
        Ok(UninstalledPlugin {
            plugin_id: id.into(),
            removed_task_bindings,
            unreferenced_package_hashes,
            private_data_removed: remove_private_data,
        })
    }
}
impl Store {
    pub fn begin_plugin_lifecycle(
        &self,
        id: &str,
        revision: i64,
        operation: uuid::Uuid,
        enabled: bool,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.begin_plugin_lifecycle(id, revision, operation, enabled))
    }
    pub fn finalize_plugin_lifecycle(
        &self,
        observed: &PluginLifecycleObservation,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.finalize_plugin_lifecycle(observed))
    }
    pub fn begin_plugin_enable_checked(
        &self,
        id: &str,
        revision: i64,
        operation: uuid::Uuid,
    ) -> Result<PluginActivationSnapshot, StoreError> {
        self.with_app(|app| app.begin_plugin_enable_checked(id, revision, operation))
    }
    pub fn finalize_plugin_lifecycle_checked(
        &self,
        observed: &PluginLifecycleObservation,
        failure_reason: Option<&str>,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.finalize_plugin_lifecycle_checked(observed, failure_reason))
    }
    pub fn list_active_plugins_for_trust_key(
        &self,
        key_id: &str,
    ) -> Result<Vec<PluginRecord>, StoreError> {
        self.with_app_read(|app| app.list_active_plugins_for_trust_key(key_id))
    }
    pub fn list_active_unsigned_plugins(&self) -> Result<Vec<PluginRecord>, StoreError> {
        self.with_app_read(|app| app.list_active_unsigned_plugins())
    }
    pub fn uninstall_plugin(
        &self,
        id: &str,
        revision: i64,
        operation: Option<uuid::Uuid>,
        remove_private_data: bool,
    ) -> Result<UninstalledPlugin, StoreError> {
        self.with_app(|app| app.uninstall_plugin(id, revision, operation, remove_private_data))
    }
}
