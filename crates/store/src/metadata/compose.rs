use std::collections::BTreeMap;
use domain::{
    ComposedMetadata, EffectiveFieldOrigin, EntityScope, FieldPresence, FieldValue,
    LibraryId, MediaId, MetadataFields, SourceFieldOrigin, SourceSelection,
};
use rusqlite::params;
use crate::{Store, StoreError};

impl Store {
    pub fn effective_metadata_fields(
        &self,
        library: LibraryId,
        media: MediaId,
        scope: EntityScope,
        locale: &str,
    ) -> Result<ComposedMetadata, StoreError> {
        let (season, episode) = scope.season_episode();
        let media_str = media.to_string();
        let scope_str = scope.as_str();

        // 1. Resolve actual locale
        let actual_locale = self
            .resolved_metadata_locale(library, media, locale)?
            .unwrap_or_else(|| locale.to_string());

        // 2. Fetch all sources for this entity/locale
        let mut stmt = self.library.prepare(
            "SELECT source, provider_entity_id, source_revision, normalized_json, selection_origins_json
             FROM media_metadata_sources
             WHERE media_id = ?1 AND entity_scope = ?2 AND season = ?3 AND episode = ?4 AND locale = ?5
             ORDER BY fetched_at ASC",
        )?;

        let rows = stmt.query_map(
            params![media_str, scope_str, season, episode, actual_locale],
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

        let mut sources_data = Vec::new();
        for r in rows {
            let (source, prov_id, rev, norm_json, sel_json) = r?;
            let norm_fields: MetadataFields = serde_json::from_str(&norm_json).unwrap_or_default();
            let origins: BTreeMap<String, SourceFieldOrigin> = serde_json::from_str(&sel_json).unwrap_or_default();
            sources_data.push((source, prov_id, rev, norm_fields, origins));
        }

        // Rank function
        fn source_rank(src: &str) -> u8 {
            match src {
                "tmdb" => 0,
                "douban" => 1,
                "tvdb" => 2,
                "bangumi" => 3,
                "anilist" => 4,
                "fanart" => 9,
                _ => 5,
            }
        }

        // Sort sources by priority rank
        sources_data.sort_by_key(|(s, _, _, _, _)| source_rank(s));

        // Compose fields across sources
        let mut composed_fields = MetadataFields::default();
        let mut field_origins: BTreeMap<String, EffectiveFieldOrigin> = BTreeMap::new();

        // Helper macro/closure for field composition
        macro_rules! compose_field {
            ($field_name:ident, $field_str:expr) => {
                for (source, prov_id, rev, fields, origins) in &sources_data {
                    match &fields.$field_name {
                        FieldValue::Value(v) => {
                            if composed_fields.$field_name.is_missing() {
                                composed_fields.$field_name = FieldValue::Value(v.clone());
                                let sel = origins.get($field_str).cloned().unwrap_or(SourceFieldOrigin {
                                    selection: SourceSelection::Direct,
                                    response_pointer: format!("/{}", $field_str),
                                    actual_locale: None,
                                    certification_country: None,
                                });
                                field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                                    source: source.clone(),
                                    provider_entity_id: prov_id.clone(),
                                    source_revision: *rev,
                                    origin_media_id: media,
                                    presence: FieldPresence::Value,
                                    selection: sel,
                                    pending_target_fallback: false,
                                    target_external_id: None,
                                });
                            }
                            break;
                        }
                        FieldValue::Empty => {
                            if composed_fields.$field_name.is_missing() {
                                composed_fields.$field_name = FieldValue::Empty;
                                let sel = origins.get($field_str).cloned().unwrap_or(SourceFieldOrigin {
                                    selection: SourceSelection::Empty,
                                    response_pointer: format!("/{}", $field_str),
                                    actual_locale: None,
                                    certification_country: None,
                                });
                                field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                                    source: source.clone(),
                                    provider_entity_id: prov_id.clone(),
                                    source_revision: *rev,
                                    origin_media_id: media,
                                    presence: FieldPresence::Empty,
                                    selection: sel,
                                    pending_target_fallback: false,
                                    target_external_id: None,
                                });
                            }
                            break;
                        }
                        FieldValue::Missing => {}
                    }
                }
            };
        }

        Self::compose_all_fields(&mut composed_fields, &mut field_origins, &sources_data, media);

        // 3. Overrides take highest precedence
        if let Some((overrides, o_rev)) = self.get_metadata_overrides(media, scope, &actual_locale)? {
            Self::apply_all_overrides(&mut composed_fields, &mut field_origins, overrides, o_rev, media);
        }

        Ok(ComposedMetadata {
            fields: composed_fields,
            actual_locale,
            field_origins,
        })
    }

    fn compose_all_fields(
        composed_fields: &mut MetadataFields,
        field_origins: &mut BTreeMap<String, EffectiveFieldOrigin>,
        sources_data: &[(String, String, i64, MetadataFields, BTreeMap<String, SourceFieldOrigin>)],
        media: MediaId,
    ) {
        macro_rules! compose_field {
            ($field_name:ident, $field_str:expr) => {
                for (source, prov_id, rev, fields, origins) in sources_data {
                    match &fields.$field_name {
                        FieldValue::Value(v) => {
                            if composed_fields.$field_name.is_missing() {
                                composed_fields.$field_name = FieldValue::Value(v.clone());
                                let sel = origins.get($field_str).cloned().unwrap_or(SourceFieldOrigin {
                                    selection: SourceSelection::Direct,
                                    response_pointer: format!("/{}", $field_str),
                                    actual_locale: None,
                                    certification_country: None,
                                });
                                field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                                    source: source.clone(),
                                    provider_entity_id: prov_id.clone(),
                                    source_revision: *rev,
                                    origin_media_id: media,
                                    presence: FieldPresence::Value,
                                    selection: sel,
                                    pending_target_fallback: false,
                                    target_external_id: None,
                                });
                            }
                            break;
                        }
                        FieldValue::Empty => {
                            if composed_fields.$field_name.is_missing() {
                                composed_fields.$field_name = FieldValue::Empty;
                                let sel = origins.get($field_str).cloned().unwrap_or(SourceFieldOrigin {
                                    selection: SourceSelection::Empty,
                                    response_pointer: format!("/{}", $field_str),
                                    actual_locale: None,
                                    certification_country: None,
                                });
                                field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                                    source: source.clone(),
                                    provider_entity_id: prov_id.clone(),
                                    source_revision: *rev,
                                    origin_media_id: media,
                                    presence: FieldPresence::Empty,
                                    selection: sel,
                                    pending_target_fallback: false,
                                    target_external_id: None,
                                });
                            }
                            break;
                        }
                        FieldValue::Missing => {}
                    }
                }
            };
        }

        compose_field!(title, "title");
        compose_field!(original_title, "original_title");
        compose_field!(overview, "overview");
        compose_field!(tagline, "tagline");
        compose_field!(release_date, "release_date");
        compose_field!(end_date, "end_date");
        compose_field!(content_rating, "content_rating");
        compose_field!(language, "language");
        compose_field!(status, "status");
        compose_field!(air_date, "air_date");
        compose_field!(poster_url, "poster_url");
        compose_field!(backdrop_url, "backdrop_url");
        compose_field!(still_url, "still_url");
        compose_field!(year, "year");
        compose_field!(rating, "rating");
        compose_field!(vote_count, "vote_count");
        compose_field!(runtime_minutes, "runtime_minutes");
        compose_field!(number_of_seasons, "number_of_seasons");
        compose_field!(number_of_episodes, "number_of_episodes");
        compose_field!(countries, "countries");
        compose_field!(studios, "studios");
        compose_field!(directors, "directors");
        compose_field!(creators, "creators");
        compose_field!(genres, "genres");
        compose_field!(episode_run_time, "episode_run_time");
        compose_field!(cast, "cast");
    }

    fn apply_all_overrides(
        composed_fields: &mut MetadataFields,
        field_origins: &mut BTreeMap<String, EffectiveFieldOrigin>,
        overrides: MetadataFields,
        o_rev: i64,
        media: MediaId,
    ) {
        macro_rules! apply_override {
            ($field_name:ident, $field_str:expr) => {
                match overrides.$field_name {
                    FieldValue::Value(v) => {
                        composed_fields.$field_name = FieldValue::Value(v);
                        field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                            source: "manual".into(),
                            provider_entity_id: String::new(),
                            source_revision: o_rev,
                            origin_media_id: media,
                            presence: FieldPresence::Value,
                            selection: SourceFieldOrigin {
                                selection: SourceSelection::Direct,
                                response_pointer: "/override".into(),
                                actual_locale: None,
                                certification_country: None,
                            },
                            pending_target_fallback: false,
                            target_external_id: None,
                        });
                    }
                    FieldValue::Empty => {
                        composed_fields.$field_name = FieldValue::Empty;
                        field_origins.insert($field_str.into(), EffectiveFieldOrigin {
                            source: "manual".into(),
                            provider_entity_id: String::new(),
                            source_revision: o_rev,
                            origin_media_id: media,
                            presence: FieldPresence::Empty,
                            selection: SourceFieldOrigin {
                                selection: SourceSelection::Empty,
                                response_pointer: "/override".into(),
                                actual_locale: None,
                                certification_country: None,
                            },
                            pending_target_fallback: false,
                            target_external_id: None,
                        });
                    }
                    FieldValue::Missing => {}
                }
            };
        }

        apply_override!(title, "title");
        apply_override!(original_title, "original_title");
        apply_override!(overview, "overview");
        apply_override!(tagline, "tagline");
        apply_override!(release_date, "release_date");
        apply_override!(end_date, "end_date");
        apply_override!(content_rating, "content_rating");
        apply_override!(language, "language");
        apply_override!(status, "status");
        apply_override!(air_date, "air_date");
        apply_override!(poster_url, "poster_url");
        apply_override!(backdrop_url, "backdrop_url");
        apply_override!(still_url, "still_url");
        apply_override!(year, "year");
        apply_override!(rating, "rating");
        apply_override!(vote_count, "vote_count");
        apply_override!(runtime_minutes, "runtime_minutes");
        apply_override!(number_of_seasons, "number_of_seasons");
        apply_override!(number_of_episodes, "number_of_episodes");
        apply_override!(countries, "countries");
        apply_override!(studios, "studios");
        apply_override!(directors, "directors");
        apply_override!(creators, "creators");
        apply_override!(genres, "genres");
        apply_override!(episode_run_time, "episode_run_time");
        apply_override!(cast, "cast");
    }
}

