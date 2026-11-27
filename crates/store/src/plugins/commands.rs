use super::operation_rows::{self, Command};
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;

pub(super) fn validate_command(
    app: &App<'_>,
    op: &PluginOperation,
    command: &Command,
) -> Result<(), StoreError> {
    if let Some(scope) = &command.scope {
        let binding = super::authority::Binding {
            plugin_id: op.plugin_id.clone().ok_or(PluginError::Denied)?,
            hash: command.hash.clone().ok_or(PluginError::Denied)?,
            generation: command.authority_generation.ok_or(PluginError::Denied)?,
            revision: command.expected_revision.ok_or(PluginError::Denied)?,
            scope: scope.clone(),
        };
        let runtime = command.class == "runtime";
        if runtime && op.generation != Some(binding.generation) {
            return Err(PluginError::StaleAuthority.into());
        }
        if runtime {
            super::command_authority::validate_targets(app, &binding, &op.kind, &command.targets)?;
        } else {
            super::authority::validate_config(app, &binding)?;
        }
    } else if command.class == "runtime" {
        return Err(PluginError::Denied.into());
    }
    Ok(())
}

pub(super) fn claimed_unchecked(
    app: &App<'_>,
    claim: &PluginCommandClaim,
) -> Result<(PluginOperation, Command), StoreError> {
    let op = operation_rows::get(app, claim.operation_id)?.ok_or(PluginError::NotFound)?;
    let cmd = operation_rows::command(app, claim.operation_id)?;
    if op.revision != claim.revision
        || op.generation != claim.generation
        || op.state != PluginOperationState::Running
        || cmd.revision != claim.command_revision
        || cmd.attempt != claim.attempt
        || cmd.nonce.as_deref() != Some(claim.nonce.to_string().as_str())
    {
        return Err(PluginError::Conflict.into());
    }
    Ok((op, cmd))
}

pub(super) fn claimed(
    app: &App<'_>,
    claim: &PluginCommandClaim,
) -> Result<(PluginOperation, Command), StoreError> {
    let (op, cmd) = claimed_unchecked(app, claim)?;
    validate_command(app, &op, &cmd)?;
    Ok((op, cmd))
}

impl App<'_> {
    pub fn plugin_command_status(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<PluginPendingCommand>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let Some(operation) = operation_rows::get(self, id)? else {
            tx.commit()?;
            return Ok(None);
        };
        let command = operation_rows::command(self, id)?;
        let cursor = PluginCommandCursor {
            next_run_at: operation.next_run_at.unwrap_or(0),
            operation_id: id,
        };
        tx.commit()?;
        Ok(Some(PluginPendingCommand {
            operation,
            command_revision: command.revision,
            attempt: command.attempt,
            targets: command.targets,
            cursor,
        }))
    }
    pub fn reject_stale_plugin_command(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        let cmd = operation_rows::command(self, id)?;
        if op.revision != revision
            || op.generation != generation
            || cmd.attempt != attempt
            || !matches!(
                op.state,
                PluginOperationState::Queued | PluginOperationState::Deferred
            )
            || cmd.effect != PluginEffectObservation::NoEffect
            || revision == i64::MAX
            || cmd.revision == i64::MAX
        {
            return Err(PluginError::Conflict.into());
        }
        match validate_command(self, &op, &cmd) {
            Err(StoreError::Plugin(PluginError::StaleAuthority | PluginError::Denied)) => {}
            Ok(()) => return Err(PluginError::Conflict.into()),
            Err(other) => return Err(other),
        }
        tx.execute("UPDATE plugin_operations SET state='rejected',phase='authority-rejected',revision=revision+1,next_run_at=NULL,updated_at=?2 WHERE operation_id=?1", params![id.to_string(), now])?;
        tx.execute("UPDATE plugin_commands SET revision=revision+1,claim_nonce=NULL,effect='none' WHERE operation_id=?1", params![id.to_string()])?;
        let updated = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
    /// Host startup/reconciliation discovery includes running and unknown effects.
    pub fn unresolved_plugin_commands(
        &self,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginPendingCommand>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt=tx.prepare("SELECT operation_id FROM plugin_operations WHERE state NOT IN ('succeeded','rejected','failed','cancelled') AND operation_id>?1 ORDER BY operation_id LIMIT ?2")?;
        let ids = stmt
            .query_map(
                params![
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    super::paging::limit(count) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            let id = uuid::Uuid::parse_str(&id)?;
            let operation = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
            let command = operation_rows::command(self, id)?;
            let cursor = PluginCommandCursor {
                next_run_at: operation.next_run_at.unwrap_or(0),
                operation_id: id,
            };
            result.push(PluginPendingCommand {
                operation,
                command_revision: command.revision,
                attempt: command.attempt,
                targets: command.targets,
                cursor,
            });
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn pending_plugin_commands(
        &self,
        after: Option<&PluginCommandCursor>,
        count: Option<usize>,
        now: i64,
    ) -> Result<Vec<PluginPendingCommand>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let mut stmt=tx.prepare("SELECT operation_id FROM plugin_operations WHERE state IN ('queued','deferred') AND COALESCE(next_run_at,0)<=?1 AND (COALESCE(next_run_at,0),operation_id)>(?2,?3) ORDER BY COALESCE(next_run_at,0),operation_id LIMIT ?4")?;
        let ids = stmt
            .query_map(
                params![
                    now,
                    after.map_or(-1, |c| c.next_run_at),
                    after
                        .map(|c| c.operation_id.to_string())
                        .unwrap_or_default(),
                    super::paging::limit(count) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            let id = uuid::Uuid::parse_str(&id)?;
            let operation = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
            let cmd = operation_rows::command(self, id)?;
            let cursor = PluginCommandCursor {
                next_run_at: operation.next_run_at.unwrap_or(0),
                operation_id: id,
            };
            result.push(PluginPendingCommand {
                operation,
                command_revision: cmd.revision,
                attempt: cmd.attempt,
                targets: cmd.targets,
                cursor,
            });
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn claim_plugin_command(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let op = operation_rows::get(self, id)?.ok_or(PluginError::NotFound)?;
        let cmd = operation_rows::command(self, id)?;
        if op.revision != revision || op.generation != generation || cmd.attempt != attempt {
            return Err(PluginError::Conflict.into());
        }
        if cmd.effect == PluginEffectObservation::Confirmed {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if cmd.effect == PluginEffectObservation::Unknown {
            return Err(PluginError::UnknownEffect.into());
        }
        if !matches!(
            op.state,
            PluginOperationState::Queued | PluginOperationState::Deferred
        ) || op.next_run_at.is_some_and(|at| at > now)
            || revision == i64::MAX
            || attempt == i64::MAX
            || cmd.revision == i64::MAX
        {
            return Err(PluginError::Conflict.into());
        }
        validate_command(self, &op, &cmd)?;
        let nonce = uuid::Uuid::new_v4();
        tx.execute("UPDATE plugin_operations SET state='running',revision=revision+1,updated_at=?2,next_run_at=NULL WHERE operation_id=?1",params![id.to_string(),now])?;
        tx.execute("UPDATE plugin_commands SET revision=revision+1,attempt=attempt+1,claim_nonce=?2 WHERE operation_id=?1",params![id.to_string(),nonce.to_string()])?;
        tx.commit()?;
        Ok(PluginCommandClaim {
            operation_id: id,
            revision: revision + 1,
            command_revision: cmd.revision + 1,
            attempt: attempt + 1,
            generation,
            nonce,
        })
    }
    pub fn record_plugin_command_outcome(
        &self,
        claim: &PluginCommandClaim,
        outcome: &PluginCommandOutcome,
    ) -> Result<PluginOperation, StoreError> {
        validate_outcome(outcome)?;
        let tx = crate::write_tx(self.app)?;
        let stale_reject = outcome.state == PluginOperationState::Rejected
            && outcome.effect == PluginEffectObservation::NoEffect
            && outcome.result.is_none();
        let (op, cmd) = if stale_reject {
            claimed_unchecked(self, claim)?
        } else {
            claimed(self, claim)?
        };
        if cmd.effect != PluginEffectObservation::NoEffect {
            return Err(PluginError::EffectDispositionRequired.into());
        }
        if op.revision == i64::MAX || cmd.revision == i64::MAX {
            return Err(PluginError::Conflict.into());
        }
        if let Some(bytes) = &outcome.result {
            if let Some(scope) = &cmd.scope {
                let key = scope.key()?;
                tx.execute("INSERT INTO plugin_operation_results VALUES (?1,?2,?3) ON CONFLICT(operation_id) DO UPDATE SET security_scope=excluded.security_scope,result=excluded.result",params![op.operation_id.to_string(),key,bytes])?;
            } else if cmd.class == "management" {
                let json: serde_json::Value = serde_json::from_slice(bytes)?;
                self.put_setting(
                    &super::staged_operations::result_key(op.operation_id),
                    &serde_json::to_string(&json)?,
                )?;
            } else {
                return Err(PluginError::Denied.into());
            }
        }
        let effect = match outcome.effect {
            PluginEffectObservation::NoEffect => "none",
            PluginEffectObservation::Confirmed => "confirmed",
            PluginEffectObservation::Unknown => "unknown",
        };
        let disposition = outcome.effect == PluginEffectObservation::Confirmed
            && outcome.state == PluginOperationState::Deferred;
        let state = if disposition {
            PluginOperationState::Blocked
        } else {
            outcome.state
        };
        let phase = if disposition {
            "effect-disposition-required"
        } else {
            &outcome.phase
        };
        let next_run_at = if disposition {
            None
        } else {
            outcome.next_run_at
        };
        tx.execute("UPDATE plugin_operations SET state=?2,phase=?3,revision=revision+1,next_run_at=?4,updated_at=?5 WHERE operation_id=?1",params![op.operation_id.to_string(),operation_rows::state_name(state),phase,next_run_at,crate::unix_now()])?;
        tx.execute("UPDATE plugin_commands SET revision=revision+1,claim_nonce=NULL,effect=?2 WHERE operation_id=?1",params![op.operation_id.to_string(),effect])?;
        let updated = operation_rows::get(self, op.operation_id)?.ok_or(PluginError::NotFound)?;
        tx.commit()?;
        Ok(updated)
    }
}

pub(super) fn valid_phase(phase: &str) -> bool {
    !phase.is_empty()
        && phase.len() <= 128
        && phase
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:./".contains(&b))
}

fn validate_outcome(outcome: &PluginCommandOutcome) -> Result<(), StoreError> {
    let invalid_state = matches!(
        outcome.state,
        PluginOperationState::Queued | PluginOperationState::Running
    );
    let unknown = (outcome.effect == PluginEffectObservation::Unknown)
        != (outcome.state == PluginOperationState::Unknown);
    let unsafe_cancel = outcome.state == PluginOperationState::Cancelled
        && outcome.effect != PluginEffectObservation::NoEffect;
    if invalid_state
        || unknown
        || unsafe_cancel
        || (outcome.state == PluginOperationState::Deferred) != outcome.next_run_at.is_some()
        || outcome.next_run_at.is_some_and(|t| t < 0)
        || !valid_phase(&outcome.phase)
        || outcome.result.as_ref().is_some_and(|b| b.len() > 1048576)
    {
        return Err(PluginError::Invalid("invalid command outcome").into());
    }
    Ok(())
}

impl Store {
    pub fn plugin_command_status(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<PluginPendingCommand>, StoreError> {
        self.with_app_read(|app| app.plugin_command_status(id))
    }
    pub fn reject_stale_plugin_command(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
        now: i64,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.reject_stale_plugin_command(id, revision, generation, attempt, now))
    }
    pub fn unresolved_plugin_commands(
        &self,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginPendingCommand>, StoreError> {
        self.with_app_read(|app| app.unresolved_plugin_commands(after, count))
    }
    pub fn pending_plugin_commands(
        &self,
        after: Option<&PluginCommandCursor>,
        count: Option<usize>,
        now: i64,
    ) -> Result<Vec<PluginPendingCommand>, StoreError> {
        self.with_app_read(|app| app.pending_plugin_commands(after, count, now))
    }
    pub fn claim_plugin_command(
        &self,
        id: uuid::Uuid,
        revision: i64,
        generation: Option<i64>,
        attempt: i64,
        now: i64,
    ) -> Result<PluginCommandClaim, StoreError> {
        self.with_app(|app| app.claim_plugin_command(id, revision, generation, attempt, now))
    }
    pub fn record_plugin_command_outcome(
        &self,
        claim: &PluginCommandClaim,
        outcome: &PluginCommandOutcome,
    ) -> Result<PluginOperation, StoreError> {
        self.with_app(|app| app.record_plugin_command_outcome(claim, outcome))
    }
}
