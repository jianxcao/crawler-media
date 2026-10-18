use std::collections::BTreeMap;
use serde_json::Value;

use domain::{
    EntityScope, FieldValue, MetadataCast, MetadataFields, MetadataGenre, MetadataPreferences,
    NormalizedMetadata, SourceFieldOrigin, SourceSelection,
};

use crate::client::TmdbError;

pub fn normalize_metadata(
    source: &str,
    scope: EntityScope,
    body: &str,
    preferences: &MetadataPreferences,
) -> Result<NormalizedMetadata, TmdbError> {
    let json: Value = serde_json::from_str(body)?;
    let mut fields = MetadataFields::default();
    let mut origins = BTreeMap::new();

    if source == "tmdb" {
        normalize_tmdb(&json, scope, preferences, &mut fields, &mut origins)?;
    } else {
        // Fallback for non-tmdb sources
        normalize_generic(&json, &mut fields, &mut origins);
    }

    Ok(NormalizedMetadata {
        fields,
        field_origins: origins,
    })
}

fn normalize_tmdb(
    json: &Value,
    scope: EntityScope,
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) -> Result<(), TmdbError> {
    // 1. Direct text fields
    extract_direct_string(json, "/title", "title", fields, origins);
    extract_direct_string(json, "/original_title", "original_title", fields, origins);
    extract_direct_string(json, "/overview", "overview", fields, origins);
    extract_direct_string(json, "/tagline", "tagline", fields, origins);
    extract_direct_string(json, "/release_date", "release_date", fields, origins);
    extract_direct_string(json, "/last_air_date", "end_date", fields, origins);
    extract_direct_string(json, "/original_language", "language", fields, origins);
    extract_direct_string(json, "/status", "status", fields, origins);
    extract_direct_string(json, "/poster_path", "poster_url", fields, origins);
    extract_direct_string(json, "/backdrop_path", "backdrop_url", fields, origins);

    // 2. Translations fallback for overview & tagline
    if let Some(trans_arr) = json.pointer("/translations/translations").and_then(Value::as_array) {
        check_translations(trans_arr, preferences, fields, origins);
    }

    // 3. Content rating / Certification
    match scope {
        EntityScope::Work => {
            // Movie: /release_dates/results
            if let Some(rd_arr) = json.pointer("/release_dates/results").and_then(Value::as_array) {
                check_movie_certifications(rd_arr, preferences, fields, origins);
            }
            // TV: /content_ratings/results
            if let Some(cr_arr) = json.pointer("/content_ratings/results").and_then(Value::as_array) {
                check_tv_content_ratings(cr_arr, preferences, fields, origins);
            }
        }
        _ => {}
    }

    // 4. Genres
    if let Some(genres_val) = json.get("genres") {
        if let Some(arr) = genres_val.as_array() {
            let mut list = Vec::new();
            for (idx, g) in arr.iter().enumerate() {
                let id = g.get("id").and_then(|v| v.as_i64()).map(|n| n.to_string());
                let name = g.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                let filter_key = id.clone().unwrap_or_else(|| format!("name:{}", name.to_lowercase()));
                list.push(MetadataGenre {
                    source: "tmdb".into(),
                    source_id: id,
                    name,
                    filter_key,
                });
            }
            fields.genres = FieldValue::Value(list);
            origins.insert(
                "genres".into(),
                SourceFieldOrigin {
                    selection: SourceSelection::Direct,
                    response_pointer: "/genres".into(),
                    actual_locale: None,
                    certification_country: None,
                },
            );
        } else if genres_val.is_null() {
            fields.genres = FieldValue::Empty;
            origins.insert(
                "genres".into(),
                SourceFieldOrigin {
                    selection: SourceSelection::Empty,
                    response_pointer: "/genres".into(),
                    actual_locale: None,
                    certification_country: None,
                },
            );
        }
    }

    Ok(())
}

fn extract_direct_string(
    json: &Value,
    pointer: &str,
    field_name: &str,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    let key = pointer.trim_start_matches('/');
    if let Some(v) = json.get(key) {
        if let Some(s) = v.as_str() {
            if s.is_empty() {
                set_field(fields, field_name, FieldValue::Empty);
                origins.insert(
                    field_name.to_string(),
                    SourceFieldOrigin {
                        selection: SourceSelection::Empty,
                        response_pointer: pointer.to_string(),
                        actual_locale: None,
                        certification_country: None,
                    },
                );
            } else {
                set_field(fields, field_name, FieldValue::Value(s.to_string()));
                origins.insert(
                    field_name.to_string(),
                    SourceFieldOrigin {
                        selection: SourceSelection::Direct,
                        response_pointer: pointer.to_string(),
                        actual_locale: None,
                        certification_country: None,
                    },
                );
            }
        } else if v.is_null() {
            set_field(fields, field_name, FieldValue::Empty);
            origins.insert(
                field_name.to_string(),
                SourceFieldOrigin {
                    selection: SourceSelection::Empty,
                    response_pointer: pointer.to_string(),
                    actual_locale: None,
                    certification_country: None,
                },
            );
        }
    }
}

fn set_field(fields: &mut MetadataFields, field_name: &str, val: FieldValue<String>) {
    match field_name {
        "title" => fields.title = val,
        "original_title" => fields.original_title = val,
        "overview" => fields.overview = val,
        "tagline" => fields.tagline = val,
        "release_date" => fields.release_date = val,
        "end_date" => fields.end_date = val,
        "language" => fields.language = val,
        "status" => fields.status = val,
        "poster_url" => fields.poster_url = val,
        "backdrop_url" => fields.backdrop_url = val,
        _ => {}
    }
}

fn check_translations(
    translations: &[Value],
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    for pref_lang in &preferences.language_priority {
        let (req_lang, req_country) = parse_locale(pref_lang);

        for (idx, tr) in translations.iter().enumerate() {
            let iso_lang = tr.get("iso_639_1").and_then(Value::as_str).unwrap_or_default();
            let iso_country = tr.get("iso_3166_1").and_then(Value::as_str);

            let lang_matches = iso_lang.eq_ignore_ascii_case(req_lang);
            let country_matches = match (req_country, iso_country) {
                (Some(c1), Some(c2)) => c1.eq_ignore_ascii_case(c2),
                (None, _) => true,
                _ => false,
            };

            if lang_matches && country_matches {
                let data = tr.get("data");
                if let Some(ov) = data.and_then(|d| d.get("overview")).and_then(Value::as_str) {
                    if !ov.is_empty() {
                        let actual_locale = match iso_country {
                            Some(c) if !c.is_empty() => Some(format!("{iso_lang}-{c}")),
                            _ => Some(iso_lang.to_string()),
                        };
                        fields.overview = FieldValue::Value(ov.to_string());
                        origins.insert(
                            "overview".into(),
                            SourceFieldOrigin {
                                selection: SourceSelection::Translation,
                                response_pointer: format!("/translations/translations/{idx}/data/overview"),
                                actual_locale: actual_locale.clone(),
                                certification_country: None,
                            },
                        );
                    }
                }
                if let Some(tag) = data.and_then(|d| d.get("tagline")).and_then(Value::as_str) {
                    let actual_locale = match iso_country {
                        Some(c) if !c.is_empty() => Some(format!("{iso_lang}-{c}")),
                        _ => Some(iso_lang.to_string()),
                    };
                    if tag.is_empty() {
                        fields.tagline = FieldValue::Empty;
                        origins.insert(
                            "tagline".into(),
                            SourceFieldOrigin {
                                selection: SourceSelection::Empty,
                                response_pointer: format!("/translations/translations/{idx}/data/tagline"),
                                actual_locale,
                                certification_country: None,
                            },
                        );
                    } else {
                        fields.tagline = FieldValue::Value(tag.to_string());
                        origins.insert(
                            "tagline".into(),
                            SourceFieldOrigin {
                                selection: SourceSelection::Translation,
                                response_pointer: format!("/translations/translations/{idx}/data/tagline"),
                                actual_locale,
                                certification_country: None,
                            },
                        );
                    }
                }
                if origins.contains_key("overview") && origins.contains_key("tagline") {
                    return;
                }
            }
        }
    }
}

fn parse_locale(loc: &str) -> (&str, Option<&str>) {
    let mut parts = loc.split(['-', '_']);
    let lang = parts.next().unwrap_or(loc);
    let country = parts.next();
    (lang, country)
}

fn check_movie_certifications(
    results: &[Value],
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    for pref_country in &preferences.cert_country_priority {
        for (res_idx, r) in results.iter().enumerate() {
            let country = r.get("iso_3166_1").and_then(Value::as_str).unwrap_or_default();
            if country.eq_ignore_ascii_case(pref_country) {
                if let Some(dates) = r.get("release_dates").and_then(Value::as_array) {
                    for (date_idx, d) in dates.iter().enumerate() {
                        if let Some(cert) = d.get("certification").and_then(Value::as_str) {
                            if !cert.is_empty() {
                                fields.content_rating = FieldValue::Value(cert.to_string());
                                origins.insert(
                                    "content_rating".into(),
                                    SourceFieldOrigin {
                                        selection: SourceSelection::Certification,
                                        response_pointer: format!(
                                            "/release_dates/results/{res_idx}/release_dates/{date_idx}/certification"
                                        ),
                                        actual_locale: None,
                                        certification_country: Some(country.to_string()),
                                    },
                                );
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn check_tv_content_ratings(
    results: &[Value],
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    for pref_country in &preferences.cert_country_priority {
        for (idx, r) in results.iter().enumerate() {
            let country = r.get("iso_3166_1").and_then(Value::as_str).unwrap_or_default();
            if country.eq_ignore_ascii_case(pref_country) {
                if let Some(rating) = r.get("rating").and_then(Value::as_str) {
                    if !rating.is_empty() {
                        fields.content_rating = FieldValue::Value(rating.to_string());
                        origins.insert(
                            "content_rating".into(),
                            SourceFieldOrigin {
                                selection: SourceSelection::Certification,
                                response_pointer: format!("/content_ratings/results/{idx}/rating"),
                                actual_locale: None,
                                certification_country: Some(country.to_string()),
                            },
                        );
                        return;
                    }
                }
            }
        }
    }
}

fn normalize_generic(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    extract_direct_string(json, "/title", "title", fields, origins);
    extract_direct_string(json, "/overview", "overview", fields, origins);
}
