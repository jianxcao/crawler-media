use super::operation_rows;
use super::*;
use crate::{App, Store, StoreError};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const IDEM_PREFIX: &str = "plugin_idem.";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginIdempotencyEnvelope {
    pub scope: String,
    pub method: String,
    pub path: String,
    pub body_digest: [u8; 32],
    pub response_json: serde_json::Value,
    pub created_at: i64,
}

pub fn validate_idempotency_input(
    idempotency_key: &str,
    request: &CanonicalPluginRequest,
) -> Result<(), StoreError> {
    if idempotency_key.is_empty()
        || idempotency_key.len() > 256
        || request.method.is_empty()
        || !request
            .method
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b == b'-')
        || !request.path.starts_with('/')
    {
        return Err(PluginError::Invalid("invalid canonical idempotency request").into());
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub(super) fn envelope_key(
    subject: &PluginSubject,
    idempotency_key: &str,
) -> Result<String, StoreError> {
    let subject_json = serde_json::to_string(subject)?;
    Ok(format!(
        "{IDEM_PREFIX}{}.{}",
        hex_sha256(subject_json.as_bytes()),
        hex_sha256(idempotency_key.as_bytes())
    ))
}

pub(super) fn has_idempotency_envelope(
    app: &App<'_>,
    subject: &PluginSubject,
    idempotency_key: &str,
) -> Result<bool, StoreError> {
    let key = envelope_key(subject, idempotency_key)?;
    Ok(app.get_setting(&key)?.is_some())
}

pub(super) fn has_operation_for_subject_key(
    app: &App<'_>,
    subject_json: &str,
    idempotency_key: &str,
) -> Result<bool, StoreError> {
    Ok(app.app.query_row(
        "SELECT EXISTS(SELECT 1 FROM plugin_operations WHERE subject=?1 AND idempotency_key=?2)",
        params![subject_json, idempotency_key],
        |r| r.get(0),
    )?)
}

impl App<'_> {
    pub fn get_plugin_operation_by_subject_idempotency(
        &self,
        subject: &PluginSubject,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        kind: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        validate_idempotency_input(idempotency_key, request)?;
        let tx = crate::read_tx(self.app)?;
        let subject_json = serde_json::to_string(subject)?;
        let mut stmt = tx.prepare(
            "SELECT operation_id,canonical_method,canonical_path,body_digest,kind FROM plugin_operations WHERE subject=?1 AND idempotency_key=?2 ORDER BY created_at DESC, operation_id DESC",
        )?;
        let rows = stmt
            .query_map(params![subject_json, idempotency_key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        if rows.is_empty() {
            if has_idempotency_envelope(self, subject, idempotency_key)? {
                return Err(PluginError::IdempotencyConflict.into());
            }
            tx.commit()?;
            return Ok(None);
        }
        for (id, method, path, body, stored_kind) in rows {
            if method == request.method
                && path == request.path
                && body.as_slice() == request.body_digest.as_slice()
                && stored_kind == kind
            {
                let op = operation_rows::get(self, uuid::Uuid::parse_str(&id)?)?;
                tx.commit()?;
                return Ok(op);
            }
        }
        Err(PluginError::IdempotencyConflict.into())
    }

    pub fn get_plugin_idempotency_envelope(
        &self,
        subject: &PluginSubject,
        scope: &str,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        validate_idempotency_input(idempotency_key, request)?;
        let tx = crate::read_tx(self.app)?;
        let key = envelope_key(subject, idempotency_key)?;
        if let Some(raw) = self.get_setting(&key)? {
            let env: PluginIdempotencyEnvelope = serde_json::from_str(&raw)?;
            if env.scope == scope
                && env.method == request.method
                && env.path == request.path
                && env.body_digest == request.body_digest
            {
                tx.commit()?;
                return Ok(Some(env.response_json));
            }
            return Err(PluginError::IdempotencyConflict.into());
        }
        let subject_json = serde_json::to_string(subject)?;
        if has_operation_for_subject_key(self, &subject_json, idempotency_key)? {
            return Err(PluginError::IdempotencyConflict.into());
        }
        tx.commit()?;
        Ok(None)
    }

    pub fn put_plugin_idempotency_envelope(
        &self,
        subject: &PluginSubject,
        scope: &str,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        response_json: &serde_json::Value,
    ) -> Result<(), StoreError> {
        validate_idempotency_input(idempotency_key, request)?;
        let key = envelope_key(subject, idempotency_key)?;
        let env = PluginIdempotencyEnvelope {
            scope: scope.to_string(),
            method: request.method.clone(),
            path: request.path.clone(),
            body_digest: request.body_digest,
            response_json: response_json.clone(),
            created_at: crate::unix_now(),
        };
        self.put_setting(&key, &serde_json::to_string(&env)?)?;
        Ok(())
    }
}

impl Store {
    pub fn get_plugin_operation_by_subject_idempotency(
        &self,
        subject: &PluginSubject,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        kind: &str,
    ) -> Result<Option<PluginOperation>, StoreError> {
        self.with_app_read(|app| {
            app.get_plugin_operation_by_subject_idempotency(subject, idempotency_key, request, kind)
        })
    }

    pub fn get_plugin_idempotency_envelope(
        &self,
        subject: &PluginSubject,
        scope: &str,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
    ) -> Result<Option<serde_json::Value>, StoreError> {
        self.with_app_read(|app| {
            app.get_plugin_idempotency_envelope(subject, scope, idempotency_key, request)
        })
    }

    pub fn save_plugin_idempotency_envelope(
        &self,
        subject: &PluginSubject,
        scope: &str,
        idempotency_key: &str,
        request: &CanonicalPluginRequest,
        response_json: &serde_json::Value,
    ) -> Result<serde_json::Value, StoreError> {
        self.app_write(|app| {
            if let Some(existing) =
                app.get_plugin_idempotency_envelope(subject, scope, idempotency_key, request)?
            {
                return Ok(existing);
            }
            app.put_plugin_idempotency_envelope(
                subject,
                scope,
                idempotency_key,
                request,
                response_json,
            )?;
            Ok(response_json.clone())
        })
    }
}
