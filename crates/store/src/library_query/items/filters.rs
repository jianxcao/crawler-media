
use super::query::Bindings;
use crate::{StoreError, library_query::types::BrowseFilter};

/// One `IN (...)` term lookup over `library_item_terms`; the listed values are OR-ed.
fn term_clause(bindings: &mut Bindings, kind: &str, values: &[String]) -> Option<String> {
    let values: Vec<&String> = values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .collect();
    if values.is_empty() {
        return None;
    }
    let kind = bindings.push(kind.to_string().into());
    let list = bindings.placeholders(values.len(), |index| values[index].clone().into());
    Some(format!(
        "EXISTS (SELECT 1 FROM library_item_terms t WHERE t.library_id = i.library_id \
         AND t.media_id = i.media_id AND t.term_kind = {kind} AND t.term_value IN ({list}))"
    ))
}

fn any_clause(parts: Vec<String>) -> Option<String> {
    match parts.len() {
        0 => None,
        1 => parts.into_iter().next(),
        _ => Some(format!("({})", parts.join(" OR "))),
    }
}

fn decade_clause(bindings: &mut Bindings, decades: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    for decade in decades {
        if decade == "earlier" {
            parts.push("i.year < 1990".to_string());
            continue;
        }
        let Some(start) = decade
            .strip_suffix('s')
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        let from = bindings.push(start.into());
        let until = bindings.push((start + 10).into());
        parts.push(format!("(i.year >= {from} AND i.year < {until})"));
    }
    any_clause(parts)
}

fn runtime_clause(runtimes: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    for bucket in runtimes {
        parts.push(
            match bucket.as_str() {
                "lte60" => "(i.runtime_sort > 0 AND i.runtime_sort <= 60)",
                "60to90" => "(i.runtime_sort > 60 AND i.runtime_sort <= 90)",
                "90to120" => "(i.runtime_sort > 90 AND i.runtime_sort <= 120)",
                "gt120" => "i.runtime_sort > 120",
                _ => continue,
            }
            .to_string(),
        );
    }
    any_clause(parts)
}

fn stock_clause(stock: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    for value in stock {
        match value.as_str() {
            "missing" => parts.push("i.missing = 1".to_string()),
            "unscraped" => parts.push("i.scraped = 0".to_string()),
            _ => {}
        }
    }
    any_clause(parts)
}

fn watch_clause(watch: &str) -> Option<&'static str> {
    Some(match watch {
        "favorite" => "COALESCE(u.favorite, 0) = 1",
        "seen" => "COALESCE(u.seen, 0) = 1",
        "watching" => "(COALESCE(u.seen, 0) = 1 AND COALESCE(u.played, 0) = 0)",
        "played" | "watched" => "COALESCE(u.played, 0) = 1",
        "unwatched" | "unplayed" | "unseen" => "COALESCE(u.seen, 0) = 0",
        _ => return None,
    })
}

/// Every selected dimension as one AND-ed SQL clause. Within a dimension the listed values
/// are OR-ed, matching the previous in-memory filter (`any`).
pub(crate) fn filter_clauses(filter: &BrowseFilter, bindings: &mut Bindings) -> Vec<String> {
    let countries = filter
        .countries
        .iter()
        .map(|value| value.to_uppercase())
        .collect::<Vec<_>>();
    let languages = filter
        .languages
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<Vec<_>>();
    let mut clauses = Vec::new();
    clauses.extend(term_clause(bindings, "genre", &filter.genres));
    clauses.extend(term_clause(bindings, "country", &countries));
    clauses.extend(term_clause(bindings, "resolution", &filter.resolutions));
    clauses.extend(decade_clause(bindings, &filter.decades));
    clauses.extend(runtime_clause(&filter.runtimes));
    if !languages.is_empty() {
        let list = bindings.placeholders(languages.len(), |index| languages[index].clone().into());
        clauses.push(format!("LOWER(i.language) IN ({list})"));
    }
    if let Some(hdr) = filter.hdr {
        clauses.push(if hdr {
            "i.hdr = 1".to_string()
        } else {
            "i.hdr = 0".to_string()
        });
    }
    if let Some(min_rating) = filter.min_rating {
        let value = bindings.push(min_rating.into());
        clauses.push(format!("i.rating_sort >= {value}"));
    }
    if let Some(identity) = &filter.identity {
        clauses.push(match identity.as_str() {
            "provisional" => "i.identity_state = 'provisional'".to_string(),
            _ => "i.identity_state <> 'provisional'".to_string(),
        });
    }
    clauses.extend(stock_clause(&filter.stock));
    if let Some(watch) = filter.watch.as_deref().and_then(watch_clause) {
        clauses.push(watch.to_string());
    }
    if filter.require_image {
        clauses.push(
            "(i.poster_ref IS NOT NULL OR i.backdrop_ref IS NOT NULL OR i.has_still = 1)"
                .to_string(),
        );
    }
    clauses
}

impl BrowseFilter {
    /// Return an error when a filter dimension carries values the query builder does not
    /// understand, instead of silently dropping the dimension.
    pub fn validate(&self) -> Result<(), StoreError> {
        const DECADES: [&str; 5] = ["2020s", "2010s", "2000s", "1990s", "earlier"];
        const RUNTIMES: [&str; 4] = ["lte60", "60to90", "90to120", "gt120"];
        const STOCK: [&str; 2] = ["missing", "unscraped"];
        const WATCH: [&str; 8] = [
            "favorite",
            "seen",
            "watching",
            "played",
            "watched",
            "unwatched",
            "unplayed",
            "unseen",
        ];
        for decade in &self.decades {
            if !DECADES.contains(&decade.as_str()) {
                return Err(StoreError::Invalid(format!(
                    "invalid decade filter: {decade}"
                )));
            }
        }
        for bucket in &self.runtimes {
            if !RUNTIMES.contains(&bucket.as_str()) {
                return Err(StoreError::Invalid(format!(
                    "invalid runtime filter: {bucket}"
                )));
            }
        }
        for value in &self.stock {
            if !STOCK.contains(&value.as_str()) {
                return Err(StoreError::Invalid(format!(
                    "invalid stock filter: {value}"
                )));
            }
        }
        if let Some(watch) = &self.watch
            && !WATCH.contains(&watch.as_str()) {
                return Err(StoreError::Invalid(format!(
                    "invalid watch filter: {watch}"
                )));
            }
        if let Some(identity) = &self.identity
            && !matches!(identity.as_str(), "provisional" | "confirmed") {
                return Err(StoreError::Invalid(format!(
                    "invalid identity filter: {identity}"
                )));
            }
        if let Some(rating) = self.min_rating
            && (!rating.is_finite() || !(0.0..=10.0).contains(&rating)) {
                return Err(StoreError::Invalid(
                    "rating_gte must be between 0 and 10".into(),
                ));
            }
        Ok(())
    }
}
