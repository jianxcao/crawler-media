use domain::{FieldValue, MetadataFields, SourceFieldOrigin, SourceSelection};
use serde_json::Value;
use std::collections::BTreeMap;

pub fn normalize_generic(
    json: &Value,
    fields: &mut MetadataFields,
    origins: &mut BTreeMap<String, SourceFieldOrigin>,
) {
    extract_direct_string(json, "/title", "title", fields, origins);
    extract_direct_string(json, "/overview", "overview", fields, origins);
}

pub fn extract_direct_string(
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

pub fn set_field(fields: &mut MetadataFields, field_name: &str, val: FieldValue<String>) {
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
        "still_url" => fields.still_url = val,
        _ => {}
    }
}
