//! Actual ordered streams reject better owners before count and bounded paging.
use super::{ContinueQuery, candidates, scope::Sql};
use crate::StoreError;
use domain::LibraryId;
use rusqlite::types::Value;

pub(super) struct Query {
    pub sql: String,
    pub values: Vec<Value>,
}
pub(super) struct Queries {
    pub counts: Vec<Query>,
    pub streams: Vec<Query>,
    pub root: Option<String>,
    pub values: Vec<Value>,
}
#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Next,
    Resume,
}

pub(super) fn queries(
    query: &ContinueQuery,
    libraries: &[LibraryId],
    purpose: Purpose,
) -> Result<Queries, StoreError> {
    let mut scope = Sql::protocol(query, libraries)?;
    let whole =
        query.media_id.is_some() || matches!(&query.parent, domain::ProtocolParent::Series(_));
    let rows = candidates::candidates(&scope, purpose, whole, "m", false);
    let owners = candidates::candidates(&scope, purpose, whole, "o", true);
    let mut counts = Vec::new();
    let mut streams = Vec::new();
    let depth = query
        .offset
        .saturating_add(query.limit.min(500))
        .min(i64::MAX as usize);
    let limit = scope.bind(depth as i64);
    for row in rows {
        let anti_owner = owners
            .iter()
            .map(|other| {
                format!(
                    "NOT EXISTS(SELECT 1 FROM {} WHERE {} AND {}={} AND {})",
                    other.from,
                    other.filter,
                    other.media,
                    row.media,
                    other.beats(&row, purpose)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND ");
        let base = format!(
            "WITH {} SELECT {{fields}} FROM {} WHERE {} AND {anti_owner}",
            scope.allowed, row.from, row.filter
        );
        counts.push(Query {
            sql: base.replace("{fields}", "COUNT(*)"),
            values: scope.values[..scope.values.len() - 1].to_vec(),
        });
        let order = if row.order.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", row.order)
        };
        streams.push(Query {
            sql: format!(
                "{}{order} LIMIT {limit}",
                base.replace("{fields}", &row.fields())
            ),
            values: scope.values.clone(),
        });
    }
    Ok(Queries {
        counts,
        streams,
        root: scope.root,
        values: scope.values,
    })
}

/// A physical source is selected without sorting its candidates, at page size.
pub(super) fn source_sql(alias: &str, root: Option<&str>) -> String {
    let root_filter = |name: &str| {
        root.map(|root| format!(" AND {name}.root_id={root}"))
            .unwrap_or_default()
    };
    let filter = root_filter("f");
    let better_filter = root_filter("bf");
    format!(
        "SELECT fu.file_id FROM library_file_units fu JOIN library_files f ON f.file_id=fu.file_id WHERE fu.library_id={alias}.library_id AND fu.media_id={alias}.media_id AND fu.season={alias}.season AND fu.episode={alias}.episode AND f.publication_state='published' AND +f.presence_state='present' {filter} AND NOT EXISTS(SELECT 1 FROM library_file_units bu JOIN library_files bf ON bf.file_id=bu.file_id WHERE bu.library_id=fu.library_id AND bu.media_id=fu.media_id AND bu.season=fu.season AND bu.episode=fu.episode AND bf.publication_state='published' AND bf.presence_state='present' {better_filter} AND (bf.file_revision>f.file_revision OR (bf.file_revision=f.file_revision AND bf.file_id<f.file_id))) LIMIT 1"
    )
}
