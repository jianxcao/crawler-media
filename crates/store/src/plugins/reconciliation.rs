use super::operation_rows;
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;

fn update(
    app: &App<'_>,
    id: uuid::Uuid,
    state: PluginOperationState,
    phase: &str,
    effect: &str,
) -> Result<PluginOperation, StoreError> {
    if operation_rows::command(app, id)?.revision == i64::MAX {
        return Err(PluginError::Conflict.into());
    }
    app.app.execute("UPDATE plugin_operations SET state=?2,phase=?3,revision=revision+1,next_run_at=NULL,updated_at=?4 WHERE operation_id=?1",params![id.to_string(),operation_rows::state_name(state),phase,crate::unix_now()])?;
    app.app.execute("UPDATE plugin_commands SET revision=revision+1,claim_nonce=NULL,effect=?2 WHERE operation_id=?1",params![id.to_string(),effect])?;
    operation_rows::get(app, id)?.ok_or_else(|| PluginError::NotFound.into())
}

impl App<'_> {
    pub fn cancel_plugin_operation(
        &self,
        subject: &PluginSubject,
        id: uuid::Uuid,
        revision: i64,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = self
            .get_plugin_operation(subject, id)?
            .ok_or(PluginError::NotFound)?;
        if op.revision != revision || revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        if op.state == PluginOperationState::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if op.state.is_terminal() {
            return Err(PluginError::Conflict.into());
        }
        let command = operation_rows::command(self, id)?;
        if command.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if command.effect == PluginEffectObservation::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if super::staged_operations::is_active_lifecycle_op(self, &op)? {
            return Err(PluginError::Conflict.into());
        }
        let running = op.state == PluginOperationState::Running;
        let state = if running {
            PluginOperationState::Unknown
        } else {
            PluginOperationState::Cancelled
        };
        let result = update(
            self,
            id,
            state,
            "cancel-requested",
            if running { "unknown" } else { "none" },
        )?;
        if !running && super::staged_operations::load_intent(self, id)?.is_some() {
            super::staged_operations::release_locks(self, id)?;
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn resume_plugin_command(
        &self,
        subject: &PluginSubject,
        id: uuid::Uuid,
        revision: i64,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = self
            .get_plugin_operation(subject, id)?
            .ok_or(PluginError::NotFound)?;
        if op.state == PluginOperationState::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if op.revision != revision || revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        let cmd = operation_rows::command(self, id)?;
        if cmd.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if cmd.effect == PluginEffectObservation::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if !matches!(
            op.state,
            PluginOperationState::Blocked | PluginOperationState::Waiting
        ) {
            return Err(PluginError::Conflict.into());
        }
        super::commands::validate_command(self, &op, &cmd)?;
        let result = update(self, id, PluginOperationState::Queued, "resumed", "none")?;
        tx.commit()?;
        Ok(result)
    }
    /// Host recovery records loss of a running worker as uncertainty, never retry.
    pub fn record_plugin_command_interruption(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        let cmd = operation_rows::command(self, id)?;
        if op.revision != revision
            || op.generation != generation
            || cmd.attempt != attempt
            || op.state != PluginOperationState::Running
            || revision == i64::MAX
        {
            return Err(PluginError::Conflict.into());
        }
        if cmd.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        let result = update(
            self,
            id,
            PluginOperationState::Unknown,
            "worker-interrupted",
            "unknown",
        )?;
        tx.commit()?;
        Ok(result)
    }
    /// Host effect reconciliation only. It may settle an old generation while draining.
    pub fn reconcile_plugin_command(
        &self,
        observed: &PluginReconciliationObservation,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, observed.operation_id)?.ok_or(PluginError::NotFound)?;
        let cmd = operation_rows::command(self, observed.operation_id)?;
        if op.revision != observed.revision
            || op.generation != observed.generation
            || cmd.attempt != observed.attempt
            || op.revision == i64::MAX
        {
            return Err(PluginError::Conflict.into());
        }
        let unknown = op.state == PluginOperationState::Unknown
            && cmd.effect == PluginEffectObservation::Unknown;
        let confirmed = cmd.effect == PluginEffectObservation::Confirmed
            && matches!(
                op.state,
                PluginOperationState::Waiting
                    | PluginOperationState::Blocked
                    | PluginOperationState::Deferred
            );
        if !unknown && !confirmed {
            return Err(PluginError::Conflict.into());
        }
        if confirmed && observed.outcome == PluginReconciliationOutcome::ConfirmedNoEffect {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        let (state, effect) = match observed.outcome {
            PluginReconciliationOutcome::Succeeded => {
                (PluginOperationState::Succeeded, "confirmed")
            }
            PluginReconciliationOutcome::Failed => (PluginOperationState::Failed, "confirmed"),
            PluginReconciliationOutcome::ConfirmedNoEffect => {
                (PluginOperationState::Cancelled, "none")
            }
        };
        let result = update(self, observed.operation_id, state, "reconciled", effect)?;
        if super::staged_operations::load_intent(self, observed.operation_id)?.is_some() {
            super::staged_operations::release_locks(self, observed.operation_id)?;
        }
        tx.commit()?;
        Ok(result)
    }
}
impl Store {
    pub fn cancel_plugin_operation(
        &self,
        s: &PluginSubject,
        id: uuid::Uuid,
        revision: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.cancel_plugin_operation(s, id, revision))
    }
    pub fn resume_plugin_command(
        &self,
        s: &PluginSubject,
        id: uuid::Uuid,
        revision: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.resume_plugin_command(s, id, revision))
    }
    pub fn record_plugin_command_interruption(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| {
            app.record_plugin_command_interruption(id, revision, generation, attempt)
        })
    }
    pub fn reconcile_plugin_command(
        &self,
        o: &PluginReconciliationObservation,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.reconcile_plugin_command(o))
    }
}
