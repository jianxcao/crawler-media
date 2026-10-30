use rusqlite::{Connection, OptionalExtension, params};

use super::super::{Store, StoreError};
use super::types::EffectiveMetadata;
use domain::{MediaId, MetadataAdoption};

/// Display facts a **Scrape** observed for one **Media**; they are written with the
/// adopted metadata so the card and the detail view read one source of truth.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrapedDisplay<'a> {
    pub rating: Option<f64>,
    pub release_date: Option<&'a str>,
    pub studio: Option<&'a str>,
    pub studio_logo: Option<&'a str>,
}

impl Store {
    pub fn adopt_metadata(
        &self,
        input: &MetadataAdoption,
        _now_ms: i64,
    ) -> Result<i64, StoreError> {
        self.with_library_write(|db| db.adopt_metadata(input, _now_ms))
    }

    pub fn get_effective_metadata(
        &self,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<EffectiveMetadata>, StoreError> {
        self.with_library_read(|db| db.get_effective_metadata(media_id, locale))
    }

    /// Record display facts observed by a Scrape (rating, release date, studio, logo).
    ///
    /// They belong to the adopted metadata, never inside `library_items.aliases_json`
    /// (which holds only the five external ids), and the affected cards are reduced in
    /// the same transaction.
    pub fn record_scraped_display(
        &self,
        media_id: MediaId,
        locale: &str,
        display: ScrapedDisplay<'_>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.record_scraped_display(media_id, locale, display))
    }
}

/// The adoption was fetched for a Media identity captured before the network call. Refuse
/// it (zero writes) unless that identity is still writable, its metadata revision is the
/// captured one, and the response belongs to the Media's current refresh target.
fn ensure_adoption_current(
    tx: &Connection,
    input: &MetadataAdoption,
    expected: i64,
) -> Result<(), StoreError> {
    let media = input.media_id.to_string();
    let row: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT identity_state, kind, metadata_revision FROM media WHERE media_id = ?1",
            params![media],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((state, kind, revision)) = row else {
        return Err(StoreError::Missing(format!("media {}", input.media_id)));
    };
    if state == "merging" || state == "retired" {
        return Err(StoreError::Protected(format!(
            "media {} identity is frozen ({state})",
            input.media_id
        )));
    }
    if revision != expected {
        return Err(StoreError::Conflict(format!(
            "media {} metadata revision moved: captured {expected}, current {revision}",
            input.media_id
        )));
    }
    let target: Option<String> = tx
        .query_row(
            "SELECT external_id FROM media_aliases
             WHERE media_id = ?1 AND source = ?2 AND kind = ?3 AND is_refresh_target = 1",
            params![media, input.source, kind],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(target) = target
        && target != input.provider_work_id
    {
        return Err(StoreError::Conflict(format!(
            "media {} refresh target for {} is {target}, not {}",
            input.media_id, input.source, input.provider_work_id
        )));
    }
    Ok(())
}

/// Insert or replace this source's snapshot for the entity/locale, bumping its revision.
fn upsert_source_snapshot(tx: &Connection, input: &MetadataAdoption) -> Result<(), StoreError> {
    let media = input.media_id.to_string();
    let (season, episode) = input.scope.season_episode();
    let present: Vec<&String> = input.normalized.field_origins.keys().collect();
    tx.execute(
        "INSERT INTO media_metadata_sources
            (media_id, entity_scope, season, episode, source, locale, provider_entity_id,
             provider_work_id, normalized_json, present_fields_json, preferences_json,
             selection_origins_json, fetched_at, source_revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1)
         ON CONFLICT(media_id, entity_scope, season, episode, source, locale) DO UPDATE SET
             provider_entity_id = excluded.provider_entity_id,
             provider_work_id = excluded.provider_work_id,
             normalized_json = excluded.normalized_json,
             present_fields_json = excluded.present_fields_json,
             preferences_json = excluded.preferences_json,
             selection_origins_json = excluded.selection_origins_json,
             fetched_at = excluded.fetched_at,
             source_revision = media_metadata_sources.source_revision + 1",
        params![
            media,
            input.scope.as_str(),
            season,
            episode,
            input.source,
            input.locale,
            input.provider_entity_id,
            input.provider_work_id,
            serde_json::to_string(&input.normalized.fields)?,
            serde_json::to_string(&present)?,
            serde_json::to_string(&input.preferences)?,
            serde_json::to_string(&input.normalized.field_origins)?,
            input.fetched_at,
        ],
    )?;
    Ok(())
}

#[path = "adopt/connection.rs"]
mod connection;
