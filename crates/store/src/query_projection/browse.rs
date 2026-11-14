use super::types::Terms;
use crate::StoreError;
use rusqlite::{Connection, params};

/// Closed bucket rules shared by live settlement and the raw-fact expectation.
pub(super) fn buckets(
    year: Option<i64>,
    runtime: i64,
    rating: f64,
    language: Option<&str>,
    hdr: bool,
    missing: bool,
    scraped: bool,
) -> Terms {
    let mut terms = Terms::new();
    if let Some(year) = year {
        let decade = match year {
            ..1990 => Some("earlier"),
            1990..2000 => Some("1990s"),
            2000..2010 => Some("2000s"),
            2010..2020 => Some("2010s"),
            2020..2030 => Some("2020s"),
            _ => None,
        };
        if let Some(value) = decade {
            terms.insert(("decade".into(), value.into()));
        }
    }
    let runtime = match runtime {
        1..=60 => Some("lte60"),
        61..=90 => Some("60to90"),
        91..=120 => Some("90to120"),
        121.. => Some("gt120"),
        _ => None,
    };
    if let Some(value) = runtime {
        terms.insert(("runtime".into(), value.into()));
    }
    for threshold in 6..=9 {
        if rating >= f64::from(threshold) {
            terms.insert(("rating".into(), threshold.to_string()));
        }
    }
    if let Some(language) = language.filter(|v| !v.is_empty()) {
        terms.insert(("language".into(), language.to_ascii_lowercase()));
    }
    terms.insert(("hdr".into(), hdr.to_string()));
    if missing {
        terms.insert(("stock".into(), "missing".into()));
    }
    if !scraped {
        terms.insert(("stock".into(), "unscraped".into()));
    }
    terms
}
pub(super) fn from_card(tx: &Connection, lib: &str, media: &str) -> Result<Terms, StoreError> {
    Ok(tx.query_row("SELECT year,runtime_sort,rating_sort,language,hdr,missing,scraped FROM library_items WHERE library_id=?1 AND media_id=?2", params![lib,media], |r| {
        let language: Option<String> = r.get(3)?;
        Ok(buckets(r.get(0)?,r.get(1)?,r.get(2)?,language.as_deref(),r.get(4)?,r.get(5)?,r.get(6)?))
    })?)
}
pub(super) fn is_static(kind: &str) -> bool {
    matches!(
        kind,
        "genre"
            | "country"
            | "resolution"
            | "decade"
            | "runtime"
            | "language"
            | "rating"
            | "hdr"
            | "stock"
    )
}
pub(super) fn read_terms(
    tx: &Connection,
    table: &str,
    lib: &str,
    media: &str,
) -> Result<Terms, StoreError> {
    Ok(tx
        .prepare_cached(&format!(
            "SELECT term_kind,term_value FROM {table} WHERE library_id=?1 AND media_id=?2"
        ))?
        .query_map(params![lib, media], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}
pub(crate) fn sync_terms(
    tx: &Connection,
    table: &str,
    lib: &str,
    media: &str,
    before: &Terms,
    after: &Terms,
) -> Result<(u64, u64), StoreError> {
    for (kind, value) in before.difference(after) {
        tx.execute(&format!("DELETE FROM {table} WHERE library_id=?1 AND media_id=?2 AND term_kind=?3 AND term_value=?4"), params![lib,media,kind,value])?;
    }
    for (kind, value) in after.difference(before) {
        tx.execute(
            &format!(
                "INSERT INTO {table}(library_id,media_id,term_kind,term_value) VALUES(?1,?2,?3,?4)"
            ),
            params![lib, media, kind, value],
        )?;
    }
    Ok((
        after.difference(before).count() as u64,
        before.difference(after).count() as u64,
    ))
}
