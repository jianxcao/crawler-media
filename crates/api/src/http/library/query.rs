use axum::http::StatusCode;
use axum::response::Response;
use std::collections::HashMap;

use crate::http::err;
use crate::store::library_query;

pub(crate) const BROWSE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn browse_work_error(error: crate::blocking_budget::BlockingError) -> Response {
    use crate::blocking_budget::BlockingError;
    match error {
        BlockingError::Busy => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        BlockingError::Timeout => err(
            StatusCode::GATEWAY_TIMEOUT,
            "library.timeout",
            "浏览请求超时",
        ),
        error => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime.worker",
            &error.to_string(),
        ),
    }
}

/// HTTP query parameters → the store browse filter. A value the filter cannot express is an
/// error the handler turns into 400: a bogus filter must never silently return everything.
/// Shared by the item page, the first-letter index, gallery and facets so one request shape
/// cannot mean different things on different endpoints.
/// Every query key the browse surface reads (filters, ordering, paging and the
/// facet tier). A key outside this set means the caller expects a filter this
/// API cannot express, so it is rejected instead of silently returning a list
/// filtered by less than the caller asked for.
const BROWSE_QUERY_KEYS: [&str; 17] = [
    "g",
    "c",
    "d",
    "rt",
    "lang",
    "res",
    "stock",
    "hdr",
    "rating_gte",
    "identity",
    "w",
    "unwatched_first",
    "sort",
    "order",
    "cursor",
    "limit",
    "tier",
];

pub(crate) fn browse_filter_from_query(
    query: &HashMap<String, String>,
) -> Result<library_query::BrowseFilter, String> {
    let list = |keys: &[&str]| -> Vec<String> {
        keys.iter()
            .find_map(|key| query.get(*key))
            .map(|raw| {
                raw.split(',')
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let flag = |key: &str| -> Result<Option<bool>, String> {
        match query.get(key) {
            Some(raw) => raw
                .parse::<bool>()
                .map(Some)
                .map_err(|_| format!("{key} must be true or false")),
            None => Ok(None),
        }
    };
    // One generic rule, no list of accepted-but-replaced spellings: a key the
    // browse surface does not understand is a 400, whether it never existed or
    // was replaced.
    if let Some(key) = query
        .keys()
        .find(|key| !BROWSE_QUERY_KEYS.contains(&key.as_str()))
    {
        return Err(format!("unknown query key {key}"));
    }
    let rating = match query.get("rating_gte") {
        Some(raw) => {
            let value = raw
                .parse::<f64>()
                .map_err(|_| "rating_gte must be numeric".to_string())?;
            if !value.is_finite() || !(0.0..=10.0).contains(&value) {
                return Err("rating_gte must be between 0 and 10".into());
            }
            Some(value)
        }
        None => None,
    };
    Ok(library_query::BrowseFilter {
        genres: list(&["g"]),
        countries: list(&["c"]),
        decades: list(&["d"]),
        runtimes: list(&["rt"]),
        languages: list(&["lang"]),
        resolutions: list(&["res"]),
        stock: list(&["stock"]),
        hdr: flag("hdr")?,
        min_rating: rating,
        identity: query.get("identity").cloned(),
        watch: query.get("w").cloned(),
        unwatched_first: match query.get("unwatched_first") {
            None => false,
            Some(raw) if raw == "true" || raw == "1" => true,
            Some(raw) if raw == "false" || raw == "0" => false,
            Some(raw) => {
                return Err(format!("unwatched_first must be true or false, got {raw}"));
            }
        },
        require_image: false,
    })
}

/// The page/sort half of a browse request, shared by items, index and gallery.
pub(crate) fn browse_order_from_query(
    query: &HashMap<String, String>,
) -> (library_query::BrowseOrder, bool) {
    let order = match query.get("sort").map(String::as_str) {
        Some("title") => library_query::BrowseOrder::Title,
        Some("added_at") => library_query::BrowseOrder::AddedAt,
        Some("release_date" | "release_date_asc") => library_query::BrowseOrder::ReleaseDate,
        Some("rating") => library_query::BrowseOrder::Rating,
        Some("runtime") => library_query::BrowseOrder::Runtime,
        Some("size") => library_query::BrowseOrder::Size,
        Some("resolution") => library_query::BrowseOrder::Resolution,
        Some("last_played") => library_query::BrowseOrder::LastPlayed,
        Some("random") => library_query::BrowseOrder::Random,
        _ => library_query::BrowseOrder::Title,
    };
    let descending = match query.get("order").map(|value| value.as_str()) {
        Some("desc") => true,
        Some("asc") => false,
        // Same defaults the wall has always used; `release_date_asc` is the explicit
        // ascending spelling of the release-date sort.
        _ => {
            query.get("sort").map(String::as_str) != Some("release_date_asc")
                && matches!(
                    order,
                    library_query::BrowseOrder::AddedAt
                        | library_query::BrowseOrder::ReleaseDate
                        | library_query::BrowseOrder::Rating
                        | library_query::BrowseOrder::Size
                        | library_query::BrowseOrder::LastPlayed
                )
        }
    };
    (order, descending)
}

/// Map a store failure of a browse read to a response, keeping filter rejections a 400.
pub(crate) fn browse_store_error(error: store::StoreError) -> Response {
    if matches!(&error, store::StoreError::QueryUnavailable { .. }) {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "library.reindexing",
            "Library 查询暂时不可用",
        )
    } else if crate::library_read_work::store_error_is_unavailable(&error) {
        err(
            StatusCode::SERVICE_UNAVAILABLE,
            "library.unavailable",
            "Library 查询暂时不可用",
        )
    } else {
        match error {
            store::StoreError::Invalid(message) => {
                err(StatusCode::BAD_REQUEST, "library.invalid", &message)
            }
            error => err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            ),
        }
    }
}
