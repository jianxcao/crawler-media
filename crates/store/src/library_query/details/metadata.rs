use crate::metadata::EffectiveMetadata;
use crate::metadata::locale::{LocaleFallback, select_locale};
use crate::{Store, StoreError};
use domain::{FieldValue, LibraryId, MediaId, MetadataFields};
use rusqlite::OptionalExtension;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedFilterFacts {
    pub genres: Vec<String>,
    pub studios: Vec<String>,
    pub countries: Vec<String>,
    pub overview: Option<String>,
    pub people: Vec<AdoptedPerson>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdoptedPerson {
    pub name: String,
    pub role: Option<String>,
    pub tmdb_id: Option<String>,
}

pub fn instance_default_locale(store: &Store) -> Result<Option<String>, StoreError> {
    crate::metadata_language::default_locale(&store.library)
}

pub fn library_display_locale(
    store: &Store,
    library_id: LibraryId,
) -> Result<Option<String>, StoreError> {
    let display: Option<String> = store
        .library
        .query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            [library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match display.filter(|locale| !locale.trim().is_empty()) {
        Some(locale) => Ok(Some(locale)),
        None => instance_default_locale(store),
    }
}

impl Store {
    pub(crate) fn resolved_metadata_locale(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        requested: &str,
    ) -> Result<Option<String>, StoreError> {
        let mut statement = self
            .library
            .prepare("SELECT locale FROM media_metadata WHERE media_id = ?1 ORDER BY locale")?;
        let locales = statement
            .query_map([media_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if locales.is_empty() {
            return Ok(None);
        }
        let chain: Vec<String> = [
            Some(requested.to_string()),
            library_display_locale(self, library_id)?,
            instance_default_locale(self)?,
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(select_locale(
            &locales,
            &chain,
            LocaleFallback::AdoptedFirst,
        ))
    }

    pub fn adopted_filter_facts_for_media(
        &self,
        media_ids: &[MediaId],
        preferred_locale: Option<&str>,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        if media_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut by_media: HashMap<MediaId, Vec<(String, String, String, String)>> = HashMap::new();
        for chunk in media_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT media_id, locale, genres_json, countries_json, details_json
                 FROM media_metadata
                 WHERE media_id IN ({placeholders})
                 ORDER BY media_id, locale"
            );
            let mut statement = self.library.prepare(&sql)?;
            let mut rows = statement.query(rusqlite::params_from_iter(
                chunk.iter().map(|id| id.to_string()),
            ))?;
            while let Some(row) = rows.next()? {
                let id_raw: String = row.get(0)?;
                let Ok(media_id) = id_raw.parse::<MediaId>() else {
                    continue;
                };
                let locale: String = row.get(1)?;
                let genres_json: String = row.get(2)?;
                let countries_json: String = row.get(3)?;
                let details_json: String = row.get(4)?;
                by_media.entry(media_id).or_default().push((
                    locale,
                    genres_json,
                    countries_json,
                    details_json,
                ));
            }
        }

        let mut out = HashMap::with_capacity(by_media.len());
        for (media_id, rows) in by_media {
            if let Some((_, genres_json, countries_json, details_json)) =
                pick_locale_row(&rows, preferred_locale)
                && let Some(facts) =
                    adopted_facts_from_json(genres_json, countries_json, details_json, media_id)
            {
                out.insert(media_id, facts);
            }
        }
        Ok(out)
    }

    pub fn adopted_filter_facts(
        &self,
        media_ids: &[MediaId],
        preferred_locale: &str,
    ) -> Result<HashMap<MediaId, AdoptedFilterFacts>, StoreError> {
        self.adopted_filter_facts_for_media(media_ids, Some(preferred_locale))
    }
}

fn pick_locale_row<'a>(
    rows: &'a [(String, String, String, String)],
    preferred_locale: Option<&str>,
) -> Option<&'a (String, String, String, String)> {
    if rows.is_empty() {
        return None;
    }
    if let Some(preferred) = preferred_locale.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(exact) = rows.iter().find(|(loc, ..)| loc == preferred) {
            return Some(exact);
        }
        let prefix = preferred.split(['-', '_']).next().unwrap_or(preferred);
        if let Some(prefix_match) = rows
            .iter()
            .find(|(loc, ..)| loc.split(['-', '_']).next().unwrap_or(loc) == prefix)
        {
            return Some(prefix_match);
        }
    }
    rows.first()
}

/// Merge the persisted composed effective row into the NFO/base input fields.
///
/// Every composed field (not a short whitelist) fills a missing input field; a field whose
/// effective origin is a manual override wins over the input as well, so details, cards and
/// protocol hydration show the same value the composer persisted.
pub(crate) fn fill_from_adopted(base: &mut MetadataFields, adopted: &EffectiveMetadata) {
    fill_from_adopted_parts(
        base,
        &adopted.title,
        &adopted.details_json,
        &adopted.field_origins_json,
    );
}

pub(crate) fn fill_from_adopted_parts(
    base: &mut MetadataFields,
    title: &str,
    details_json: &str,
    origins_json: &str,
) {
    fill(
        &mut base.title,
        Some(title.to_string()).filter(|t| !t.is_empty()),
    );
    let composed = serde_json::from_str::<serde_json::Value>(details_json)
        .ok()
        .and_then(|value| value.get("fields").cloned())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let manual: Vec<String> = serde_json::from_str::<serde_json::Value>(origins_json)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .map(|origins| {
            origins
                .into_iter()
                .filter(|(_, origin)| {
                    origin.get("source").and_then(|s| s.as_str()) == Some("manual")
                })
                .map(|(field, _)| field)
                .collect()
        })
        .unwrap_or_default();
    let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(&*base) else {
        return;
    };
    for (field, value) in composed {
        let Some(current) = merged.get(&field) else {
            continue; // display-only keys (e.g. studio_logo) are not MetadataFields
        };
        let missing = current.get("state").and_then(|s| s.as_str()) == Some("missing");
        let is_missing_value = value.get("state").and_then(|s| s.as_str()) == Some("missing");
        if is_missing_value {
            continue;
        }
        if missing || manual.contains(&field) {
            merged.insert(field, value);
        }
    }
    if let Ok(fields) = serde_json::from_value(serde_json::Value::Object(merged)) {
        *base = fields;
    }
}

fn fill<T>(field: &mut FieldValue<T>, value: Option<T>) {
    if field.is_missing()
        && let Some(value) = value
    {
        *field = FieldValue::Value(value);
    }
}

fn adopted_facts_from_json(
    genres_json: &str,
    countries_json: &str,
    details_json: &str,
    media_id: MediaId,
) -> Option<AdoptedFilterFacts> {
    let genres = parse_adopted_json(genres_json, media_id, "genres_json")?;
    let countries = parse_adopted_json(countries_json, media_id, "countries_json")?;
    let details = parse_adopted_json(details_json, media_id, "details_json")?;
    let fields = details.get("fields");
    Some(AdoptedFilterFacts {
        genres: json_names(&genres),
        studios: fields
            .and_then(|fields| fields.get("studios"))
            .map(json_names)
            .unwrap_or_default(),
        countries: json_names(&countries),
        overview: fields
            .and_then(|fields| fields.get("overview"))
            .and_then(|overview| overview.get("value"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        people: fields
            .and_then(|fields| fields.get("cast"))
            .and_then(|cast| cast.get("value"))
            .and_then(|cast| cast.as_array())
            .map(|cast| {
                cast.iter()
                    .filter_map(|member| {
                        Some(AdoptedPerson {
                            name: member.get("name")?.as_str()?.to_string(),
                            role: member
                                .get("role")
                                .and_then(|role| role.as_str())
                                .map(str::to_string),
                            tmdb_id: member
                                .get("tmdb_id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn parse_adopted_json(raw: &str, media_id: MediaId, field: &str) -> Option<serde_json::Value> {
    match serde_json::from_str(raw) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(
                %error,
                %media_id,
                field,
                "skipping malformed adopted metadata while loading filter facts"
            );
            None
        }
    }
}

fn json_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::String(text) => vec![text.clone()],
        serde_json::Value::Array(items) => items.iter().flat_map(json_names).collect(),
        serde_json::Value::Object(map) => ["name", "value"]
            .iter()
            .find_map(|key| map.get(*key))
            .map(json_names)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
