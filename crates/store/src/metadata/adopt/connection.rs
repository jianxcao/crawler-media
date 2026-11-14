//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
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
        let tx = crate::write_tx(self.library)?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(
            &tx,
            input.media_id,
        )?;
        ensure_adoption_current(&tx, input, expected)?;
        upsert_source_snapshot(&tx, input)?;
        // Recompose from every current snapshot plus overrides: a source that now reports a
        // field as Missing stops contributing, and adoption order cannot change the result.
        let revision = super::super::effective::write_effective(
            &tx,
            input.media_id,
            input.scope,
            &input.locale,
        )?;
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
        let tx = crate::write_tx(self.library)?;
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
