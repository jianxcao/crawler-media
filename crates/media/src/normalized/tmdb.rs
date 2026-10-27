use domain::{
    EntityScope, FieldValue, MetadataCast, MetadataFields, MetadataGenre, MetadataPreferences,
    SourceFieldOrigin, SourceSelection,
};
use serde_json::Value;
use std::collections::BTreeMap;

use super::generic::extract_direct_string;
use super::selection::parse_locale;
use crate::client::TmdbError;

pub fn normalize_tmdb(
    json: &Value,
    scope: EntityScope,
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) -> Result<(), TmdbError> {
    // 1. Direct text fields
    extract_direct_string(json, "/title", "title", fields, origins);
    if fields.title.is_missing() {
        extract_direct_string(json, "/name", "title", fields, origins);
    }
    extract_direct_string(json, "/original_title", "original_title", fields, origins);
    if fields.original_title.is_missing() {
        extract_direct_string(json, "/original_name", "original_title", fields, origins);
    }
    extract_direct_string(json, "/overview", "overview", fields, origins);
    extract_direct_string(json, "/tagline", "tagline", fields, origins);
    extract_direct_string(json, "/release_date", "release_date", fields, origins);
    if fields.release_date.is_missing() {
        extract_direct_string(json, "/first_air_date", "release_date", fields, origins);
    }
    extract_direct_string(json, "/last_air_date", "end_date", fields, origins);
    extract_direct_string(json, "/air_date", "air_date", fields, origins);
    extract_direct_string(json, "/original_language", "language", fields, origins);
    extract_direct_string(json, "/status", "status", fields, origins);
    extract_direct_string(json, "/poster_path", "poster_url", fields, origins);
    extract_direct_string(json, "/backdrop_path", "backdrop_url", fields, origins);
    extract_direct_string(json, "/still_path", "still_url", fields, origins);

    // Numeric fields
    extract_numbers(json, fields, origins);

    // Translations fallback for overview & tagline
    if let Some(trans_arr) = json
        .pointer("/translations/translations")
        .and_then(Value::as_array)
    {
        check_translations(trans_arr, preferences, fields, origins);
    }

    // Content rating / Certification
    if scope == EntityScope::Work {
        if let Some(rd_arr) = json
            .pointer("/release_dates/results")
            .and_then(Value::as_array)
        {
            check_movie_certifications(rd_arr, preferences, fields, origins);
        }
        if let Some(cr_arr) = json
            .pointer("/content_ratings/results")
            .and_then(Value::as_array)
        {
            check_tv_content_ratings(cr_arr, preferences, fields, origins);
        }
    }

    // Genres
    extract_genres(json, fields, origins);

    // Countries & Studios
    extract_countries_and_studios(json, fields, origins);

    // Credits / Cast / Crew
    extract_credits(json, fields, origins);

    Ok(())
}

fn extract_numbers(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    if let Some(val) = json.get("runtime") {
        if let Some(n) = val.as_u64() {
            fields.runtime_minutes = FieldValue::Value(n as u32);
            origins.insert(
                "runtime_minutes".into(),
                SourceFieldOrigin {
                    selection: SourceSelection::Direct,
                    response_pointer: "/runtime".into(),
                    actual_locale: None,
                    certification_country: None,
                },
            );
        } else if val.is_null() {
            fields.runtime_minutes = FieldValue::Empty;
        }
    }
    if let Some(val) = json.get("vote_average")
        && let Some(n) = val.as_f64()
    {
        fields.rating = FieldValue::Value(n);
        origins.insert(
            "rating".into(),
            SourceFieldOrigin {
                selection: SourceSelection::Direct,
                response_pointer: "/vote_average".into(),
                actual_locale: None,
                certification_country: None,
            },
        );
    }
    if let Some(val) = json.get("vote_count")
        && let Some(n) = val.as_u64()
    {
        fields.vote_count = FieldValue::Value(n);
        origins.insert(
            "vote_count".into(),
            SourceFieldOrigin {
                selection: SourceSelection::Direct,
                response_pointer: "/vote_count".into(),
                actual_locale: None,
                certification_country: None,
            },
        );
    }
}

fn extract_genres(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    if let Some(genres_val) = json.get("genres") {
        if let Some(arr) = genres_val.as_array() {
            let mut list = Vec::new();
            for g in arr {
                let id = g.get("id").and_then(|v| v.as_i64()).map(|n| n.to_string());
                let name = g
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let filter_key = id
                    .clone()
                    .unwrap_or_else(|| format!("name:{}", name.to_lowercase()));
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
}

fn extract_countries_and_studios(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    if let Some(arr) = json.get("production_countries").and_then(Value::as_array) {
        let countries: Vec<String> = arr
            .iter()
            .filter_map(|c| {
                c.get("iso_3166_1")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect();
        fields.countries = FieldValue::Value(countries);
        origins.insert(
            "countries".into(),
            SourceFieldOrigin {
                selection: SourceSelection::Direct,
                response_pointer: "/production_countries".into(),
                actual_locale: None,
                certification_country: None,
            },
        );
    }
    if let Some(arr) = json.get("production_companies").and_then(Value::as_array) {
        let studios: Vec<String> = arr
            .iter()
            .filter_map(|c| {
                c.get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .collect();
        fields.studios = FieldValue::Value(studios);
        origins.insert(
            "studios".into(),
            SourceFieldOrigin {
                selection: SourceSelection::Direct,
                response_pointer: "/production_companies".into(),
                actual_locale: None,
                certification_country: None,
            },
        );
    }
}

fn extract_credits(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    let cast_arr = json
        .pointer("/credits/cast")
        .and_then(Value::as_array)
        .or_else(|| json.get("cast").and_then(Value::as_array));
    if let Some(arr) = cast_arr {
        let mut cast = Vec::new();
        for c in arr {
            let name = c
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let role = c
                .get("character")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let tmdb_id = c.get("id").and_then(|v| v.as_i64()).map(|n| n.to_string());
            let thumb = c
                .get("profile_path")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let order = c.get("order").and_then(|v| v.as_u64()).map(|n| n as u32);
            cast.push(MetadataCast {
                name,
                role,
                tmdb_id,
                thumb,
                order,
            });
        }
        fields.cast = FieldValue::Value(cast);
        origins.insert(
            "cast".into(),
            SourceFieldOrigin {
                selection: SourceSelection::Direct,
                response_pointer: "/credits/cast".into(),
                actual_locale: None,
                certification_country: None,
            },
        );
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
            let iso_lang = tr
                .get("iso_639_1")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let iso_country = tr.get("iso_3166_1").and_then(Value::as_str);

            let lang_matches = iso_lang.eq_ignore_ascii_case(req_lang);
            let country_matches = match (req_country, iso_country) {
                (Some(c1), Some(c2)) => c1.eq_ignore_ascii_case(c2),
                (None, _) => true,
                _ => false,
            };

            if lang_matches && country_matches {
                let data = tr.get("data");
                if let Some(ov) = data.and_then(|d| d.get("overview")).and_then(Value::as_str)
                    && !ov.is_empty()
                {
                    let actual_locale = match iso_country {
                        Some(c) if !c.is_empty() => Some(format!("{iso_lang}-{c}")),
                        _ => Some(iso_lang.to_string()),
                    };
                    fields.overview = FieldValue::Value(ov.to_string());
                    origins.insert(
                        "overview".into(),
                        SourceFieldOrigin {
                            selection: SourceSelection::Translation,
                            response_pointer: format!(
                                "/translations/translations/{idx}/data/overview"
                            ),
                            actual_locale: actual_locale.clone(),
                            certification_country: None,
                        },
                    );
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
                                response_pointer: format!(
                                    "/translations/translations/{idx}/data/tagline"
                                ),
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
                                response_pointer: format!(
                                    "/translations/translations/{idx}/data/tagline"
                                ),
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

fn check_movie_certifications(
    results: &[Value],
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    for pref_country in &preferences.cert_country_priority {
        for (res_idx, r) in results.iter().enumerate() {
            let country = r
                .get("iso_3166_1")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if country.eq_ignore_ascii_case(pref_country)
                && let Some(dates) = r.get("release_dates").and_then(Value::as_array)
            {
                for (date_idx, d) in dates.iter().enumerate() {
                    if let Some(cert) = d.get("certification").and_then(Value::as_str)
                        && !cert.is_empty()
                    {
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

fn check_tv_content_ratings(
    results: &[Value],
    preferences: &MetadataPreferences,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    for pref_country in &preferences.cert_country_priority {
        for (idx, r) in results.iter().enumerate() {
            let country = r
                .get("iso_3166_1")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if country.eq_ignore_ascii_case(pref_country)
                && let Some(rating) = r.get("rating").and_then(Value::as_str)
                && !rating.is_empty()
            {
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
