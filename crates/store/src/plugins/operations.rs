use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

pub(super) fn has_unresolved(app: &App<'_>, id: &str) -> Result<bool, StoreError> {
    has_unresolved_excluding(app, id, None)
}
pub(super) fn has_unresolved_excluding(
    app: &App<'_>,
    id: &str,
    excluded: Option<uuid::Uuid>,
) -> Result<bool, StoreError> {
    let excluded = excluded.map(|u| u.to_string());
    Ok(app.app.query_row("SELECT EXISTS(SELECT 1 FROM plugin_operations o JOIN plugin_commands c ON c.operation_id=o.operation_id WHERE o.plugin_id=?1 AND (?2 IS NULL OR o.operation_id!=?2) AND o.state NOT IN ('succeeded','rejected','failed','cancelled') AND NOT (o.operation_id IS (SELECT active_operation_id FROM plugins WHERE plugin_id=?1) AND c.command_class='management' AND o.state!='unknown' AND c.effect='none'))",params![id,excluded],|r|r.get(0))?)
}
pub(super) fn ensure_lifecycle_op(
    app: &App<'_>,
    id: &str,
    operation: uuid::Uuid,
) -> Result<(), StoreError> {
    let owner = super::operation_rows::get(app, operation)?.ok_or(PluginError::NotFound)?;
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
    Ok(())
}
pub(super) fn cancel_pending(app: &App<'_>, id: &str) -> Result<(), StoreError> {
    let exhausted:bool=app.app.query_row("SELECT EXISTS(SELECT 1 FROM plugin_operations o JOIN plugin_commands c ON c.operation_id=o.operation_id WHERE o.plugin_id=?1 AND o.state IN ('queued','running','waiting','blocked','deferred') AND (o.revision=9223372036854775807 OR c.revision=9223372036854775807))",[id],|r|r.get(0))?;
    if exhausted {
        return Err(PluginError::Conflict.into());
    }
    app.app.execute("UPDATE plugin_commands SET effect=CASE WHEN effect='confirmed' THEN 'confirmed' WHEN effect='unknown' OR (SELECT state FROM plugin_operations WHERE operation_id=plugin_commands.operation_id)='running' THEN 'unknown' ELSE 'none' END,claim_nonce=NULL,revision=revision+1 WHERE operation_id IN (SELECT operation_id FROM plugin_operations WHERE plugin_id=?1 AND state IN ('queued','running','waiting','blocked','deferred') AND NOT (operation_id IS (SELECT active_operation_id FROM plugins WHERE plugin_id=?1) AND generation IS NULL))",[id])?;
    app.app.execute("UPDATE plugin_operations SET state=CASE (SELECT effect FROM plugin_commands WHERE operation_id=plugin_operations.operation_id) WHEN 'confirmed' THEN 'blocked' WHEN 'unknown' THEN 'unknown' ELSE 'cancelled' END,phase='authority-closed',revision=revision+1,updated_at=?2,next_run_at=NULL WHERE plugin_id=?1 AND state IN ('queued','running','waiting','blocked','deferred') AND NOT (operation_id IS (SELECT active_operation_id FROM plugins WHERE plugin_id=?1) AND generation IS NULL)",params![id,crate::unix_now()])?;
    Ok(())
}

pub(super) fn validate_cause(cause: &PluginCommandCause, runtime: bool) -> Result<(), StoreError> {
    if cause.cause_id.is_empty()
        || cause.cause_id.len() > 128
        || !cause.cause_id.bytes().all(|b| b.is_ascii_graphic())
        || (cause.depth == 0) != cause.visited.is_empty()
        || cause.visited.len() > 800
    {
        return Err(PluginError::Invalid("invalid command cause").into());
    }
    if !runtime && (cause.depth != 0 || !cause.visited.is_empty()) {
        return Err(
            PluginError::Invalid("management operation cannot carry runtime cause hops").into(),
        );
    }
    for (idx, hop) in cause.visited.iter().enumerate() {
        let valid_hop = !hop.plugin_id.is_empty()
            && hop.plugin_id.len() <= 256
            && !hop.kind.is_empty()
            && hop.kind.len() <= 256
            && !hop.target_key.is_empty()
            && hop.target_key.len() <= 256;
        if !valid_hop || cause.visited[..idx].contains(hop) {
            return Err(PluginError::Invalid("invalid command cause").into());
        }
    }
    Ok(())
}

fn next_cause_fields(
    cause: &PluginCommandCause,
    plugin_id: &str,
    kind: &str,
    targets: &[PluginGrantScope],
) -> Result<(i64, String), StoreError> {
    let next_depth = cause.depth.saturating_add(1);
    let new_hops: Vec<PluginCauseHop> = targets
        .iter()
        .map(|target| PluginCauseHop {
            plugin_id: plugin_id.to_string(),
            kind: kind.to_string(),
            target_key: target.target_key(),
        })
        .collect();
    if next_depth > 4 || new_hops.iter().any(|hop| cause.visited.contains(hop)) {
        tracing::warn!(
            plugin_id,
            cause_id = %cause.cause_id,
            kind,
            depth = next_depth,
            "plugin command rejected due to cause loop or depth limit"
        );
        return Err(PluginError::CauseLoop.into());
    }
    let mut visited = cause.visited.clone();
    visited.extend(new_hops);
    Ok((i64::from(next_depth), serde_json::to_string(&visited)?))
}

fn validate_canonical_request(
    input: &AcceptPluginOperation,
    payload: &[u8],
) -> Result<(), StoreError> {
    if input.idempotency_key.is_empty()
        || input.idempotency_key.len() > 256
        || input.kind.is_empty()
        || input.request.method.is_empty()
        || !input
            .request
            .method
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-')
        || !input.request.path.starts_with('/')
        || payload.len() > 1048576
    {
        return Err(PluginError::Invalid("invalid canonical operation request").into());
    }
    Ok(())
}

fn replay_existing(
    app: &App<'_>,
    input: &AcceptPluginOperation,
    binding: Option<&super::authority::Binding>,
    runtime: bool,
    targets: &[PluginGrantScope],
    subject: &str,
    plugin_scope: &str,
) -> Result<Option<AcceptedPluginOperation>, StoreError> {
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
        || kind != input.kind
    {
        return Err(PluginError::Conflict.into());
    }
    let id = uuid::Uuid::parse_str(&id)?;
    let command = super::operation_rows::command(app, id)?;
    if (command.class == "runtime") != runtime
        || command.scope.as_ref() != binding.map(|b| &b.scope)
    {
        return Err(PluginError::Denied.into());
    }
    if command.targets.as_slice() != targets || command.cause.cause_id != input.cause.cause_id {
        return Err(PluginError::Conflict.into());
    }
    let operation = super::operation_rows::get(app, id)?.ok_or(PluginError::NotFound)?;
    Ok(Some(AcceptedPluginOperation {
        operation,
        replayed: true,
    }))
}

fn validate_binding(
    app: &App<'_>,
    input: &AcceptPluginOperation,
    binding: &super::authority::Binding,
    runtime: bool,
    targets: &[PluginGrantScope],
) -> Result<(), StoreError> {
    if input.plugin_id.as_deref() != Some(binding.plugin_id.as_str()) {
        return Err(PluginError::Denied.into());
    }
    if matches!(&input.subject, PluginSubject::Task { plugin_id, .. } if plugin_id != &binding.plugin_id)
    {
        return Err(PluginError::Denied.into());
    }
    if runtime {
        super::command_authority::validate_targets(app, binding, &input.kind, targets)?;
        if input.expected_generation != Some(binding.generation) {
            return Err(PluginError::StaleAuthority.into());
        }
    } else {
        super::authority::validate_config(app, binding)?;
    }
    Ok(())
}

fn insert_operation_rows(
    app: &App<'_>,
    tx: &rusqlite::Connection,
    input: &AcceptPluginOperation,
    binding: Option<&super::authority::Binding>,
    runtime: bool,
    targets: &[PluginGrantScope],
    payload: &[u8],
    subject: &str,
    plugin_scope: &str,
) -> Result<PluginOperation, StoreError> {
    let (stored_depth, stored_visited) = if runtime {
        let b = binding.ok_or(PluginError::Denied)?;
        next_cause_fields(&input.cause, &b.plugin_id, &input.kind, targets)?
    } else {
        (0, "[]".to_string())
    };
    let id = uuid::Uuid::new_v4();
    let now = crate::unix_now();
    let generation = runtime.then(|| binding.map(|b| b.generation)).flatten();
    tx.execute(
        "INSERT INTO plugin_operations VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,'queued','accepted',NULL,?11,?11)",
        params![id.to_string(), input.plugin_id, plugin_scope, subject, input.idempotency_key, input.kind, input.request.method, input.request.path, input.request.body_digest.as_slice(), generation, now],
    )?;
    let scope = binding.map(|b| b.scope.key()).transpose()?;
    tx.execute(
        "INSERT INTO plugin_commands VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1,0,NULL,'none')",
        params![
            id.to_string(),
            if runtime { "runtime" } else { "management" },
            binding.map(|b| b.hash.as_str()),
            input.expected_revision,
            scope,
            binding.map(|b| b.generation),
            serde_json::to_string(targets)?,
            &input.cause.cause_id,
            stored_depth,
            &stored_visited,
        ],
    )?;
    if let Some(scope) = &scope {
        tx.execute(
            "INSERT INTO plugin_command_payloads VALUES (?1,?2,?3)",
            params![id.to_string(), scope, payload],
        )?;
    }
    super::operation_rows::get(app, id)?.ok_or_else(|| PluginError::NotFound.into())
}

fn accept(
    app: &App<'_>,
    input: &AcceptPluginOperation,
    binding: Option<&super::authority::Binding>,
    runtime: bool,
    targets: &[PluginGrantScope],
    payload: &[u8],
) -> Result<AcceptedPluginOperation, StoreError> {
    validate_canonical_request(input, payload)?;
    validate_cause(&input.cause, runtime)?;
    let tx = crate::write_tx(app.app)?;
    if let Some(binding) = binding {
        validate_binding(app, input, binding, runtime, targets)?;
    }
    let subject = serde_json::to_string(&input.subject)?;
    let plugin_scope = serde_json::to_string(&input.plugin_id)?;
    if let Some(replayed) = replay_existing(
        app,
        input,
        binding,
        runtime,
        targets,
        &subject,
        &plugin_scope,
    )? {
        tx.commit()?;
        return Ok(replayed);
    }
    if let Some(id) = &input.plugin_id {
        let p = app.get_plugin(id)?.ok_or(PluginError::NotFound)?;
        if input.expected_revision != Some(p.revision)
            || input.expected_generation.is_some_and(|g| g != p.generation)
        {
            return Err(PluginError::Conflict.into());
        }
    } else if input.expected_revision.is_some() || input.expected_generation.is_some() || runtime {
        return Err(PluginError::Invalid("management scope fence mismatch").into());
    }
    let operation = insert_operation_rows(
        app,
        &tx,
        input,
        binding,
        runtime,
        targets,
        payload,
        &subject,
        &plugin_scope,
    )?;
    tx.commit()?;
    Ok(AcceptedPluginOperation {
        operation,
        replayed: false,
    })
}

impl App<'_> {
    /// Authenticated management service only; never expose as a guest command import.
    pub fn accept_plugin_operation(
        &self,
        input: &AcceptPluginOperation,
    ) -> Result<AcceptedPluginOperation, StoreError> {
        if matches!(input.subject, PluginSubject::Task { .. }) {
            return Err(PluginError::Denied.into());
        }
        accept(self, input, None, false, &[], &[])
    }
    pub fn accept_plugin_command(
        &self,
        authority: &PluginCommandAuthority,
        input: &AcceptPluginOperation,
        payload: &[u8],
    ) -> Result<AcceptedPluginOperation, StoreError> {
        if input.kind != authority.kind {
            return Err(PluginError::Denied.into());
        }
        accept(
            self,
            input,
            Some(&authority.binding),
            true,
            &authority.targets,
            payload,
        )
    }
    pub fn accept_plugin_config_operation(
        &self,
        authority: &PluginConfigAuthority,
        input: &AcceptPluginOperation,
        payload: &[u8],
    ) -> Result<AcceptedPluginOperation, StoreError> {
        if matches!(input.subject, PluginSubject::Task { .. }) {
            return Err(PluginError::Denied.into());
        }
        accept(self, input, Some(&authority.0), false, &[], payload)
    }
    pub fn get_plugin_operation(
        &self,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginOperation>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {} FROM plugin_operations WHERE operation_id=?1 AND subject=?2",
                    super::operation_rows::COLUMNS
                ),
                params![id.to_string(), serde_json::to_string(subject)?],
                super::operation_rows::row,
            )
            .optional()
            .map_err(Into::into)
    }
    pub fn list_plugin_operations(
        &self,
        subject: &PluginSubject,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginOperation>, StoreError> {
        let mut stmt=self.app.prepare(&format!("SELECT {} FROM plugin_operations WHERE subject=?1 AND operation_id>?2 ORDER BY operation_id LIMIT ?3",super::operation_rows::COLUMNS))?;
        Ok(stmt
            .query_map(
                params![
                    serde_json::to_string(subject)?,
                    after.map(|id| id.to_string()).unwrap_or_default(),
                    super::paging::limit(count) as i64
                ],
                super::operation_rows::row,
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

impl Store {
    pub fn accept_plugin_operation(
        &self,
        input: &AcceptPluginOperation,
    ) -> Result<AcceptedPluginOperation, StoreError> {
        self.with_app(|app| app.accept_plugin_operation(input))
    }
    pub fn accept_plugin_command(
        &self,
        a: &PluginCommandAuthority,
        input: &AcceptPluginOperation,
        payload: &[u8],
    ) -> Result<AcceptedPluginOperation, StoreError> {
        self.with_app(|app| app.accept_plugin_command(a, input, payload))
    }
    pub fn accept_plugin_config_operation(
        &self,
        a: &PluginConfigAuthority,
        input: &AcceptPluginOperation,
        payload: &[u8],
    ) -> Result<AcceptedPluginOperation, StoreError> {
        self.with_app(|app| app.accept_plugin_config_operation(a, input, payload))
    }
    pub fn get_plugin_operation(
        &self,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginOperation>, StoreError> {
        self.with_app_read(|app| app.get_plugin_operation(subject, id))
    }
    pub fn list_plugin_operations(
        &self,
        subject: &PluginSubject,
        after: Option<uuid::Uuid>,
        count: Option<usize>,
    ) -> Result<Vec<PluginOperation>, StoreError> {
        self.with_app_read(|app| app.list_plugin_operations(subject, after, count))
    }
    pub fn has_unresolved_plugin_operations(&self, id: &str) -> Result<bool, StoreError> {
        self.with_app_read(|app| has_unresolved(app, id))
    }
}
