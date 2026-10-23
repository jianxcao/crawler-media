//! Persisted intent and registration for library-generated NFO sidecars.
//!
//! Ordering contract of a generated sidecar:
//! 1. the caller persists the export intent (`begin_nfo_export`) before it
//!    touches the filesystem, so a crash between the two is recoverable;
//! 2. the caller replaces the file atomically, then registers the observed
//!    digest (`complete_nfo_export`);
//! 3. a restart decides by digest: the registration for an already replaced
//!    file is written without rewriting it.
//!
//! The registration lives in the existing `library_operations` history table
//! under `kind = 'scrape'`; the succeeded row is the export registry, so the
//! sidecar and its authority feedback share one durable record.

use std::collections::BTreeMap;
use std::path::PathBuf;

use domain::{
    EffectiveFieldOrigin, EntityScope, FileVersion, LedgerId, LibraryId, MediaId, MetadataFields,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::playback::types::OperationStatus;
use crate::{Store, StoreError};

/// `library_operations.kind` of an NFO export.
pub const NFO_EXPORT_KIND: &str = "scrape";

/// Everything the export driver needs to rebuild, replace and register one
/// generated sidecar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NfoExportRequest {
    pub operation_id: String,
    pub export_id: String,
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub root_id: String,
    pub file_id: LedgerId,
    pub expected_version: FileVersion,
    pub config_revision: i64,
    pub scope: EntityScope,
    pub locale: String,
    pub path: PathBuf,
    /// Digest the driver expects to find before replacing (`None` = no file).
    pub expected_digest: Option<String>,
    pub body: String,
    /// Digest of `body`, registered once the file holds it.
    pub output_digest: String,
    /// Base effective fields the body was rendered from: the three-way baseline
    /// recorded as `export_fields_json`, *without* local edits.
    pub fields: MetadataFields,
    pub field_origins: BTreeMap<String, EffectiveFieldOrigin>,
    /// R18: this Library's sidecar edits (sidecar vs. last exported base), rendered
    /// on top of `fields`. They live only in this sidecar/input — never in the
    /// global Media overrides — so deleting the NFO drops them and another
    /// Library never sees them.
    #[serde(default)]
    pub local_edits: MetadataFields,
}

/// Durable record of a sidecar this library produced.
#[derive(Clone, Debug, PartialEq)]
pub struct NfoExportRegistration {
    pub operation_id: String,
    pub export_id: String,
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub scope: EntityScope,
    pub locale: String,
    pub path: PathBuf,
    pub output_digest: String,
    pub fields: MetadataFields,
    pub state: String,
}

#[derive(Serialize, Deserialize)]
struct NfoExportPlanV1 {
    version: u32,
    request: NfoExportRequest,
}

#[derive(Serialize, Deserialize)]
struct NfoExportCursorV1 {
    version: u32,
    observed_digest: String,
}

fn encode_plan(request: &NfoExportRequest) -> Result<String, StoreError> {
    serde_json::to_string(&NfoExportPlanV1 {
        version: 1,
        request: request.clone(),
    })
    .map_err(|error| StoreError::Invalid(error.to_string()))
}

fn decode_plan(plan_json: &str) -> Result<NfoExportRequest, StoreError> {
    serde_json::from_str::<NfoExportPlanV1>(plan_json)
        .map(|plan| plan.request)
        .map_err(|error| StoreError::Invalid(error.to_string()))
}

fn registration(
    operation_id: &str,
    state: &str,
    request: NfoExportRequest,
) -> NfoExportRegistration {
    NfoExportRegistration {
        operation_id: operation_id.to_string(),
        export_id: request.export_id,
        library_id: request.library_id,
        media_id: request.media_id,
        scope: request.scope,
        locale: request.locale,
        path: request.path,
        output_digest: request.output_digest,
        fields: request.fields,
        state: state.to_string(),
    }
}

impl Store {
    /// Persist the export intent of one sidecar before any file is touched.
    pub fn begin_nfo_export(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<OperationStatus, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let library = request.library_id.to_string();
        let lifecycle: Option<String> = tx
            .query_row(
                "SELECT lifecycle_state FROM libraries WHERE library_id = ?1",
                params![library],
                |row| row.get(0),
            )
            .optional()?;
        let Some(lifecycle) = lifecycle else {
            return Err(StoreError::Missing(format!("library {library}")));
        };
        if lifecycle != "ready" && lifecycle != "reindexing" {
            return Err(StoreError::Protected(format!(
                "library {library} lifecycle is {lifecycle}"
            )));
        }

        ensure_export_binding(&tx, request)?;
        let plan_json = encode_plan(request)?;
        for (operation_id, stale_json) in scrape_operations(&tx)? {
            let stale = decode_plan(&stale_json)?;
            if stale.media_id == request.media_id && stale.path == request.path {
                tx.execute(
                    "DELETE FROM library_operations WHERE operation_id = ?1",
                    params![operation_id],
                )?;
            }
        }

        tx.execute(
            "INSERT INTO library_operations
                (operation_id, library_id, kind, state, expected_revision, plan_json,
                 cursor_json, error, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, 'planned', ?4, ?5, NULL, NULL, ?6, ?6)",
            params![
                request.operation_id,
                library,
                NFO_EXPORT_KIND,
                request.config_revision,
                plan_json,
                now_ms
            ],
        )?;
        tx.commit()?;
        Ok(OperationStatus {
            operation_id: request.operation_id.clone(),
            phase: "planned".into(),
            state: "planned".into(),
            error: None,
        })
    }

    /// Register the digest the sidecar holds now, completing the export.
    pub fn complete_nfo_export(
        &self,
        operation_id: &str,
        observed_digest: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let plan_json: Option<String> = tx
            .query_row(
                "SELECT plan_json FROM library_operations
                 WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| row.get(0),
            )
            .optional()?;
        let Some(plan_json) = plan_json else {
            return Err(StoreError::Missing(format!(
                "nfo export operation {operation_id}"
            )));
        };
        let request = decode_plan(&plan_json)?;
        if request.output_digest != observed_digest {
            return Err(StoreError::Conflict(format!(
                "nfo export {operation_id} expected digest {}, observed {observed_digest}",
                request.output_digest
            )));
        }
        let cursor_json = serde_json::to_string(&NfoExportCursorV1 {
            version: 1,
            observed_digest: observed_digest.to_string(),
        })
        .map_err(|error| StoreError::Invalid(error.to_string()))?;
        tx.execute(
            "UPDATE library_operations
             SET state = 'succeeded', cursor_json = ?2, error = NULL, updated_at_ms = ?3
             WHERE operation_id = ?1",
            params![operation_id, cursor_json, now_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether `operation_id` is still an unfinished NFO export.
    pub fn nfo_export_pending(&self, operation_id: &str) -> Result<bool, StoreError> {
        let state: Option<String> = self
            .library
            .query_row(
                "SELECT state FROM library_operations WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| row.get(0),
            )
            .optional()?;
        Ok(state.is_some_and(|state| state != "succeeded" && state != "failed"))
    }

    /// Re-check the file/config/Media binding an export was planned against (R17 CAS).
    pub fn nfo_export_binding_current(&self, request: &NfoExportRequest) -> Result<(), StoreError> {
        ensure_export_binding(&self.library, request)
    }

    /// Turn an unfinished export terminal as failed; recovery no longer drives it.
    pub fn fail_nfo_export(
        &self,
        operation_id: &str,
        error: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "UPDATE library_operations SET state = 'failed', error = ?2, updated_at_ms = ?3
             WHERE operation_id = ?1 AND kind = ?4",
            params![operation_id, error, now_ms, NFO_EXPORT_KIND],
        )?;
        Ok(())
    }

    /// Registration of a completed export, or `None` while it is unfinished.
    pub fn nfo_export_registration(
        &self,
        operation_id: &str,
    ) -> Result<Option<NfoExportRegistration>, StoreError> {
        let row: Option<(String, String)> = self
            .library
            .query_row(
                "SELECT state, plan_json FROM library_operations
                 WHERE operation_id = ?1 AND kind = ?2",
                params![operation_id, NFO_EXPORT_KIND],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, plan_json)) = row else {
            return Ok(None);
        };
        if state != "succeeded" {
            return Ok(None);
        }
        Ok(Some(registration(
            operation_id,
            &state,
            decode_plan(&plan_json)?,
        )))
    }

    /// Exports whose intent is durable but whose file may not be registered.
    pub fn pending_nfo_exports(&self) -> Result<Vec<NfoExportRequest>, StoreError> {
        let tx = crate::write_tx(&self.library)?;
        let mut requests = Vec::new();
        for (_, plan_json) in scrape_operations(&tx)? {
            requests.push(decode_plan(&plan_json)?);
        }
        Ok(requests)
    }
}

/// Every unfinished `kind = 'scrape'` operation, oldest first.
fn scrape_operations(tx: &rusqlite::Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT operation_id, plan_json FROM library_operations
         WHERE kind = ?1 AND state NOT IN ('succeeded', 'failed')
         ORDER BY updated_at_ms, operation_id",
    )?;
    let rows = statement.query_map(params![NFO_EXPORT_KIND], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut operations = Vec::new();
    for row in rows {
        operations.push(row?);
    }
    Ok(operations)
}

/// The export must still describe the file, Media and config it was planned against:
/// a stale source version, a reconfigured Library or a reassigned file is refused (R17).
fn ensure_export_binding(
    tx: &rusqlite::Connection,
    request: &NfoExportRequest,
) -> Result<(), StoreError> {
    let library = request.library_id.to_string();
    let config: i64 = tx.query_row(
        "SELECT config_revision FROM libraries WHERE library_id = ?1",
        params![library],
        |row| row.get(0),
    )?;
    if config != request.config_revision {
        return Err(StoreError::Protected(format!(
            "nfo export {}: library config revision {config}, planned {}",
            request.export_id, request.config_revision
        )));
    }
    let file: Option<(String, String, i64, Option<String>)> = tx
        .query_row(
            "SELECT library_id, media_id, file_revision, source_version
             FROM library_files WHERE file_id = ?1",
            params![request.file_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((file_library, media, revision, source)) = file else {
        return Err(StoreError::Protected(format!(
            "nfo export {}: file {} is gone",
            request.export_id, request.file_id
        )));
    };
    let source = source.unwrap_or_default();
    if file_library != library
        || media != request.media_id.to_string()
        || revision != request.expected_version.revision
        || source != request.expected_version.source_version
    {
        return Err(StoreError::Protected(format!(
            "nfo export {}: file {} no longer matches the planned version",
            request.export_id, request.file_id
        )));
    }
    Ok(())
}
