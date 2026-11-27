use super::operation_rows;
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

const INTENT_PREFIX: &str = "plugin_op.intent.";
const LOCK_PREFIX: &str = "plugin_op.install_lock.";
const RESULT_PREFIX: &str = "plugin_op.result.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StagedPluginOpKind {
    InstallPrepare,
    Install,
    UpgradePrepare,
    Upgrade,
    Rollback,
    Uninstall,
    StateReset,
}

impl StagedPluginOpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InstallPrepare => "install-prepare",
            Self::Install => "install",
            Self::UpgradePrepare => "upgrade-prepare",
            Self::Upgrade => "upgrade",
            Self::Rollback => "rollback",
            Self::Uninstall => "uninstall",
            Self::StateReset => "state-reset",
        }
    }
    pub(super) fn requires_package_hash(self) -> bool {
        matches!(
            self,
            Self::InstallPrepare
                | Self::Install
                | Self::UpgradePrepare
                | Self::Upgrade
                | Self::Rollback
        )
    }
    pub(super) fn is_new_install(self) -> bool {
        matches!(self, Self::InstallPrepare | Self::Install)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedPluginIntent {
    pub target_plugin_id: String,
    pub package_hash: Option<String>,
    pub version: Option<String>,
    pub artifact_path: Option<String>,
    pub state_policy: Option<String>,
    pub target_config_digest: Option<String>,
    pub grants: Vec<PluginGrant>,
}

#[derive(Clone, Debug)]
pub struct AcceptStagedPluginOperation {
    pub subject: PluginSubject,
    pub idempotency_key: String,
    pub request: CanonicalPluginRequest,
    pub op_kind: StagedPluginOpKind,
    pub expected_revision: i64,
    pub intent: StagedPluginIntent,
    pub cause: PluginCommandCause,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedStagedPluginOperation {
    pub operation: PluginOperation,
    pub intent: StagedPluginIntent,
    pub replayed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PluginAdminOperation {
    pub operation: PluginOperation,
    pub command_revision: i64,
    pub attempt: i64,
    pub intent: Option<StagedPluginIntent>,
    pub result_json: Option<serde_json::Value>,
}

pub(super) fn intent_key(op_id: uuid::Uuid) -> String {
    format!("{INTENT_PREFIX}{op_id}")
}

pub(super) fn lock_key(plugin_id: &str) -> String {
    format!("{LOCK_PREFIX}{plugin_id}")
}

pub(super) fn result_key(op_id: uuid::Uuid) -> String {
    format!("{RESULT_PREFIX}{op_id}")
}

pub(super) fn load_intent(
    app: &App<'_>,
    op_id: uuid::Uuid,
) -> Result<Option<StagedPluginIntent>, StoreError> {
    app.get_setting(&intent_key(op_id))?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn load_result(
    app: &App<'_>,
    op_id: uuid::Uuid,
) -> Result<Option<serde_json::Value>, StoreError> {
    app.get_setting(&result_key(op_id))?
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn active_install_lock_op(
    app: &App<'_>,
    plugin_id: &str,
) -> Result<Option<PluginOperation>, StoreError> {
    let Some(raw) = app.get_setting(&lock_key(plugin_id))? else {
        return Ok(None);
    };
    let Ok(op_id) = uuid::Uuid::parse_str(&raw) else {
        return Ok(None);
    };
    let Some(op) = operation_rows::get(app, op_id)? else {
        return Ok(None);
    };
    Ok((!op.state.is_terminal()).then_some(op))
}

pub(super) fn is_active_lifecycle_op(
    app: &App<'_>,
    op: &PluginOperation,
) -> Result<bool, StoreError> {
    let Some(pid) = op.plugin_id.as_deref() else {
        return Ok(false);
    };
    let Some(p) = app.get_plugin(pid)? else {
        return Ok(false);
    };
    Ok(p.active_operation_id == Some(op.operation_id)
        && matches!(
            p.lifecycle,
            PluginLifecycle::Enabling | PluginLifecycle::Draining
        ))
}

pub(super) fn release_locks(app: &App<'_>, op_id: uuid::Uuid) -> Result<(), StoreError> {
    let id_str = op_id.to_string();
    app.app.execute(
        "UPDATE plugins SET active_operation_id=NULL WHERE active_operation_id=?1 AND lifecycle NOT IN ('enabling','draining')",
        [&id_str],
    )?;
    if let Some(intent) = load_intent(app, op_id)? {
        let key = lock_key(&intent.target_plugin_id);
        if app.get_setting(&key)?.as_deref() == Some(id_str.as_str()) {
            app.delete_setting(&key)?;
        }
    }
    Ok(())
}

pub(super) fn check_version_conflict(
    app: &App<'_>,
    plugin_id: &str,
    version: Option<&str>,
    hash: Option<&str>,
) -> Result<(), StoreError> {
    let (Some(version), Some(hash)) = (version, hash) else {
        return Ok(());
    };
    let prior: Option<String> = app
        .app
        .query_row(
            "SELECT package_hash FROM plugin_versions WHERE plugin_id=?1 AND version=?2",
            params![plugin_id, version],
            |r| r.get(0),
        )
        .optional()?;
    if prior.as_deref().is_some_and(|h| h != hash) {
        return Err(PluginError::PackageConflict.into());
    }
    Ok(())
}

impl App<'_> {
    pub fn advance_staged_plugin_phase(
        &self,
        claim: &mut PluginCommandClaim,
        phase: &str,
        updated_intent: Option<&StagedPluginIntent>,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        if !super::commands::valid_phase(phase) {
            return Err(PluginError::Invalid("invalid staged phase").into());
        }
        let tx = crate::write_tx(self.app)?;
        let (op, cmd) = super::commands::claimed_unchecked(self, claim)?;
        if cmd.class != "management" || op.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        let current = load_intent(self, op.operation_id)?.ok_or(PluginError::Invalid(
            "operation is not a staged plugin operation",
        ))?;
        if let Some(intent) = updated_intent {
            if intent.target_plugin_id != current.target_plugin_id
                || intent.package_hash != current.package_hash
            {
                return Err(PluginError::Invalid("staged intent identity is immutable").into());
            }
            super::grants::validate_grants(&intent.grants)?;
            self.put_setting(
                &intent_key(op.operation_id),
                &serde_json::to_string(intent)?,
            )?;
        }
        tx.execute(
            "UPDATE plugin_operations SET phase=?2,revision=revision+1,updated_at=?3 WHERE operation_id=?1",
            params![op.operation_id.to_string(), phase, now],
        )?;
        let updated = operation_rows::get(self, op.operation_id)?.ok_or(PluginError::NotFound)?;
        claim.revision = updated.revision;
        tx.commit()?;
        Ok(updated)
    }

    pub fn register_staged_plugin_version(
        &self,
        claim: &mut PluginCommandClaim,
        version: &PluginVersionMetadata,
        grants: &[PluginGrant],
        now: i64,
    ) -> Result<PluginRecord, StoreError> {
        super::grants::validate_grants(grants)?;
        let tx = crate::write_tx(self.app)?;
        let (op, cmd) = super::commands::claimed_unchecked(self, claim)?;
        if cmd.class != "management" || op.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        let intent = load_intent(self, op.operation_id)?
            .ok_or(PluginError::Invalid("missing staged install intent"))?;
        if intent.target_plugin_id != version.plugin_id {
            return Err(PluginError::Invalid("staged version plugin_id mismatch").into());
        }
        if intent
            .package_hash
            .as_deref()
            .is_some_and(|h| h != version.package_hash)
        {
            return Err(PluginError::Invalid("staged version package_hash mismatch").into());
        }
        check_version_conflict(
            self,
            &version.plugin_id,
            Some(&version.version),
            Some(&version.package_hash),
        )?;
        super::lifecycle::check_activation_policy(self, version)?;
        if let Some(prior) = self.get_plugin_version(&version.plugin_id, &version.package_hash)? {
            if prior != *version {
                return Err(PluginError::PackageConflict.into());
            }
            let current = self
                .get_plugin(&version.plugin_id)?
                .ok_or(PluginError::NotFound)?;
            if current.active_operation_id != Some(op.operation_id) {
                return Err(PluginError::Conflict.into());
            }
        } else {
            let current = self.get_plugin(&version.plugin_id)?;
            if let Some(p) = &current {
                if p.active_operation_id != Some(op.operation_id) {
                    return Err(PluginError::Conflict.into());
                }
            } else {
                let lock_owned = self.get_setting(&lock_key(&version.plugin_id))?.as_deref()
                    == Some(op.operation_id.to_string().as_str());
                if !lock_owned {
                    return Err(PluginError::Conflict.into());
                }
                super::quota::purge_incompatible_domain_state(
                    &tx,
                    &version.plugin_id,
                    version.credential_bound,
                )?;
                let init_gen = super::records::load_generation_floor(self, &version.plugin_id)?;
                tx.execute(
                    "INSERT INTO plugins VALUES (?1,?2,'installed-disabled',0,1,0,?3,?4,NULL)",
                    params![
                        version.plugin_id,
                        version.package_hash,
                        init_gen,
                        op.operation_id.to_string()
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO plugin_versions VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    version.plugin_id,
                    version.package_hash,
                    version.version,
                    version.manifest_json,
                    version.config_schema_json,
                    version.state_schema,
                    version.artifact_path,
                    version.signing_key_id,
                    version.signature_status,
                    version.ui_metadata_ref,
                    version.credential_bound
                ],
            )?;
            for grant in grants {
                tx.execute(
                    "INSERT INTO plugin_grants VALUES (?1,?2,?3,?4,'granted',?5) ON CONFLICT(plugin_id,package_hash,capability,object_scope) DO UPDATE SET status='granted',sensitive=excluded.sensitive",
                    params![
                        version.plugin_id,
                        version.package_hash,
                        grant.capability,
                        serde_json::to_string(&grant.scope)?,
                        grant.sensitive
                    ],
                )?;
            }
        }
        tx.execute(
            "UPDATE plugin_operations SET plugin_id=?2,phase='registered',revision=revision+1,updated_at=?3 WHERE operation_id=?1",
            params![op.operation_id.to_string(), version.plugin_id, now],
        )?;
        claim.revision = op.revision + 1;
        let record = self
            .get_plugin(&version.plugin_id)?
            .ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn complete_staged_plugin_operation(
        &self,
        claim: &PluginCommandClaim,
        state: PluginOperationState,
        phase: &str,
        result_json: Option<&serde_json::Value>,
        failure_reason: Option<&str>,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        if !state.is_terminal() || !super::commands::valid_phase(phase) {
            return Err(PluginError::Invalid("invalid staged completion outcome").into());
        }
        let tx = crate::write_tx(self.app)?;
        let (op, cmd) = super::commands::claimed_unchecked(self, claim)?;
        if cmd.class != "management" || op.revision == i64::MAX || cmd.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        if is_active_lifecycle_op(self, &op)? {
            return Err(PluginError::NotDrained.into());
        }
        if let Some(val) = result_json {
            self.put_setting(&result_key(op.operation_id), &serde_json::to_string(val)?)?;
        }
        tx.execute(
            "UPDATE plugin_operations SET state=?2,phase=?3,revision=revision+1,next_run_at=NULL,updated_at=?4 WHERE operation_id=?1",
            params![
                op.operation_id.to_string(),
                operation_rows::state_name(state),
                phase,
                now
            ],
        )?;
        tx.execute(
            "UPDATE plugin_commands SET revision=revision+1,claim_nonce=NULL WHERE operation_id=?1",
            [op.operation_id.to_string()],
        )?;
        if let Some(reason) = failure_reason {
            tx.execute(
                "UPDATE plugins SET failure_reason=?2 WHERE active_operation_id=?1",
                params![op.operation_id.to_string(), reason],
            )?;
        }
        release_locks(self, op.operation_id)?;
        let updated = operation_rows::get(self, op.operation_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }

    pub fn cancel_plugin_operation_admin(
        &self,
        id: uuid::Uuid,
        revision: i64,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        if op.revision != revision || revision == i64::MAX || op.state.is_terminal() {
            return Err(PluginError::Conflict.into());
        }
        if op.state == PluginOperationState::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        let cmd = operation_rows::command(self, id)?;
        if cmd.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        if cmd.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if cmd.effect == PluginEffectObservation::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if is_active_lifecycle_op(self, &op)? {
            return Err(PluginError::Conflict.into());
        }
        let running = op.state == PluginOperationState::Running;
        let (state, effect) = if running {
            (PluginOperationState::Unknown, "unknown")
        } else {
            (PluginOperationState::Cancelled, "none")
        };
        tx.execute(
            "UPDATE plugin_operations SET state=?2,phase='cancel-requested',revision=revision+1,next_run_at=NULL,updated_at=?3 WHERE operation_id=?1",
            params![id.to_string(), operation_rows::state_name(state), now],
        )?;
        tx.execute(
            "UPDATE plugin_commands SET revision=revision+1,claim_nonce=NULL,effect=?2 WHERE operation_id=?1",
            params![id.to_string(), effect],
        )?;
        if !running {
            release_locks(self, id)?;
        }
        let updated = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}

impl Store {
    pub fn advance_staged_plugin_phase(
        &self,
        claim: &mut PluginCommandClaim,
        phase: &str,
        updated_intent: Option<&StagedPluginIntent>,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.advance_staged_plugin_phase(claim, phase, updated_intent, now))
    }

    pub fn register_staged_plugin_version(
        &self,
        claim: &mut PluginCommandClaim,
        version: &PluginVersionMetadata,
        grants: &[PluginGrant],
        now: i64,
    ) -> Result<PluginRecord, StoreError> {
        self.with_app(|app| app.register_staged_plugin_version(claim, version, grants, now))
    }

    pub fn complete_staged_plugin_operation(
        &self,
        claim: &PluginCommandClaim,
        state: PluginOperationState,
        phase: &str,
        result_json: Option<&serde_json::Value>,
        failure_reason: Option<&str>,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| {
            app.complete_staged_plugin_operation(
                claim,
                state,
                phase,
                result_json,
                failure_reason,
                now,
            )
        })
    }

    pub fn cancel_plugin_operation_admin(
        &self,
        id: uuid::Uuid,
        revision: i64,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.cancel_plugin_operation_admin(id, revision, now))
    }
}
