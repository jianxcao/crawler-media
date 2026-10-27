//! Facet counts from transactionally maintained marginals and scoped member sets.
use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use super::super::{
    facets::{ALL, Dimension, PRIMARY, sort_counts, without},
    items::{Bindings, WATCH_TIER_SQL, filter_clauses},
    types::{BrowseFilter, FacetRequest, FacetTier, LibraryFacets},
};
use crate::StoreError;

type Counts = BTreeMap<String, Vec<(String, u64)>>;

fn selected(filter: &BrowseFilter, dimension: Dimension) -> bool {
    match dimension {
        Dimension::Genre => filter.genres.iter().any(|v| !v.trim().is_empty()),
        Dimension::Country => filter.countries.iter().any(|v| !v.trim().is_empty()),
        Dimension::Decade => !filter.decades.is_empty(),
        Dimension::Watch => filter.watch.is_some(),
        Dimension::Rating => filter.min_rating.is_some(),
        Dimension::Runtime => !filter.runtimes.is_empty(),
        Dimension::Language => !filter.languages.is_empty(),
        Dimension::Resolution => filter.resolutions.iter().any(|v| !v.trim().is_empty()),
        Dimension::Hdr => filter.hdr.is_some(),
        Dimension::Stock => !filter.stock.is_empty(),
    }
}

fn common(filter: &BrowseFilter) -> BrowseFilter {
    ALL.iter().fold(filter.clone(), |f, d| without(&f, *d))
}

/// A term present on every card is an exact tautology, even when other filters
/// are selected. Use the persisted marginal count; never infer this from a page.
fn omit_full_library_terms(
    tx: &Connection,
    request: &FacetRequest,
) -> Result<(FacetRequest, i64), StoreError> {
    let mut effective = request.clone();
    let total: i64 = tx.query_row("SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='all' AND value='*'", [request.library_id.to_string()], |r| r.get(0)).optional()?.unwrap_or(0);
    if request.filter.identity.is_some()
        || request.filter.require_image
        || request.filter.watch.is_some()
        || request.filter.min_rating.is_some()
    {
        return Ok((effective, total));
    }
    for dimension in ALL {
        if !selected(&effective.filter, dimension) {
            continue;
        }
        let Some(values) = term_values(&effective.filter, dimension) else {
            continue;
        };
        if values.len() != 1 {
            continue;
        }
        let Some(kind) = indexed_term_kind(dimension) else {
            continue;
        };
        for value in values {
            let count: Option<i64> = tx.query_row("SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension=?2 AND value=?3",params![request.library_id.to_string(),kind,value],|r|r.get(0)).optional()?;
            if count == Some(total) {
                effective.filter = without(&effective.filter, dimension);
                break;
            }
        }
    }
    Ok((effective, total))
}

fn indexed_term_kind(dimension: Dimension) -> Option<&'static str> {
    dimension.term_kind().or(match dimension {
        Dimension::Language => Some("language"),
        Dimension::Decade => Some("decade"),
        Dimension::Runtime => Some("runtime"),
        Dimension::Rating => Some("rating"),
        Dimension::Hdr => Some("hdr"),
        Dimension::Stock => Some("stock"),
        _ => None,
    })
}

fn term_values(filter: &BrowseFilter, dimension: Dimension) -> Option<Vec<String>> {
    let values = match dimension {
        Dimension::Genre => filter
            .genres
            .iter()
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .collect(),
        Dimension::Country => filter
            .countries
            .iter()
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.to_uppercase())
            .collect(),
        Dimension::Decade => filter.decades.clone(),
        Dimension::Runtime => filter.runtimes.clone(),
        Dimension::Language if filter.languages.iter().any(String::is_empty) => return None,
        Dimension::Language => filter.languages.iter().map(|v| v.to_lowercase()).collect(),
        Dimension::Resolution => filter
            .resolutions
            .iter()
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .collect(),
        Dimension::Hdr => filter.hdr.map(|v| vec![v.to_string()]).unwrap_or_default(),
        Dimension::Stock => filter.stock.clone(),
        _ => return None,
    };
    Some(values)
}

fn member_set(
    filter: &BrowseFilter,
    dimension: Dimension,
    user: Option<domain::UserId>,
    bindings: &mut Bindings,
) -> String {
    if let Some(values) = term_values(filter, dimension) {
        let kind = dimension.term_kind().unwrap_or_else(|| match dimension {
            Dimension::Decade => "decade",
            Dimension::Runtime => "runtime",
            Dimension::Language => "language",
            Dimension::Hdr => "hdr",
            Dimension::Stock => "stock",
            _ => unreachable!(),
        });
        let kind = bindings.push(kind.to_string().into());
        let list = bindings.placeholders(values.len(), |i| values[i].clone().into());
        return format!(
            "SELECT DISTINCT media_id FROM library_item_terms WHERE library_id=?1 AND term_kind={kind} AND term_value IN ({list})"
        );
    }
    let mut own = ALL
        .iter()
        .filter(|d| **d != dimension)
        .fold(filter.clone(), |f, d| without(&f, *d));
    own.identity = None;
    own.require_image = false;
    let join = super::user_join(bindings, user, dimension == Dimension::Watch);
    let clauses = filter_clauses(&own, bindings);
    format!(
        "SELECT i.media_id FROM library_items i{join} WHERE i.library_id=?1 AND {}",
        clauses.join(" AND ")
    )
}

fn set_ctes(
    request: &FacetRequest,
    bindings: &mut Bindings,
) -> (String, Vec<(Dimension, String)>, String) {
    let mut ctes = Vec::new();
    let common = common(&request.filter);
    let clauses = filter_clauses(&common, bindings);
    let base = if clauses.is_empty() {
        "SELECT media_id FROM library_items WHERE library_id=?1".to_string()
    } else {
        format!(
            "SELECT i.media_id FROM library_items i WHERE i.library_id=?1 AND {}",
            clauses.join(" AND ")
        )
    };
    ctes.push(format!("b AS MATERIALIZED ({base})"));
    let chosen = ALL
        .iter()
        .copied()
        .filter(|d| selected(&request.filter, *d))
        .collect::<Vec<_>>();
    for (n, d) in chosen.iter().enumerate() {
        ctes.push(format!(
            "s{n} AS MATERIALIZED ({})",
            member_set(&request.filter, *d, request.user_id, bindings)
        ));
    }
    let mut prior = "b".to_string();
    for n in 0..chosen.len() {
        let name = format!("p{}", n + 1);
        ctes.push(format!(
            "{name} AS MATERIALIZED (SELECT media_id FROM {prior} INTERSECT SELECT media_id FROM s{n})"
        ));
        prior = name;
    }
    let mut suffix = "b".to_string();
    for n in (0..chosen.len()).rev() {
        let name = format!("r{n}");
        ctes.push(format!(
            "{name} AS MATERIALIZED (SELECT media_id FROM {suffix} INTERSECT SELECT media_id FROM s{n})"
        ));
        suffix = name;
    }
    let full = prior;
    let eligible = chosen.iter().enumerate().map(|(n,d)| {
        let left = if n == 0 {"b".into()} else {format!("p{n}")};
        let right = if n+1 == chosen.len() {"b".into()} else {format!("r{}",n+1)};
        if chosen.len() == 1 { return (*d, "b".into()); }
        if n == 0 { return (*d, right); }
        if n + 1 == chosen.len() { return (*d, left); }
        let name = format!("e{n}");
        ctes.push(format!("{name} AS MATERIALIZED (SELECT media_id FROM {left} INTERSECT SELECT media_id FROM {right})"));
        (*d,name)
    }).collect();
    (ctes.join(","), eligible, full)
}

fn watch_condition(value: &str) -> String {
    match value {
        "unwatched" => format!("({WATCH_TIER_SQL})=0"),
        "watching" => format!("({WATCH_TIER_SQL})=1"),
        "played" => format!("({WATCH_TIER_SQL})=2"),
        _ => "COALESCE(u.favorite,0)=1".into(),
    }
}

fn aggregate_cte(name: &str, source: &str) -> String {
    // No `INDEXED BY`: the terms primary key is `(library_id, media_id, ...)`, so
    // `t.library_id=?1 AND t.media_id=e.media_id` is already a key prefix, and
    // the per-card join is exactly what `terms_media` used to supply. Pinning a
    // second index here would re-introduce the b-tree the reorder deleted.
    format!(
        "{name} AS MATERIALIZED (SELECT t.term_kind,t.term_value,COUNT(*) n \
         FROM {source} e CROSS JOIN library_item_terms t \
         WHERE t.library_id=?1 AND t.media_id=e.media_id \
         GROUP BY t.term_kind,t.term_value)"
    )
}

fn count_cte(name: &str, source: &str) -> String {
    format!("{name} AS MATERIALIZED (SELECT COUNT(*) n FROM {source})")
}

fn bucket_count_sql(
    marginal: &str,
    kind: &str,
    value: &str,
    aggregate: &str,
    eligible_count: &str,
    all_total: i64,
) -> String {
    format!(
        "CASE WHEN {marginal}=0 THEN 0 \
         WHEN {marginal}={all_total} THEN (SELECT n FROM {eligible_count}) \
         ELSE COALESCE((SELECT a.n FROM {aggregate} a \
         WHERE a.term_kind='{kind}' AND a.term_value={value}),0) END"
    )
}

/// The four `watch` tiers of the requesting user, in one materialised pass.
///
/// They used to be four separate `COUNT(*)` scans of `user_library_items`. Once `ANALYZE`
/// produced `sqlite_stat1`, the planner moved each of those scans off the *covering* primary
/// key range onto `user_items_activity`, which does not carry `seen`, so every counted row
/// paid for an extra b-tree seek: 136k → 240k VM steps for an unfiltered facet request on the
/// 21k-card fixture. One pass cannot regress that way, and it is four scans' worth of work
/// cheaper.
fn watch_summary_cte() -> String {
    // `watching` is `seen - played` by construction (the tier CASE puts `played=1` in tier 2
    // only when `seen<>0`), so three sums cover all four tiers.
    "w AS MATERIALIZED (SELECT \
       COALESCE(SUM(u.seen<>0),0) n_seen, \
       COALESCE(SUM(u.seen<>0 AND u.played<>0),0) n_played, \
       COALESCE(SUM(u.favorite<>0),0) n_favorite \
     FROM user_library_items u WHERE u.library_id=?1 AND u.user_id=?2)"
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn append_dimension_output(
    request: &FacetRequest,
    dimension: &Dimension,
    eligible: &[(Dimension, String)],
    full: &str,
    aggregate: &str,
    eligible_count: &str,
    all_total: i64,
    filtered: bool,
    marginal: bool,
    watch_summary: Option<&str>,
    bindings: &mut Bindings,
    outputs: &mut Vec<String>,
) {
    let source = eligible
        .iter()
        .find(|(d, _)| d == dimension)
        .map(|(_, n)| n.as_str())
        .unwrap_or(full);
    if *dimension == Dimension::Watch {
        if let Some(summary) = watch_summary {
            // Unfiltered tiers all come from the one materialised pass (?1 library, ?2 user).
            let all = "(SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='all' AND value='*')";
            let key = dimension.key();
            outputs.push(format!(
                "SELECT '{key}','unwatched',COALESCE(({all}) - (SELECT n_seen FROM {summary}),0) WHERE 1"
            ));
            outputs.push(format!(
                "SELECT '{key}','watching',COALESCE((SELECT n_seen - n_played FROM {summary}),0) WHERE 1"
            ));
            outputs.push(format!(
                "SELECT '{key}','played',COALESCE((SELECT n_played FROM {summary}),0) WHERE 1"
            ));
            outputs.push(format!(
                "SELECT '{key}','favorite',COALESCE((SELECT n_favorite FROM {summary}),0) WHERE (SELECT n_favorite FROM {summary}) > 0"
            ));
            return;
        }
        let uid = bindings.push(
            request
                .user_id
                .map(|u| u.to_string())
                .unwrap_or_default()
                .into(),
        );
        for (value, _, always) in dimension.conditional_values() {
            let condition = watch_condition(&value);
            let count = if filtered && request.user_id.is_none() {
                if value == "unwatched" {
                    format!("SELECT n FROM {eligible_count}")
                } else {
                    "SELECT 0".into()
                }
            } else if filtered {
                format!(
                    "SELECT COUNT(*) FROM {source} e LEFT JOIN user_library_items u ON u.library_id=?1 AND u.media_id=e.media_id AND u.user_id={uid} WHERE {condition}"
                )
            } else if value == "unwatched" {
                format!(
                    "SELECT (SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='all' AND value='*') - (SELECT COUNT(*) FROM user_library_items u WHERE u.library_id=?1 AND u.user_id={uid} AND u.seen<>0)"
                )
            } else {
                let predicate = if value == "watching" {
                    "u.seen<>0 AND u.played=0"
                } else if value == "played" {
                    "u.seen<>0 AND u.played<>0"
                } else {
                    "u.favorite<>0"
                };
                format!(
                    "SELECT COUNT(*) FROM user_library_items u WHERE u.library_id=?1 AND u.user_id={uid} AND {predicate}"
                )
            };
            let guard = if always {
                "1".into()
            } else {
                format!(
                    "EXISTS(SELECT 1 FROM user_library_items u WHERE u.library_id=?1 AND u.user_id={uid} AND u.favorite=1)"
                )
            };
            outputs.push(format!(
                "SELECT '{}','{value}',COALESCE(({count}),0) WHERE {guard}",
                dimension.key()
            ));
        }
    } else if let Some(kind) = indexed_term_kind(*dimension) {
        let values = dimension.conditional_values();
        if values.is_empty() {
            let query = if filtered && !marginal {
                let count = bucket_count_sql(
                    "c.member_count",
                    kind,
                    "c.value",
                    aggregate,
                    eligible_count,
                    all_total,
                );
                format!(
                    "SELECT '{}',c.value,{count} FROM library_query_counts c WHERE c.library_id=?1 AND c.surface='browse' AND c.dimension='{kind}'",
                    dimension.key()
                )
            } else {
                format!(
                    "SELECT '{}',c.value,c.member_count FROM library_query_counts c WHERE c.library_id=?1 AND c.surface='browse' AND c.dimension='{kind}'",
                    dimension.key()
                )
            };
            outputs.push(query);
        } else {
            if filtered && !marginal {
                for (value, _, _) in values {
                    let marginal = format!(
                        "COALESCE((SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='{kind}' AND value='{value}'),0)"
                    );
                    let count = bucket_count_sql(
                        &marginal,
                        kind,
                        &format!("'{value}'"),
                        aggregate,
                        eligible_count,
                        all_total,
                    );
                    outputs.push(format!("SELECT '{}','{value}',{count}", dimension.key()));
                }
            } else {
                for (value, _, _) in values {
                    outputs.push(format!("SELECT '{}','{value}',COALESCE((SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='{kind}' AND value='{value}'),0)", dimension.key()));
                }
            }
        }
    }
}

fn query_counts(
    tx: &Connection,
    request: &FacetRequest,
    dimensions: &[Dimension],
) -> Result<(u64, Counts), StoreError> {
    let (effective, all_total) = omit_full_library_terms(tx, request)?;
    let request = &effective;
    let filtered = !ALL.iter().all(|d| !selected(&request.filter, *d))
        || request.filter.identity.is_some()
        || request.filter.require_image;
    let mut bindings = Bindings::new();
    bindings.push(request.library_id.to_string().into());
    let (mut ctes, eligible, full) = if filtered {
        set_ctes(request, &mut bindings)
    } else {
        (String::new(), Vec::new(), String::new())
    };
    // An unfiltered request still needs the requesting user's watch tiers; bind the user once
    // as ?2 and read all four tiers from a single materialised aggregate.
    let watch_summary = if filtered {
        None
    } else {
        bindings.push(
            request
                .user_id
                .map(|u| u.to_string())
                .unwrap_or_default()
                .into(),
        );
        ctes = watch_summary_cte();
        Some("w")
    };
    if filtered {
        ctes.push(',');
        ctes.push_str(&aggregate_cte("a_full", &full));
        ctes.push(',');
        ctes.push_str(&count_cte("n_full", &full));
        for (n, (_, source)) in eligible.iter().enumerate() {
            ctes.push(',');
            ctes.push_str(&aggregate_cte(&format!("a_{n}"), source));
            ctes.push(',');
            ctes.push_str(&count_cte(&format!("n_{n}"), source));
        }
    }
    let mut outputs = Vec::new();
    outputs.push(if filtered {"SELECT 'total','',(SELECT n FROM n_full)".into()} else {
        "SELECT 'total','',COALESCE((SELECT member_count FROM library_query_counts WHERE library_id=?1 AND surface='browse' AND dimension='all' AND value='*'),0)".into()
    });
    for dimension in dimensions {
        let source = eligible
            .iter()
            .find(|(d, _)| d == dimension)
            .map(|(_, source)| source.as_str());
        let marginal = source == Some("b")
            && request.filter.identity.is_none()
            && !request.filter.require_image;
        let aggregate = eligible
            .iter()
            .position(|(d, _)| d == dimension)
            .map(|n| format!("a_{n}"))
            .unwrap_or_else(|| "a_full".into());
        let eligible_count = eligible
            .iter()
            .position(|(d, _)| d == dimension)
            .map(|n| format!("n_{n}"))
            .unwrap_or_else(|| "n_full".into());
        append_dimension_output(
            request,
            dimension,
            &eligible,
            &full,
            &aggregate,
            &eligible_count,
            all_total,
            filtered,
            marginal,
            watch_summary,
            &mut bindings,
            &mut outputs,
        );
    }
    let sql = if ctes.is_empty() {
        outputs.join(" UNION ALL ")
    } else {
        format!("WITH {ctes} {}", outputs.join(" UNION ALL "))
    };
    let mut statement = tx.prepare(&sql)?;
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
        } else {
            groups
                .entry(key)
                .or_default()
                .push((value, count.max(0) as u64));
        }
    }
    Ok((total, groups))
}

/// Borrowed connection permits the caller to retain one snapshot across related reads.
pub(crate) fn read_facets(
    tx: &Connection,
    request: &FacetRequest,
) -> Result<LibraryFacets, StoreError> {
    request.filter.validate()?;
    let dimensions: &[Dimension] = match request.tier {
        FacetTier::Primary => &PRIMARY,
        FacetTier::All => &ALL,
    };
    let (total, mut counts) = query_counts(tx, request, dimensions)?;
    let groups = dimensions
        .iter()
        .map(|d| {
            (
                d.key().to_string(),
                sort_counts(*d, counts.remove(d.key()).unwrap_or_default()),
            )
        })
        .collect();
    tracing::debug!(library_id=%request.library_id,total,"Library facet snapshot aggregated");
    Ok(LibraryFacets { total, groups })
}
