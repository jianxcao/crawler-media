use super::operation_rows;
use super::staged_operations::{
    AcceptStagedPluginOperation, AcceptedStagedPluginOperation, active_install_lock_op,
    check_version_conflict, intent_key, load_intent, lock_key,
};
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

fn validate_staged_accept(input: &AcceptStagedPluginOperation) -> Result<(), StoreError> {
    if matches!(input.subject, PluginSubject::Task { .. }) {
        return Err(PluginError::Denied.into());
    }
    if input.idempotency_key.is_empty()
        || input.idempotency_key.len() > 256
        || input.intent.target_plugin_id.is_empty()
        || input.intent.target_plugin_id.len() > 256
        || input.request.method.is_empty()
        || !input
            .request
            .method
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-')
        || !input.request.path.starts_with('/')
    {
        return Err(PluginError::Invalid("invalid staged operation request").into());
    }
    if input.op_kind.requires_package_hash()
        && input
            .intent
            .package_hash
            .as_deref()
            .is_none_or(|h| h.is_empty() || h.len() > 256)
    {
        return Err(PluginError::Invalid("staged package operation requires package_hash").into());
    }
    super::operations::validate_cause(&input.cause, false)?;
    super::grants::validate_grants(&input.intent.grants)
}

fn replay_staged(
    app: &App<'_>,
    input: &AcceptStagedPluginOperation,
    subject: &str,
    plugin_scope: &str,
) -> Result<Option<AcceptedStagedPluginOperation>, StoreError> {
    let prior: Option<(String, String, String, Vec<u8>, String)> = app
        .app
        .query_row(
            "SELECT operation_id,canonical_method,canonical_path,body_digest,kind FROM plugin_operations WHERE subject=?1 AND plugin_scope=?2 AND idempotency_key=?3",
            params![subject, plugin_scope, input.idempotency_key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((id, method, path, body, kind)) = prior else {
        if super::idempotency::has_operation_for_subject_key(app, subject, &input.idempotency_key)?
            || super::idempotency::has_idempotency_envelope(
                app,
                &input.subject,
                &input.idempotency_key,
            )?
        {
            return Err(PluginError::IdempotencyConflict.into());
        }
        return Ok(None);
    };
    if method != input.request.method
        || path != input.request.path
        || body != input.request.body_digest
        || kind != input.op_kind.as_str()
    {
        return Err(PluginError::IdempotencyConflict.into());
    }
    let id = uuid::Uuid::parse_str(&id)?;
    let cmd = operation_rows::command(app, id)?;
    if cmd.class != "management" || cmd.cause.cause_id != input.cause.cause_id {
        return Err(PluginError::IdempotencyConflict.into());
    }
    let operation = operation_rows::get(app, id)?.ok_or(PluginError::NotFound)?;
    let intent = load_intent(app, id)?.ok_or(PluginError::IdempotencyConflict)?;
    Ok(Some(AcceptedStagedPluginOperation {
        operation,
        intent,
        replayed: true,
    }))
}

fn verify_accept_preconditions(
    app: &App<'_>,
    input: &AcceptStagedPluginOperation,
) -> Result<Option<PluginRecord>, StoreError> {
    let id = &input.intent.target_plugin_id;
    let existing = app.get_plugin(id)?;
    if input.op_kind.is_new_install() {
        if let Some(p) = existing {
            if let Some(active) = p.active_operation_id {
                return Err(PluginError::OperationActive(active).into());
            }
            check_version_conflict(
                app,
                id,
                input.intent.version.as_deref(),
                input.intent.package_hash.as_deref(),
            )?;
            return Err(PluginError::Conflict.into());
        }
        if input.expected_revision != 0 {
            return Err(PluginError::Conflict.into());
        }
        if let Some(active) = active_install_lock_op(app, id)? {
            return Err(PluginError::OperationActive(active.operation_id).into());
        }
        return Ok(None);
    }

    let p = existing.ok_or(PluginError::NotFound)?;
    if let Some(active) = p.active_operation_id {
        return Err(PluginError::OperationActive(active).into());
    }
    if p.revision != input.expected_revision || p.revision == i64::MAX {
        return Err(PluginError::Conflict.into());
    }
    if matches!(
        input.op_kind,
        StagedPluginOpKind::Uninstall | StagedPluginOpKind::StateReset
    ) {
        super::config::drained(&p)?;
        if super::operations::has_unresolved(app, id)? {
            return Err(PluginError::NotDrained.into());
        }
    }
    if matches!(
        input.op_kind,
        StagedPluginOpKind::UpgradePrepare
            | StagedPluginOpKind::Upgrade
            | StagedPluginOpKind::Rollback
    ) {
        check_version_conflict(
            app,
            id,
            input.intent.version.as_deref(),
            input.intent.package_hash.as_deref(),
        )?;
    }
    Ok(Some(p))
}

impl App<'_> {
    pub fn begin_staged_plugin_operation(
        &self,
        input: &AcceptStagedPluginOperation,
    ) -> Result<AcceptedStagedPluginOperation, StoreError> {
        validate_staged_accept(input)?;
        let subject = serde_json::to_string(&input.subject)?;
        let plugin_scope = serde_json::to_string(&Some(&input.intent.target_plugin_id))?;
        let tx = crate::write_tx(self.app)?;
        if let Some(replayed) = replay_staged(self, input, &subject, &plugin_scope)? {
            tx.commit()?;
            return Ok(replayed);
        }
        let existing = verify_accept_preconditions(self, input)?;
        let op_id = uuid::Uuid::new_v4();
        let now = crate::unix_now();
        let plugin_id_col = existing.as_ref().map(|p| p.plugin_id.as_str());
        tx.execute(
            "INSERT INTO plugin_operations VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,NULL,'queued','accepted',NULL,?10,?10)",
            params![
                op_id.to_string(),
                plugin_id_col,
                plugin_scope,
                subject,
                input.idempotency_key,
                input.op_kind.as_str(),
                input.request.method,
                input.request.path,
                input.request.body_digest.as_slice(),
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO plugin_commands VALUES (?1,'management',?2,?3,NULL,NULL,'[]',?4,0,'[]',1,0,NULL,'none')",
            params![
                op_id.to_string(),
                input.intent.package_hash.as_deref(),
                input.expected_revision,
                &input.cause.cause_id,
            ],
        )?;
        self.put_setting(&intent_key(op_id), &serde_json::to_string(&input.intent)?)?;
        if let Some(p) = existing {
            tx.execute(
                "UPDATE plugins SET active_operation_id=?2 WHERE plugin_id=?1",
                params![p.plugin_id, op_id.to_string()],
            )?;
        } else {
            self.put_setting(
                &lock_key(&input.intent.target_plugin_id),
                &op_id.to_string(),
            )?;
        }
        let operation = operation_rows::get(self, op_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(AcceptedStagedPluginOperation {
            operation,
            intent: input.intent.clone(),
            replayed: false,
        })
    }

    pub fn claim_staged_plugin_operation(
        &self,
        operation_id: uuid::Uuid,
        revision: i64,
        attempt: i64,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        let tx = crate::write_tx(self.app)?;
        if load_intent(self, operation_id)?.is_none() {
            return Err(PluginError::Invalid("operation is not a staged plugin operation").into());
        }
        let claim = self.claim_plugin_command(operation_id, revision, None, attempt, now)?;
        tx.commit()?;
        Ok(claim)
    }

    pub fn recover_staged_plugin_operation(
        &self,
        operation_id: uuid::Uuid,
        revision: i64,
        attempt: i64,
        resume_phase: &str,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        if !super::commands::valid_phase(resume_phase) {
            return Err(PluginError::Invalid("invalid resume phase").into());
        }
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, operation_id)?.ok_or(PluginError::NotFound)?;
        let cmd = operation_rows::command(self, operation_id)?;
        if cmd.class != "management" || load_intent(self, operation_id)?.is_none() {
            return Err(PluginError::Invalid("operation is not a staged plugin operation").into());
        }
        if op.phase == "cancel-requested" {
            return Err(if op.state == PluginOperationState::Unknown
                || cmd.effect == PluginEffectObservation::Unknown
            {
                PluginError::UnknownEffect
            } else {
                PluginError::Conflict
            }
            .into());
        }
        if cmd.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if op.revision != revision
            || cmd.attempt != attempt
            || !matches!(
                op.state,
                PluginOperationState::Running
                    | PluginOperationState::Unknown
                    | PluginOperationState::Blocked
            )
            || op.revision == i64::MAX
            || cmd.revision == i64::MAX
            || cmd.attempt == i64::MAX
        {
            return Err(PluginError::Conflict.into());
        }
        let nonce = uuid::Uuid::new_v4();
        tx.execute(
            "UPDATE plugin_operations SET state='running',phase=?2,revision=revision+1,next_run_at=NULL,updated_at=?3 WHERE operation_id=?1",
            params![operation_id.to_string(), resume_phase, now],
        )?;
        tx.execute(
            "UPDATE plugin_commands SET revision=revision+1,attempt=attempt+1,claim_nonce=?2,effect='none' WHERE operation_id=?1",
            params![operation_id.to_string(), nonce.to_string()],
        )?;
        tx.commit()?;
        Ok(PluginCommandClaim {
            operation_id,
            revision: op.revision + 1,
            command_revision: cmd.revision + 1,
            attempt: cmd.attempt + 1,
            generation: op.generation,
            nonce,
        })
    }
}

impl Store {
    pub fn begin_staged_plugin_operation(
        &self,
        input: &AcceptStagedPluginOperation,
    ) -> Result<AcceptedStagedPluginOperation, StoreError> {
        self.with_app(|app| app.begin_staged_plugin_operation(input))
    }

    pub fn claim_staged_plugin_operation(
        &self,
        operation_id: uuid::Uuid,
        revision: i64,
        attempt: i64,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        self.with_app(|app| app.claim_staged_plugin_operation(operation_id, revision, attempt, now))
    }

    pub fn recover_staged_plugin_operation(
        &self,
        operation_id: uuid::Uuid,
        revision: i64,
        attempt: i64,
        resume_phase: &str,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        self.with_app(|app| {
            app.recover_staged_plugin_operation(operation_id, revision, attempt, resume_phase, now)
        })
    }
}
