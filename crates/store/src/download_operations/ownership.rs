use domain::DownloaderId;
use rusqlite::{Connection, OptionalExtension, params};

use super::types::*;
use super::{parse_opt_id, parse_ts_id_cursor, validate_non_empty};
use crate::maps::parse_id;
use crate::plugins::{PluginError, PluginPage, paging};
use crate::{Store, StoreError};

const OWN_COLS: &str = "downloader_id, ownership_tag, info_hash, operation_id, subscribe_id, plugin_id, subject, site_id, enclosure, ownership_state, provenance_kind, verified_bytes, bytes_digest, revision, created_at, updated_at";

fn map_ownership(row: &rusqlite::Row<'_>) -> rusqlite::Result<DownloadOwnershipRecord> {
    let state_raw: String = row.get(9)?;
    let prov_raw: String = row.get(10)?;
    Ok(DownloadOwnershipRecord {
        downloader_id: parse_id(row.get(0)?, 0)?,
        ownership_tag: row.get(1)?,
        info_hash: row.get(2)?,
        operation_id: row.get(3)?,
        subscribe_id: parse_opt_id(row.get(4)?)?,
        plugin_id: row.get(5)?,
        subject: row.get(6)?,
        site_id: parse_opt_id(row.get(7)?)?,
        enclosure: row.get(8)?,
        ownership_state: state_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        provenance_kind: prov_raw
            .parse()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        verified_bytes: row.get(11)?,
        bytes_digest: row.get(12)?,
        revision: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn fetch_ownership(
    conn: &Connection,
    downloader_id: DownloaderId,
    ownership_tag: &str,
) -> Result<Option<DownloadOwnershipRecord>, StoreError> {
    conn.query_row(
        &format!(
            "SELECT {OWN_COLS} FROM download_ownership WHERE downloader_id = ?1 AND ownership_tag = ?2"
        ),
        params![downloader_id.to_string(), ownership_tag],
        map_ownership,
    )
    .optional()
    .map_err(Into::into)
}

fn normalize_opt_hash(raw: Option<&str>) -> Result<Option<String>, StoreError> {
    match raw {
        None => Ok(None),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(PluginError::Invalid("info_hash must not be empty").into());
            }
            Ok(Some(trimmed.to_ascii_lowercase()))
        }
    }
}

fn resolve_ownership_provenance(
    existing: Option<&DownloadOwnershipRecord>,
    requested: DownloadProvenanceKind,
) -> DownloadProvenanceKind {
    match existing {
        Some(prev)
            if matches!(
                prev.provenance_kind,
                DownloadProvenanceKind::ExistingForeign | DownloadProvenanceKind::BrokenExisting
            ) =>
        {
            prev.provenance_kind
        }
        Some(prev) if prev.ownership_state == DownloadOwnershipState::Foreign => {
            DownloadProvenanceKind::ExistingForeign
        }
        Some(prev) if prev.ownership_state == DownloadOwnershipState::Broken => {
            DownloadProvenanceKind::BrokenExisting
        }
        _ => requested,
    }
}

fn ensure_owner_compatible(
    existing: Option<&DownloadOwnershipRecord>,
    input: &RecordDownloadOwnershipInput,
) -> Result<(), StoreError> {
    let Some(prev) = existing else {
        return Ok(());
    };
    let same_op_delete = input.ownership_state == DownloadOwnershipState::Deleted
        && prev.operation_id == input.operation_id;
    let plugin_mismatch =
        !(same_op_delete && input.plugin_id.is_none()) && prev.plugin_id != input.plugin_id;
    let subscribe_mismatch = !(same_op_delete && input.subscribe_id.is_none())
        && prev.subscribe_id != input.subscribe_id;
    if plugin_mismatch || subscribe_mismatch {
        return Err(PluginError::Conflict.into());
    }
    Ok(())
}

pub(super) fn upsert_ownership_in_tx(
    tx: &Connection,
    input: &RecordDownloadOwnershipInput,
) -> Result<DownloadOwnershipRecord, StoreError> {
    validate_non_empty(&input.ownership_tag, "ownership_tag must not be empty")?;
    validate_non_empty(&input.operation_id, "operation_id must not be empty")?;
    validate_non_empty(&input.subject, "subject must not be empty")?;
    validate_non_empty(&input.enclosure, "enclosure must not be empty")?;
    if input.verified_bytes.is_some_and(|v| v < 0) {
        return Err(PluginError::Invalid("verified_bytes must be >= 0").into());
    }
    let normalized_hash = normalize_opt_hash(input.info_hash.as_deref())?;
    let dl_str = input.downloader_id.to_string();
    let existing = fetch_ownership(tx, input.downloader_id, &input.ownership_tag)?;
    ensure_owner_compatible(existing.as_ref(), input)?;
    if let Some(prev) = &existing
        && input.ownership_state == DownloadOwnershipState::Owned
        && (matches!(
            prev.ownership_state,
            DownloadOwnershipState::Foreign | DownloadOwnershipState::Broken
        ) || matches!(
            prev.provenance_kind,
            DownloadProvenanceKind::ExistingForeign | DownloadProvenanceKind::BrokenExisting
        ))
    {
        return Err(PluginError::Conflict.into());
    }
    if input.ownership_state == DownloadOwnershipState::Owned
        && let Some(hash) = normalized_hash.as_deref()
    {
        let blocked: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM download_ownership
                 WHERE downloader_id = ?1 AND LOWER(info_hash) = LOWER(?2)
                   AND ownership_tag != ?3 AND ownership_state IN ('foreign', 'broken')
                 LIMIT 1",
                params![dl_str, hash, input.ownership_tag],
                |r| r.get(0),
            )
            .optional()?;
        if blocked.is_some() {
            return Err(PluginError::Conflict.into());
        }
    }
    let effective_provenance =
        resolve_ownership_provenance(existing.as_ref(), input.provenance_kind);
    tx.execute(
        "INSERT INTO download_ownership (
            downloader_id, ownership_tag, info_hash, operation_id, subscribe_id,
            plugin_id, subject, site_id, enclosure, ownership_state,
            provenance_kind, verified_bytes, bytes_digest, revision, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)
         ON CONFLICT(downloader_id, ownership_tag) DO UPDATE SET
            info_hash = COALESCE(excluded.info_hash, download_ownership.info_hash),
            operation_id = excluded.operation_id,
            subscribe_id = COALESCE(download_ownership.subscribe_id, excluded.subscribe_id),
            plugin_id = COALESCE(download_ownership.plugin_id, excluded.plugin_id),
            ownership_state = excluded.ownership_state,
            provenance_kind = CASE
                WHEN download_ownership.provenance_kind IN ('existing_foreign', 'broken_existing')
                    THEN download_ownership.provenance_kind
                ELSE excluded.provenance_kind
            END,
            verified_bytes = COALESCE(excluded.verified_bytes, download_ownership.verified_bytes),
            bytes_digest = COALESCE(excluded.bytes_digest, download_ownership.bytes_digest),
            revision = download_ownership.revision + 1,
            updated_at = excluded.updated_at",
        params![
            dl_str,
            input.ownership_tag,
            normalized_hash,
            input.operation_id,
            input.subscribe_id.map(|id| id.to_string()),
            input.plugin_id,
            input.subject,
            input.site_id.map(|id| id.to_string()),
            input.enclosure,
            input.ownership_state.as_str(),
            effective_provenance.as_str(),
            input.verified_bytes,
            input.bytes_digest,
            input.now_utc,
        ],
    )?;
    fetch_ownership(tx, input.downloader_id, &input.ownership_tag)?
        .ok_or_else(|| PluginError::NotFound.into())
}

impl Store {
    pub fn record_download_ownership(
        &self,
        input: &RecordDownloadOwnershipInput,
    ) -> Result<DownloadOwnershipRecord, StoreError> {
        self.with_writer(crate::Db::Subscribe, |conn| {
            let tx = crate::write_tx(conn)?;
            let record = upsert_ownership_in_tx(&tx, input)?;
            tx.commit()?;
            Ok(record)
        })
    }

    pub fn get_download_ownership(
        &self,
        downloader_id: DownloaderId,
        ownership_tag: &str,
    ) -> Result<Option<DownloadOwnershipRecord>, StoreError> {
        validate_non_empty(ownership_tag, "ownership_tag must not be empty")?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let row = fetch_ownership(&tx, downloader_id, ownership_tag)?;
            tx.commit()?;
            Ok(row)
        })
    }

    pub fn find_download_ownership_by_hash(
        &self,
        downloader_id: DownloaderId,
        info_hash: &str,
    ) -> Result<Vec<DownloadOwnershipRecord>, StoreError> {
        validate_non_empty(info_hash, "info_hash must not be empty")?;
        let normalized_hash = info_hash.trim().to_ascii_lowercase();
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {OWN_COLS} FROM download_ownership
                 WHERE downloader_id = ?1 AND LOWER(info_hash) = LOWER(?2)
                 ORDER BY CASE WHEN provenance_kind IN ('created_fresh', 'reconciled_operation') THEN 0 ELSE 1 END,
                          updated_at DESC, ownership_tag ASC"
            ))?;
            let rows = stmt
                .query_map(
                    params![downloader_id.to_string(), normalized_hash],
                    map_ownership,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(rows)
        })
    }

    pub fn list_download_ownership(
        &self,
        plugin_id: Option<&str>,
        downloader_id: Option<DownloaderId>,
        state: Option<DownloadOwnershipState>,
        after: Option<&str>,
        limit: Option<usize>,
    ) -> Result<PluginPage<DownloadOwnershipRecord>, StoreError> {
        let cap = paging::api_limit(limit);
        let cursor = parse_ts_id_cursor(after)?;
        self.with_subscribe_read(|db| {
            let tx = crate::read_tx(db.subscribe)?;
            let mut stmt = tx.prepare(&format!(
                "SELECT {OWN_COLS} FROM download_ownership
                 WHERE (?1 IS NULL OR plugin_id = ?1)
                   AND (?2 IS NULL OR downloader_id = ?2)
                   AND (?3 IS NULL OR ownership_state = ?3)
                   AND (?4 IS NULL OR updated_at < ?4 OR (updated_at = ?4 AND ownership_tag > ?5))
                 ORDER BY updated_at DESC, ownership_tag ASC
                 LIMIT ?6"
            ))?;
            let items = stmt
                .query_map(
                    params![
                        plugin_id,
                        downloader_id.map(|id| id.to_string()),
                        state.map(DownloadOwnershipState::as_str),
                        cursor.as_ref().map(|(ts, _)| *ts),
                        cursor.as_ref().map(|(_, tag)| tag.as_str()),
                        (cap + 1) as i64,
                    ],
                    map_ownership,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(paging::paginate(items, cap, |r| {
                format!("{}:{}", r.updated_at, r.ownership_tag)
            }))
        })
    }
}
