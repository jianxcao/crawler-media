//! One statement/snapshot: evaluate each selected dimension once per card. The
//! resulting mismatch mask is reused by every "exclude own dimension" bucket.
use std::collections::BTreeMap;

use rusqlite::params_from_iter;

use super::{ALL, Dimension, WATCH_TIER_SQL, without};
use crate::library_query::items::{Bindings, ItemQuerySql, filter_clauses};
use crate::library_query::types::{BrowseFilter, FacetRequest};
use crate::{Store, StoreError};

type Counts = BTreeMap<String, Vec<(String, u64)>>;

fn selected(filter: &BrowseFilter, keep: Dimension) -> BrowseFilter {
    ALL.iter()
        .filter(|d| **d != keep)
        .fold(filter.clone(), |f, d| without(&f, *d))
}
fn eligibility(dimension: Dimension) -> String {
    let bit = 1u32 << ALL.iter().position(|d| *d == dimension).unwrap_or(0);
    format!(
        "(c.common_match AND (c.mismatches & {}) = 0)",
        1023u32 ^ bit
    )
}
fn card_condition(condition: &str) -> String {
    condition
        .replace(WATCH_TIER_SQL, "c.watch_tier")
        .replace("COALESCE(u.favorite, 0)", "c.favorite")
        .replace("i.", "c.")
}

fn plan(
    request: &FacetRequest,
    dimensions: &[Dimension],
) -> Result<(String, Bindings), StoreError> {
    let common = ALL
        .iter()
        .fold(request.filter.clone(), |f, d| without(&f, *d));
    let base = ItemQuerySql::build(request.library_id, &common, request.user_id)?;
    let mut bindings = Bindings::from_values(base.bindings);
    let mut masks = Vec::new();
    for (index, dimension) in ALL.iter().enumerate() {
        let mut own = selected(&request.filter, *dimension);
        own.identity = None;
        own.require_image = false;
        let clauses = filter_clauses(&own, &mut bindings);
        if !clauses.is_empty() {
            masks.push(format!(
                "CASE WHEN ({}) THEN 0 ELSE {} END",
                clauses.join(" AND "),
                1u32 << index
            ));
        }
    }
    let mask = if masks.is_empty() {
        "0".to_string()
    } else {
        masks.join(" + ")
    };
    let cards = format!(
        "cards AS MATERIALIZED (SELECT i.media_id, i.year, i.runtime_sort,
        i.rating_sort, i.hdr, i.missing, i.scraped, i.language,
        {WATCH_TIER_SQL} AS watch_tier, COALESCE(u.favorite,0) AS favorite,
        ({common_match}) AS common_match, ({mask}) AS mismatches
        FROM {from} WHERE i.library_id=?1)",
        common_match = base.where_sql,
        from = base.from
    );
    let mut fixed = Vec::new();
    let mut outputs = vec![
        "SELECT 'total','',COALESCE(SUM(c.common_match AND c.mismatches=0),0) FROM cards c"
            .to_string(),
    ];
    for dimension in dimensions {
        add_dimension(*dimension, &mut bindings, &mut fixed, &mut outputs);
    }
    let aggregates = if fixed.is_empty() {
        String::new()
    } else {
        format!(
            ", fixed AS MATERIALIZED (SELECT {} FROM cards c)",
            fixed.join(",")
        )
    };
    Ok((
        format!("WITH {cards}{aggregates} {}", outputs.join(" UNION ALL ")),
        bindings,
    ))
}

fn add_dimension(
    dimension: Dimension,
    bindings: &mut Bindings,
    fixed: &mut Vec<String>,
    outputs: &mut Vec<String>,
) {
    let key = dimension.key();
    let eligible = eligibility(dimension);
    if let Some(kind) = dimension.term_kind() {
        let kind = bindings.push(kind.to_string().into());
        outputs.push(format!(
            "SELECT '{key}',t.term_value,SUM(CASE WHEN {eligible} THEN 1 ELSE 0 END)
          FROM library_item_terms t JOIN cards c ON c.media_id=t.media_id
          WHERE t.library_id=?1 AND t.term_kind={kind} GROUP BY t.term_value"
        ));
    } else if dimension.value_expr().is_some() {
        outputs.push(format!(
            "SELECT '{key}',LOWER(c.language),SUM(CASE WHEN {eligible} THEN 1 ELSE 0 END)
          FROM cards c WHERE NULLIF(LOWER(c.language),'') IS NOT NULL GROUP BY LOWER(c.language)"
        ));
    } else {
        for (value, condition, always) in dimension.conditional_values() {
            let index = fixed.len();
            let condition = card_condition(&condition);
            fixed.push(format!("COALESCE(SUM(CASE WHEN {eligible} AND ({condition}) THEN 1 ELSE 0 END),0) AS n{index}"));
            fixed.push(format!(
                "COALESCE(MAX(CASE WHEN {condition} THEN 1 ELSE 0 END),0) AS c{index}"
            ));
            let value = bindings.push(value.into());
            let guard = if always {
                "1".to_string()
            } else {
                format!("c{index}=1")
            };
            outputs.push(format!(
                "SELECT '{key}',{value},n{index} FROM fixed WHERE {guard}"
            ));
        }
    }
}

pub(super) fn read(
    store: &Store,
    request: &FacetRequest,
    dimensions: &[Dimension],
) -> Result<(u64, Counts), StoreError> {
    let (sql, bindings) = plan(request, dimensions)?;
    let mut statement = store.library.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(bindings.values()), |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut total = 0;
    let mut groups = Counts::new();
    for row in rows {
        let (key, value, count) = row?;
        if key == "total" {
            total = count.max(0) as u64;
        } else if !value.trim().is_empty() {
            groups
                .entry(key)
                .or_default()
                .push((value, count.max(0) as u64));
        }
    }
    tracing::debug!(library_id=%request.library_id, total, "Library facet snapshot aggregated");
    Ok((total, groups))
}
