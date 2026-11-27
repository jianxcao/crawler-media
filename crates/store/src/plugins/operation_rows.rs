use super::*;
use crate::{App, StoreError};
use rusqlite::OptionalExtension;

pub(super) const COLUMNS: &str = "operation_id,plugin_id,kind,revision,generation,state,phase,next_run_at,created_at,updated_at,(SELECT effect FROM plugin_commands WHERE plugin_commands.operation_id=plugin_operations.operation_id)";
fn parse_effect(raw: &str) -> Option<PluginEffectObservation> {
    match raw {
        "none" => Some(PluginEffectObservation::NoEffect),
        "confirmed" => Some(PluginEffectObservation::Confirmed),
        "unknown" => Some(PluginEffectObservation::Unknown),
        _ => None,
    }
}
pub(super) fn state_name(state: PluginOperationState) -> &'static str {
    match state {
        PluginOperationState::Queued => "queued",
        PluginOperationState::Running => "running",
        PluginOperationState::Waiting => "waiting",
        PluginOperationState::Blocked => "blocked",
        PluginOperationState::Succeeded => "succeeded",
        PluginOperationState::Rejected => "rejected",
        PluginOperationState::Deferred => "deferred",
        PluginOperationState::Failed => "failed",
        PluginOperationState::Cancelled => "cancelled",
        PluginOperationState::Unknown => "unknown",
    }
}
pub(super) fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PluginOperation> {
    let id: String = r.get(0)?;
    let state: String = r.get(5)?;
    let effect: String = r.get(10)?;
    let state = match state.as_str() {
        "queued" => PluginOperationState::Queued,
        "running" => PluginOperationState::Running,
        "waiting" => PluginOperationState::Waiting,
        "blocked" => PluginOperationState::Blocked,
        "succeeded" => PluginOperationState::Succeeded,
        "rejected" => PluginOperationState::Rejected,
        "deferred" => PluginOperationState::Deferred,
        "failed" => PluginOperationState::Failed,
        "cancelled" => PluginOperationState::Cancelled,
        "unknown" => PluginOperationState::Unknown,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let operation_id = uuid::Uuid::parse_str(&id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(PluginOperation {
        operation_id,
        plugin_id: r.get(1)?,
        kind: r.get(2)?,
        revision: r.get(3)?,
        generation: r.get(4)?,
        state,
        phase: r.get(6)?,
        next_run_at: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        effect: parse_effect(&effect).ok_or(rusqlite::Error::InvalidQuery)?,
    })
}
pub(super) fn get(app: &App<'_>, id: uuid::Uuid) -> Result<Option<PluginOperation>, StoreError> {
    app.app
        .query_row(
            &format!("SELECT {COLUMNS} FROM plugin_operations WHERE operation_id=?1"),
            [id.to_string()],
            row,
        )
        .optional()
        .map_err(Into::into)
}

pub(super) struct Command {
    pub class: String,
    pub hash: Option<String>,
    pub scope: Option<PluginSecurityScope>,
    pub revision: i64,
    pub attempt: i64,
    pub nonce: Option<String>,
    pub authority_generation: Option<i64>,
    pub expected_revision: Option<i64>,
    pub targets: Vec<PluginGrantScope>,
    pub cause: PluginCommandCause,
    pub effect: PluginEffectObservation,
}
pub(super) fn command(app: &App<'_>, id: uuid::Uuid) -> Result<Command, StoreError> {
    let (
        class,
        hash,
        scope,
        revision,
        attempt,
        nonce,
        authority_generation,
        expected_revision,
        targets,
        cause_id,
        cause_depth,
        cause_visited,
        effect,
    ): (
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<i64>,
        Option<i64>,
        String,
        String,
        i64,
        String,
        String,
    ) = app
        .app
        .query_row(
            "SELECT command_class,package_hash,security_scope,revision,attempt,claim_nonce,authority_generation,expected_revision,target_scopes,cause_id,cause_depth,cause_visited,effect FROM plugin_commands WHERE operation_id=?1",
            [id.to_string()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .optional()?
        .ok_or(PluginError::NotFound)?;
    let depth =
        u8::try_from(cause_depth).map_err(|_| PluginError::Invalid("invalid cause depth"))?;
    Ok(Command {
        class,
        hash,
        scope: scope
            .map(|s| PluginSecurityScope::from_key(&s))
            .transpose()?,
        revision,
        attempt,
        nonce,
        authority_generation,
        expected_revision,
        targets: serde_json::from_str(&targets)?,
        cause: PluginCommandCause {
            cause_id,
            depth,
            visited: serde_json::from_str(&cause_visited)?,
        },
        effect: parse_effect(&effect)
            .ok_or(PluginError::Invalid("invalid stored command effect"))?,
    })
}
