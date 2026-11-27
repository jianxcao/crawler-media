use super::*;
use crate::{App, Store, StoreError};
use rusqlite::{OptionalExtension, params};

fn result(
    app: &App<'_>,
    subject: &PluginSubject,
    id: uuid::Uuid,
    binding: &super::authority::Binding,
) -> Result<Option<PluginScopedBytes>, StoreError> {
    let op = app
        .get_plugin_operation(subject, id)?
        .ok_or(PluginError::NotFound)?;
    if op.plugin_id.as_deref() != Some(binding.plugin_id.as_str()) {
        return Err(PluginError::Denied.into());
    }
    let bytes=app.app.query_row("SELECT result FROM plugin_operation_results WHERE operation_id=?1 AND security_scope=?2",params![id.to_string(),binding.scope.key()?],|r|r.get::<_,Vec<u8>>(0)).optional()?;
    Ok(bytes.map(|bytes| PluginScopedBytes {
        scope: binding.scope.clone(),
        bytes,
    }))
}

fn load_claim_payload(
    tx: &rusqlite::Connection,
    op_id: uuid::Uuid,
    scope: Option<PluginSecurityScope>,
) -> Result<Option<PluginScopedBytes>, StoreError> {
    let Some(scope) = scope else {
        return Ok(None);
    };
    tx.query_row(
        "SELECT payload FROM plugin_command_payloads WHERE operation_id=?1 AND security_scope=?2",
        params![op_id.to_string(), scope.key()?],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()?
    .map(|bytes| Ok(PluginScopedBytes { scope, bytes }))
    .transpose()
}

impl App<'_> {
    pub fn inspect_claimed_plugin_command(
        &self,
        claim: &PluginCommandClaim,
    ) -> Result<PluginClaimedCommand, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let (operation, command) = super::commands::claimed(self, claim)?;
        let (subject_raw, canonical_method): (String, String) = tx.query_row(
            "SELECT subject, canonical_method FROM plugin_operations WHERE operation_id=?1",
            params![claim.operation_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let subject: PluginSubject = serde_json::from_str(&subject_raw)?;
        let payload = load_claim_payload(&tx, claim.operation_id, command.scope)?;
        tx.commit()?;
        Ok(PluginClaimedCommand {
            operation,
            is_runtime: command.class == "runtime",
            subject,
            canonical_method,
            targets: command.targets,
            cause: command.cause,
            payload,
        })
    }
    pub fn plugin_command_payload(
        &self,
        claim: &PluginCommandClaim,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let (_, command) = super::commands::claimed(self, claim)?;
        let value = load_claim_payload(&tx, claim.operation_id, command.scope)?;
        tx.commit()?;
        Ok(value)
    }
    pub fn plugin_operation_result(
        &self,
        authority: &PluginStateAuthority,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        let op = self
            .get_plugin_operation(subject, id)?
            .ok_or(PluginError::NotFound)?;
        let command = super::operation_rows::command(self, id)?;
        if command.class != "runtime" {
            return Err(PluginError::Denied.into());
        }
        super::command_authority::validate_targets(self, &authority.0, &op.kind, &command.targets)?;
        let value = result(self, subject, id, &authority.0)?;
        tx.commit()?;
        Ok(value)
    }
    /// Host Admin export only; never map to WIT or ordinary operation JSON.
    pub fn export_plugin_config_operation_result(
        &self,
        authority: &PluginConfigAuthority,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        let tx = crate::read_tx(self.app)?;
        super::authority::validate_config(self, &authority.0)?;
        if super::operation_rows::command(self, id)?.class != "management" {
            return Err(PluginError::Denied.into());
        }
        let value = result(self, subject, id, &authority.0)?;
        tx.commit()?;
        Ok(value)
    }
}
impl Store {
    pub fn inspect_claimed_plugin_command(
        &self,
        claim: &PluginCommandClaim,
    ) -> Result<PluginClaimedCommand, StoreError> {
        self.with_app_read(|app| app.inspect_claimed_plugin_command(claim))
    }
    pub fn plugin_command_payload(
        &self,
        claim: &PluginCommandClaim,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        self.with_app_read(|app| app.plugin_command_payload(claim))
    }
    pub fn plugin_operation_result(
        &self,
        a: &PluginStateAuthority,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        self.with_app_read(|app| app.plugin_operation_result(a, subject, id))
    }
    pub fn export_plugin_config_operation_result(
        &self,
        a: &PluginConfigAuthority,
        subject: &PluginSubject,
        id: uuid::Uuid,
    ) -> Result<Option<PluginScopedBytes>, StoreError> {
        self.with_app_read(|app| app.export_plugin_config_operation_result(a, subject, id))
    }
}
