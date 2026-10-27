use crate::{
    StoreError,
    library_query::{
        BrowseFilter,
        items::{Bindings, filter_clauses},
    },
};
use domain::{LibraryId, UserId};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value};

#[derive(Debug)]
pub(crate) enum CountStrategy {
    Marginal { dimension: String, value: String },
    ExactMembers,
}

/// Owns SQL and bindings only; execution always uses the caller's read snapshot.
pub(crate) struct BrowseMemberPlan {
    pub(crate) library: LibraryId,
    pub(crate) from: String,
    pub(crate) member_sql: Vec<String>,
    terms: Vec<(String, Vec<String>)>,
    pub(crate) residual: String,
    pub(crate) bindings: Vec<Value>,
    pub(crate) strategy: CountStrategy,
}

fn selected_terms(filter: &BrowseFilter) -> Vec<(String, Vec<String>)> {
    let mut dimensions = Vec::new();
    for (kind, values) in [
        ("genre", filter.genres.clone()),
        (
            "country",
            filter.countries.iter().map(|s| s.to_uppercase()).collect(),
        ),
        ("resolution", filter.resolutions.clone()),
        ("decade", filter.decades.clone()),
        ("runtime", filter.runtimes.clone()),
        (
            "language",
            filter.languages.iter().map(|s| s.to_lowercase()).collect(),
        ),
        ("stock", filter.stock.clone()),
        (
            "hdr",
            filter.hdr.map(|v| vec![v.to_string()]).unwrap_or_default(),
        ),
    ] {
        let mut values = values;
        if matches!(kind, "genre" | "country" | "resolution") {
            values.retain(|v| !v.trim().is_empty());
        }
        values.sort();
        values.dedup();
        if !values.is_empty() {
            dimensions.push((kind.into(), values));
        }
    }
    dimensions
}

pub(crate) fn plan_browse(
    library: LibraryId,
    user: Option<UserId>,
    filter: &BrowseFilter,
) -> Result<BrowseMemberPlan, StoreError> {
    filter.validate()?;
    let mut bindings = Bindings::new();
    let library_ph = bindings.push(library.to_string().into());
    let from = format!(
        "library_items i{}",
        super::user_join(&mut bindings, user, filter.watch.is_some())
    );
    let terms = selected_terms(filter);
    let mut remaining = filter.clone();
    remaining.genres.clear();
    remaining.countries.clear();
    remaining.resolutions.clear();
    remaining.decades.clear();
    remaining.runtimes.clear();
    remaining.stock.clear();
    remaining.hdr = None;
    if !filter.languages.iter().any(String::is_empty) {
        remaining.languages.clear();
    }
    let member_sql = member_sql(
        &terms,
        &library_ph,
        &mut bindings,
        filter.languages.iter().any(String::is_empty),
    );
    let mut clauses = vec![format!("i.library_id={library_ph}")];
    clauses.extend(filter_clauses(&remaining, &mut bindings));
    let residual = filter.identity.is_some()
        || filter.require_image
        || filter.min_rating.is_some()
        || filter.watch.is_some()
        || filter.languages.iter().any(String::is_empty);
    let strategy = match (residual, terms.as_slice()) {
        (false, []) => CountStrategy::Marginal {
            dimension: "all".into(),
            value: "*".into(),
        },
        (false, [(kind, values)]) if values.len() == 1 => CountStrategy::Marginal {
            dimension: kind.clone(),
            value: values[0].clone(),
        },
        _ => CountStrategy::ExactMembers,
    };
    Ok(BrowseMemberPlan {
        library,
        from,
        member_sql,
        residual: clauses.join(" AND "),
        terms: terms
            .into_iter()
            .filter(|(kind, _)| {
                !(kind == "language" && filter.languages.iter().any(String::is_empty))
            })
            .collect(),
        bindings: bindings.values,
        strategy,
    })
}

fn member_sql(
    terms: &[(String, Vec<String>)],
    library: &str,
    bindings: &mut Bindings,
    empty_language: bool,
) -> Vec<String> {
    
    terms.iter().filter(|(kind,_)| !(kind == "language" && empty_language)).map(|(kind,values)| {
        let kind = bindings.push(kind.clone().into());
        let values = bindings.placeholders(values.len(), |i| values[i].clone().into());
        format!("SELECT media_id FROM library_item_terms WHERE library_id={library} AND term_kind={kind} AND term_value IN ({values})")
    }).collect::<Vec<_>>()
}

pub(crate) fn count_browse(tx: &Connection, plan: &BrowseMemberPlan) -> Result<u64, StoreError> {
    let count: i64 = match &plan.strategy {
        CountStrategy::Marginal { dimension, value } => tx.query_row(
            "SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension=?2 AND value=?3",
            params![plan.library.to_string(), dimension, value], |r| r.get(0),
        ).optional()?.unwrap_or(0),
        CountStrategy::ExactMembers => tx.query_row(
            &exact_count_sql(tx, plan)?,
            params_from_iter(plan.bindings.iter()), |r| r.get(0),
        )?,
    };
    tracing::debug!(library = %plan.library, strategy = ?plan.strategy, count, "browse count complete");
    Ok(count.max(0) as u64)
}

/// Marginal sums are safe candidate-size estimates even for overlapping OR values.
/// Only the final distinct membership intersection decides the exact result.
fn exact_count_sql(tx: &Connection, plan: &BrowseMemberPlan) -> Result<String, StoreError> {
    let mut candidates = Vec::new();
    for (index, (kind, values)) in plan.terms.iter().enumerate() {
        let mut size = 0i64;
        for value in values {
            size += tx.query_row("SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension=?2 AND value=?3",params![plan.library.to_string(),kind,value],|r|r.get::<_,i64>(0)).optional()?.unwrap_or(0);
        }
        candidates.push((size, index));
    }
    candidates.sort_unstable();
    let Some((_, first)) = candidates.first() else {
        return Ok(format!(
            "SELECT COUNT(*) FROM {} WHERE {}",
            plan.from, plan.residual
        ));
    };
    let others = candidates
        .iter()
        .skip(1)
        .map(|(_, index)| format!(" AND i.media_id IN ({})", plan.member_sql[*index]))
        .collect::<String>();
    Ok(format!(
        "WITH candidate AS MATERIALIZED (SELECT DISTINCT media_id FROM ({})) SELECT COUNT(*) FROM candidate c CROSS JOIN {} WHERE i.media_id=c.media_id AND {}{}",
        plan.member_sql[*first], plan.from, plan.residual, others
    ))
}
