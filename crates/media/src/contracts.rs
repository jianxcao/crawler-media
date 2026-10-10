//! Envelope checks supplement endpoint decoders whose optional fields are deliberate.

use serde_json::Value;

use crate::client::TmdbError;

pub(crate) fn json(body: &str) -> Result<Value, TmdbError> {
    let value: Value = serde_json::from_str(body)?;
    let rejected = match &value {
        Value::Array(rows) => rows.iter().any(error_envelope),
        _ => error_envelope(&value),
    };
    if rejected {
        return Err(TmdbError::Parse(
            "Metadata source returned an error envelope".into(),
        ));
    }
    Ok(value)
}

fn error_envelope(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let has_error = ["error", "errors"].iter().any(|key| {
        object.get(*key).is_some_and(|error| match error {
            Value::Null => false,
            Value::Array(rows) => !rows.is_empty(),
            Value::String(text) => !text.is_empty(),
            Value::Bool(flag) => *flag,
            _ => true,
        })
    });
    let failed_status = object
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "error"
                    | "failed"
                    | "failure"
                    | "denied"
                    | "unauthorized"
                    | "forbidden"
                    | "not found"
            )
        });
    // TMDB status envelopes use 1/12/13 for success; other codes are errors, not catalog rows.
    let failed_tmdb = object
        .get("status_code")
        .is_some_and(|code| !matches!(code.as_i64(), Some(1 | 12 | 13)));
    has_error || failed_status || failed_tmdb || object.get("success") == Some(&Value::Bool(false))
}

/// Keep optional rows/fields tolerant, but require an identifiable endpoint body.
pub(crate) fn object(body: &str, fields: &[&str]) -> Result<(), TmdbError> {
    let value = json(body)?;
    let object = value
        .as_object()
        .ok_or_else(|| TmdbError::Parse("expected Metadata endpoint object".into()))?;
    if !fields.iter().any(|field| object.contains_key(*field)) {
        return Err(TmdbError::Parse("missing Metadata endpoint fields".into()));
    }
    Ok(())
}

pub(crate) fn metadata(body: &str) -> Result<(), TmdbError> {
    let value = json(body)?;
    if value
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "success" | "ok"
            )
        })
    {
        return Err(TmdbError::Parse(
            "API status envelope is not TMDB item metadata".into(),
        ));
    }
    // Rich-metadata fixtures can omit identity: preserve that intentional field tolerance.
    object(
        body,
        &[
            "id",
            "overview",
            "tagline",
            "vote_average",
            "vote_count",
            "runtime",
            "episode_run_time",
            "release_date",
            "first_air_date",
            "last_air_date",
            "original_language",
            "status",
            "number_of_seasons",
            "number_of_episodes",
            "genres",
            "origin_country",
            "credits",
            "aggregate_credits",
            "created_by",
            "production_companies",
            "networks",
            "release_dates",
            "content_ratings",
            "translations",
        ],
    )
}

/// TMDB paged lists intentionally default omitted rows and totals, including `{}`.
pub(crate) fn paged(body: &str) -> Result<(), TmdbError> {
    let value = json(body)?;
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(());
    }
    object(body, &["results", "page", "total_pages", "total_results"])
}
