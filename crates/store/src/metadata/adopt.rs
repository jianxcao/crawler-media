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
        if input.source.trim().is_empty() || input.provider_entity_id.trim().is_empty() {
            return Err(StoreError::Invalid(
                "metadata adoption requires a source and provider entity id".into(),
            ));
        }
        let Some(expected) = input.expected_revision else {
            return Err(StoreError::Invalid(
                "metadata adoption requires the captured Media metadata revision".into(),
            ));
        };
        let tx = crate::write_tx(&self.library)?;
        ensure_adoption_current(&tx, input, expected)?;
        upsert_source_snapshot(&tx, input)?;
        // Recompose from every current snapshot plus overrides: a source that now reports a
        // field as Missing stops contributing, and adoption order cannot change the result.
        let revision =
            super::effective::write_effective(&tx, input.media_id, input.scope, &input.locale)?;
        // The card is derived from the facts just written; refresh it in the same transaction.
        crate::library_write::reduce::reduce_media_everywhere(&tx, input.media_id)?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn get_effective_metadata(
        &self,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<EffectiveMetadata>, StoreError> {
        let sql = "SELECT media_id, locale, title, original_title, year, overview, release_date,
                          rating, runtime_minutes, language, genres_json, poster_url, backdrop_url,
                          details_json, field_origins_json, revision
                   FROM media_metadata
                   WHERE media_id = ?1 AND locale = ?2";
        let mut stmt = self.library.prepare(sql)?;
        let row = stmt
            .query_row(params![media_id.to_string(), locale], |r| {
                let y: Option<i64> = r.get(4)?;
                let rt: Option<i64> = r.get(8)?;
                Ok(EffectiveMetadata {
                    media_id,
                    locale: r.get(1)?,
                    title: r.get(2)?,
                    original_title: r.get(3)?,
                    year: y.map(|v| v as u16),
                    overview: r.get(5)?,
                    release_date: r.get(6)?,
                    rating: r.get(7)?,
                    runtime_minutes: rt.map(|v| v as u32),
                    language: r.get(9)?,
                    genres_json: r.get(10)?,
                    poster_url: r.get(11)?,
                    backdrop_url: r.get(12)?,
                    details_json: r.get(13)?,
                    field_origins_json: r.get(14)?,
                    revision: r.get(15)?,
                })
            })
            .optional()?;
        Ok(row)
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
        let rating = display.rating.filter(|value| (0.0..=10.0).contains(value));
        let release_date = display
            .release_date
            .filter(|value| !value.trim().is_empty());
        let studio = display.studio.filter(|value| !value.trim().is_empty());
        let studio_logo = display.studio_logo.filter(|value| !value.trim().is_empty());
        if rating.is_none() && release_date.is_none() && studio.is_none() && studio_logo.is_none() {
            return Ok(());
        }
        let tx = crate::write_tx(&self.library)?;
        crate::library_write::publish::ensure_media_writable(&tx, media_id)?;
        let media = media_id.to_string();
        let existing: Option<String> = tx
            .query_row(
                "SELECT details_json FROM media_metadata WHERE media_id = ?1 AND locale = ?2",
                params![media, locale],
                |r| r.get(0),
            )
            .optional()?;
        let mut payload = existing
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        payload.insert("version".into(), serde_json::Value::from(1));
        let fields = payload
            .entry("fields")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(object) = fields.as_object_mut() {
            if let Some(name) = studio {
                object.insert(
                    "studios".into(),
                    serde_json::json!({
                        "state": "value",
                        "value": [name.to_string()]
                    }),
                );
            }
            if let Some(logo) = studio_logo {
                object.insert(
                    "studio_logo".into(),
                    serde_json::json!({
                        "state": "value",
                        "value": logo.to_string()
                    }),
                );
            }
        }
        let details_json = serde_json::Value::Object(payload).to_string();
        match existing {
            Some(_) => {
                tx.execute(
                    "UPDATE media_metadata SET
                         rating = COALESCE(?3, rating),
                         release_date = COALESCE(?4, release_date),
                         details_json = ?5
                     WHERE media_id = ?1 AND locale = ?2",
                    params![media, locale, rating, release_date, details_json],
                )?;
            }
            None => {
                let title: String = tx.query_row(
                    "SELECT title FROM media WHERE media_id = ?1",
                    params![media],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "INSERT INTO media_metadata
                        (media_id, locale, title, rating, release_date, genres_json, countries_json,
                         details_json, field_origins_json, revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, '[]', '[]', ?6, '{}', 0)",
                    params![media, locale, title, rating, release_date, details_json],
                )?;
            }
        }
        crate::library_write::reduce::reduce_media_everywhere(&tx, media_id)?;
        tx.commit()?;
        Ok(())
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
