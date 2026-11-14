//! Merge bounded index streams, then hydrate sources for the requested page only.
use super::{ContinueQuery, StoreError, read_rows, sql};
use rusqlite::{Connection, params_from_iter};

struct Logical {
    library: String,
    media: String,
    season: i32,
    episode: i32,
    activity: i64,
}
pub(super) fn count(conn: &Connection, queries: &sql::Queries) -> Result<usize, StoreError> {
    queries.counts.iter().try_fold(0, |total, query| {
        let n: usize = conn
            .prepare_cached(&query.sql)?
            .query_row(params_from_iter(&query.values), |row| row.get(0))?;
        Ok(total + n)
    })
}
fn selected(
    conn: &Connection,
    queries: &sql::Queries,
    request: &ContinueQuery,
) -> Result<Vec<Logical>, StoreError> {
    let limit = request.limit.min(500);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut statements = queries
        .streams
        .iter()
        .map(|query| conn.prepare_cached(&query.sql))
        .collect::<Result<Vec<_>, _>>()?;
    let mut streams = statements
        .iter_mut()
        .zip(&queries.streams)
        .map(|(statement, query)| statement.query(params_from_iter(&query.values)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut heads = streams
        .iter_mut()
        .map(next_logical)
        .collect::<Result<Vec<_>, _>>()?;
    let mut page = Vec::new();
    let end = request.offset.saturating_add(limit);
    for ordinal in 0..end {
        let next = heads
            .iter()
            .enumerate()
            .filter_map(|(index, head)| head.as_ref().map(|row| (index, row)))
            .min_by(|a, b| {
                b.1.activity
                    .cmp(&a.1.activity)
                    .then_with(|| a.1.media.cmp(&b.1.media))
            })
            .map(|row| row.0);
        let Some(next) = next else {
            break;
        };
        if let Some(row) = heads[next].take()
            && ordinal >= request.offset
        {
            page.push(row);
        }
        heads[next] = next_logical(&mut streams[next])?;
    }
    Ok(page)
}
fn next_logical(rows: &mut rusqlite::Rows<'_>) -> Result<Option<Logical>, StoreError> {
    rows.next()?
        .map(|row| {
            Ok(Logical {
                library: row.get(0)?,
                media: row.get(1)?,
                season: row.get(2)?,
                episode: row.get(3)?,
                activity: row.get(4)?,
            })
        })
        .transpose()
}
pub(super) fn page_query(
    conn: &Connection,
    queries: &sql::Queries,
    request: &ContinueQuery,
) -> Result<(sql::Query, Vec<String>), StoreError> {
    let page = selected(conn, queries, request)?;
    let order = page.iter().map(|row| row.media.clone()).collect();
    let page: Vec<_> = page
        .iter()
        .map(|row| {
            serde_json::json!([
                row.library,
                row.media,
                row.season,
                row.episode,
                row.activity
            ])
        })
        .collect();
    let mut values = queries.values.clone();
    values.push(serde_json::to_string(&page)?.into());
    let payload = format!("?{}", values.len());
    let source = sql::source_sql("p", queries.root.as_deref());
    Ok((
        sql::Query {
            sql: format!(
                "WITH p AS (SELECT json_extract(value,'$[0]') AS library_id,json_extract(value,'$[1]') AS media_id,json_extract(value,'$[2]') AS season,json_extract(value,'$[3]') AS episode,json_extract(value,'$[4]') AS activity FROM json_each({payload})) SELECT p.library_id,p.media_id,p.season,p.episode,p.activity,f.file_id,f.file_revision FROM p CROSS JOIN library_files f ON f.file_id=({source})"
            ),
            values,
        },
        order,
    ))
}
pub(super) fn page(
    conn: &Connection,
    queries: &sql::Queries,
    request: &ContinueQuery,
) -> Result<Vec<super::ContinueEntry>, StoreError> {
    let (query, order) = page_query(conn, queries, request)?;
    let rows = read_rows(conn, &query, false)?;
    let mut by_media = rows
        .into_iter()
        .map(|row| (row.reference.item.media_id().to_string(), row))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(order
        .into_iter()
        .filter_map(|media| by_media.remove(&media))
        .map(|row| super::ContinueEntry {
            reference: row.reference,
            last_played_at: row.activity,
        })
        .collect())
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn explain(
    conn: &Connection,
    queries: &sql::Queries,
    request: &ContinueQuery,
) -> Result<Vec<String>, StoreError> {
    let (page, _) = page_query(conn, queries, request)?;
    let mut statements: Vec<_> = queries
        .counts
        .iter()
        .map(|query| ("count", query))
        .collect();
    statements.extend(queries.streams.iter().map(|query| ("stream", query)));
    statements.push(("page-source", &page));
    super::explain(conn, &statements)
}
