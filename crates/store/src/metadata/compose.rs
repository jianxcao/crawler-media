//! Field-by-field composition of one entity/locale from every current source snapshot plus
//! the manual overrides. Adoption, overrides and reads all use this one function, so the
//! persisted effective row, its origins and the composed view can never disagree.
use std::collections::BTreeMap;

use domain::{
    ComposedMetadata, EffectiveFieldOrigin, EntityScope, FieldPresence, FieldValue, LibraryId,
    MediaId, MetadataFields, SourceFieldOrigin, SourceSelection,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Store, StoreError};

/// Fixed adoption priority: a lower rank wins a field for the same entity/locale.
pub(crate) fn source_rank(source: &str) -> u8 {
    match source {
        "tmdb" => 0,
        "douban" => 1,
        "tvdb" => 2,
        "bangumi" => 3,
        "anilist" => 4,
        "fanart" => 9,
        _ => 5,
    }
}

/// Key inside `selection_origins_json` carrying a merge's pending target fallback (R21).
pub(crate) const PENDING_TARGET_KEY: &str = "$pending_target";

/// A snapshot (or some of its fields) kept by a Media merge although it does not
/// describe the chosen refresh target. Its values are served as a fallback, marked
/// `pending_target_fallback`, until the target itself is adopted for this key —
/// adoption rewrites `selection_origins_json`, which resolves the fallback.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingTarget {
    pub target_external_id: String,
    /// The whole snapshot belongs to a non-target entity.
    #[serde(default)]
    pub whole: bool,
    /// Per-field fallback from the non-target snapshot the merge superseded: only
    /// fields the winning target snapshot does not carry (`Missing`).
    #[serde(default)]
    pub fields: MetadataFields,
    #[serde(default)]
    pub from_entity_id: String,
}

/// One current source snapshot of an entity/locale.
pub(crate) struct SourceSnapshot {
    source: String,
    provider_entity_id: String,
    revision: i64,
    fields: MetadataFields,
    origins: BTreeMap<String, SourceFieldOrigin>,
    pending: Option<PendingTarget>,
}

/// Split `selection_origins_json` into per-field origins and the pending marker.
pub(crate) fn parse_selection_origins(
    raw: &str,
) -> (BTreeMap<String, SourceFieldOrigin>, Option<PendingTarget>) {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str::<serde_json::Value>(raw)
    else {
        return (BTreeMap::new(), None);
    };
    let pending = map
        .remove(PENDING_TARGET_KEY)
        .and_then(|value| serde_json::from_value(value).ok());
    let origins = serde_json::from_value(serde_json::Value::Object(map)).unwrap_or_default();
    (origins, pending)
}

pub(crate) type FieldOrigins = BTreeMap<String, EffectiveFieldOrigin>;

/// All current snapshots of one entity/locale, in deterministic priority order
/// (rank, then source name), independent of the order they were adopted in.
pub(crate) fn load_snapshots(
    conn: &Connection,
    media: MediaId,
    scope: EntityScope,
    locale: &str,
) -> Result<Vec<SourceSnapshot>, StoreError> {
    let (season, episode) = scope.season_episode();
    let mut stmt = conn.prepare(
        "SELECT source, provider_entity_id, source_revision, normalized_json, selection_origins_json
         FROM media_metadata_sources
         WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND locale = ?5",
    )?;
    let rows = stmt.query_map(
        params![media.to_string(), scope.as_str(), season, episode, locale],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        },
    )?;
    let mut snapshots = Vec::new();
    for row in rows {
        let (source, provider_entity_id, revision, fields_json, origins_json) = row?;
        let (origins, pending) = parse_selection_origins(&origins_json);
        snapshots.push(SourceSnapshot {
            source,
            provider_entity_id,
            revision,
            fields: serde_json::from_str(&fields_json)?,
            origins,
            pending,
        });
    }
    snapshots.sort_by(|a, b| {
        (source_rank(&a.source), a.source.as_str())
            .cmp(&(source_rank(&b.source), b.source.as_str()))
    });
    Ok(snapshots)
}

pub(crate) fn load_overrides(
    conn: &Connection,
    media: MediaId,
    scope: EntityScope,
    locale: &str,
) -> Result<Option<(MetadataFields, i64)>, StoreError> {
    let (season, episode) = scope.season_episode();
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT fields_json, revision FROM media_metadata_overrides
             WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND locale = ?5",
            params![media.to_string(), scope.as_str(), season, episode, locale],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    match row {
        Some((json, revision)) => Ok(Some((serde_json::from_str(&json)?, revision))),
        None => Ok(None),
    }
}

/// Compose one entity/locale: the highest-priority snapshot that has a value (or an explicit
/// `Empty`) wins each field; a `Missing` field falls through to the next source. Overrides win.
pub(crate) fn compose_entity(
    conn: &Connection,
    media: MediaId,
    scope: EntityScope,
    locale: &str,
) -> Result<(MetadataFields, FieldOrigins), StoreError> {
    let snapshots = load_snapshots(conn, media, scope, locale)?;
    let mut fields = MetadataFields::default();
    let mut origins = FieldOrigins::new();
    compose_sources(&mut fields, &mut origins, &snapshots, media);
    if let Some((overrides, revision)) = load_overrides(conn, media, scope, locale)? {
        apply_overrides(&mut fields, &mut origins, overrides, revision, media);
    }
    Ok((fields, origins))
}

fn origin(
    source: &str,
    provider_entity_id: &str,
    revision: i64,
    media: MediaId,
    empty: bool,
    selection: SourceFieldOrigin,
) -> EffectiveFieldOrigin {
    EffectiveFieldOrigin {
        source: source.to_string(),
        provider_entity_id: provider_entity_id.to_string(),
        source_revision: revision,
        origin_media_id: media,
        presence: if empty {
            FieldPresence::Empty
        } else {
            FieldPresence::Value
        },
        selection,
        pending_target_fallback: false,
        target_external_id: None,
    }
}

fn default_selection(field: &str, empty: bool) -> SourceFieldOrigin {
    SourceFieldOrigin {
        selection: if empty {
            SourceSelection::Empty
        } else {
            SourceSelection::Direct
        },
        response_pointer: format!("/{field}"),
        actual_locale: None,
        certification_country: None,
    }
}

/// Pick one field across ordered snapshots. Returns the winning value and its origin.
fn pick<T: Clone>(
    snapshots: &[SourceSnapshot],
    name: &str,
    media: MediaId,
    get: impl Fn(&MetadataFields) -> &FieldValue<T>,
) -> Option<(FieldValue<T>, EffectiveFieldOrigin)> {
    snapshots.iter().find_map(|snapshot| {
        let value = get(&snapshot.fields);
        if value.is_missing() {
            return None;
        }
        let empty = matches!(value, FieldValue::Empty);
        let selection = snapshot
            .origins
            .get(name)
            .cloned()
            .unwrap_or_else(|| default_selection(name, empty));
        let mut origin = origin(
            &snapshot.source,
            &snapshot.provider_entity_id,
            snapshot.revision,
            media,
            empty,
            selection,
        );
        if let Some(pending) = snapshot.pending.as_ref().filter(|pending| pending.whole) {
            origin.pending_target_fallback = true;
            origin.target_external_id = Some(pending.target_external_id.clone());
        }
        Some((value.clone(), origin))
    })
}

/// Per-field pending fallback: the first snapshot whose merge stash carries `name`.
fn pick_pending<T: Clone>(
    snapshots: &[SourceSnapshot],
    name: &str,
    media: MediaId,
    get: impl Fn(&MetadataFields) -> &FieldValue<T>,
) -> Option<(FieldValue<T>, EffectiveFieldOrigin)> {
    snapshots.iter().find_map(|snapshot| {
        let pending = snapshot.pending.as_ref()?;
        let value = get(&pending.fields);
        if value.is_missing() {
            return None;
        }
        let empty = matches!(value, FieldValue::Empty);
        let mut origin = origin(
            &snapshot.source,
            &pending.from_entity_id,
            snapshot.revision,
            media,
            empty,
            default_selection(name, empty),
        );
        origin.pending_target_fallback = true;
        origin.target_external_id = Some(pending.target_external_id.clone());
        Some((value.clone(), origin))
    })
}

macro_rules! for_each_field {
    ($apply:ident) => {
        $apply!(title, "title");
        $apply!(original_title, "original_title");
        $apply!(overview, "overview");
        $apply!(tagline, "tagline");
        $apply!(release_date, "release_date");
        $apply!(end_date, "end_date");
        $apply!(content_rating, "content_rating");
        $apply!(language, "language");
        $apply!(status, "status");
        $apply!(air_date, "air_date");
        $apply!(poster_url, "poster_url");
        $apply!(backdrop_url, "backdrop_url");
        $apply!(still_url, "still_url");
        $apply!(year, "year");
        $apply!(rating, "rating");
        $apply!(vote_count, "vote_count");
        $apply!(runtime_minutes, "runtime_minutes");
        $apply!(number_of_seasons, "number_of_seasons");
        $apply!(number_of_episodes, "number_of_episodes");
        $apply!(countries, "countries");
        $apply!(studios, "studios");
        $apply!(directors, "directors");
        $apply!(creators, "creators");
        $apply!(genres, "genres");
        $apply!(episode_run_time, "episode_run_time");
        $apply!(cast, "cast");
    };
}

fn compose_sources(
    fields: &mut MetadataFields,
    origins: &mut FieldOrigins,
    snapshots: &[SourceSnapshot],
    media: MediaId,
) {
    macro_rules! compose_field {
        ($field:ident, $name:expr) => {
            if let Some((value, origin)) = pick(snapshots, $name, media, |f| &f.$field)
                .or_else(|| pick_pending(snapshots, $name, media, |f| &f.$field))
            {
                fields.$field = value;
                origins.insert($name.into(), origin);
            }
        };
    }
    for_each_field!(compose_field);
}

fn apply_overrides(
    fields: &mut MetadataFields,
    origins: &mut FieldOrigins,
    overrides: MetadataFields,
    revision: i64,
    media: MediaId,
) {
    macro_rules! apply_override {
        ($field:ident, $name:expr) => {
            if !overrides.$field.is_missing() {
                let empty = matches!(overrides.$field, FieldValue::Empty);
                let mut selection = default_selection($name, empty);
                selection.response_pointer = "/override".into();
                origins.insert(
                    $name.into(),
                    origin("manual", "", revision, media, empty, selection),
                );
                fields.$field = overrides.$field.clone();
            }
        };
    }
    for_each_field!(apply_override);
}

impl Store {
    pub fn effective_metadata_fields(
        &self,
        library: LibraryId,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
    ) -> Result<ComposedMetadata, StoreError> {
        let actual_locale = self
            .resolved_metadata_locale(library, media, locale)?
            .unwrap_or_else(|| locale.to_string());
        let (fields, field_origins) = compose_entity(&self.library, media, scope, &actual_locale)?;
        Ok(ComposedMetadata {
            fields,
            actual_locale,
            field_origins,
        })
    }
}

/// Fields `other` carries that `winner` lacks (`Missing`), or `None` when there are none.
pub(crate) fn missing_fill(
    winner: &MetadataFields,
    other: &MetadataFields,
) -> Option<MetadataFields> {
    let mut fill = MetadataFields::default();
    let mut any = false;
    macro_rules! fill_field {
        ($field:ident, $name:expr) => {
            if winner.$field.is_missing() && !other.$field.is_missing() {
                fill.$field = other.$field.clone();
                any = true;
            }
        };
    }
    for_each_field!(fill_field);
    any.then_some(fill)
}
