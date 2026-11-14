//! LastPlayed streams one activity/zero-activity segment within each watch tier.
use super::*;

#[derive(Clone, Copy)]
struct Segment {
    tier: Option<i64>,
    rank: i64,
    active: bool,
}

struct Boundary {
    tier: Option<i64>,
    rank: i64,
    activity: i64,
    media: String,
}

fn segments(query: &LibraryQuery) -> Vec<Segment> {
    let tiers = if query.filter.unwatched_first {
        vec![Some(0), Some(1), Some(2)]
    } else {
        vec![None]
    };
    tiers
        .into_iter()
        .flat_map(|tier| {
            (0..=1).map(move |rank| Segment {
                tier,
                rank,
                active: (rank == 0) == query.descending,
            })
        })
        .collect()
}

fn boundary(query: &LibraryQuery, spec: &[OrderTerm]) -> Result<Option<Boundary>, StoreError> {
    let BrowsePage::Cursor {
        after: Some(raw), ..
    } = &query.page
    else {
        return Ok(None);
    };
    let cursor = cursor::decode(raw, &cursor::scope(query)?, spec)?;
    let numbers = cursor
        .keys
        .iter()
        .map(|key| match key {
            CursorKey::Int(value) => Ok(*value),
            _ => Err(malformed_cursor()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (tier, index) = if query.filter.unwatched_first {
        (Some(numbers[0]), 1)
    } else {
        (None, 0)
    };
    let rank = numbers[index];
    let activity = numbers[index + 1];
    let active = (rank == 0) == query.descending;
    if !(0..=1).contains(&rank)
        || tier.is_some_and(|tier| !(0..=2).contains(&tier))
        || activity < 0
        || active != (activity > 0)
    {
        return Err(malformed_cursor());
    }
    Ok(Some(Boundary {
        tier,
        rank,
        activity,
        media: cursor.media_id,
    }))
}

fn segment_query(
    query: &LibraryQuery,
    base: &ItemQuerySql,
    segment: Segment,
    after: Option<&Boundary>,
) -> (ItemQuerySql, String) {
    let mut bindings = Bindings::from_values(base.bindings.clone());
    let mut clauses = vec![base.where_sql.clone()];
    let from = if segment.active {
        let user = bindings.push(
            query
                .user_id
                .map(|id| id.to_string())
                .unwrap_or_default()
                .into(),
        );
        let library = bindings.push(query.library_id.to_string().into());
        clauses.push(format!(
            "u.user_id={user} AND u.library_id={library} AND u.last_played_at>0"
        ));
        "user_library_items u INDEXED BY user_items_activity CROSS JOIN library_items i
         ON i.library_id=u.library_id AND i.media_id=u.media_id"
            .to_string()
    } else {
        clauses.push("COALESCE(u.last_played_at,0)<=0".into());
        base.from.clone()
    };
    if let Some(tier) = segment.tier {
        clauses.push(format!("{}={tier}", super::super::WATCH_TIER_SQL));
    }
    let operator = if query.descending { "<" } else { ">" };
    if let Some(after) = after {
        let media = bindings.push(after.media.clone().into());
        clauses.push(if segment.active {
            let activity = bindings.push(after.activity.into());
            format!("(u.last_played_at,u.media_id){operator}({activity},{media})")
        } else {
            format!("i.media_id{operator}{media}")
        });
    }
    let direction = if query.descending { "DESC" } else { "ASC" };
    let order = if segment.active {
        format!("u.last_played_at {direction},u.media_id {direction}")
    } else {
        format!("i.media_id {direction}")
    };
    (
        ItemQuerySql {
            from,
            where_sql: clauses.join(" AND "),
            bindings: bindings.values,
        },
        order,
    )
}

fn keys_sql(spec: &[OrderTerm]) -> String {
    spec.iter()
        .map(|term| term.expr)
        .collect::<Vec<_>>()
        .join(",")
}

fn read_keys(
    row: &Row<'_>,
    spec: &[OrderTerm],
    offset: usize,
) -> Result<Vec<CursorKey>, StoreError> {
    spec.iter()
        .enumerate()
        .map(|(index, term)| read_key(row, offset + index, term.kind))
        .collect()
}

pub(super) fn page(
    conn: &Connection,
    query: &LibraryQuery,
    base: &ItemQuerySql,
    spec: &[OrderTerm],
) -> Result<(PageRows, super::super::stats::QueryWorkStats), StoreError> {
    let after = boundary(query, spec)?;
    let (limit, mut skip, cursor_page) = match &query.page {
        BrowsePage::Cursor { limit, .. } => ((*limit).clamp(1, MAX_ITEM_PAGE), 0, true),
        BrowsePage::Offset { start, limit } => (*limit, *start, false),
    };
    let mut page = PageRows {
        items: Vec::new(),
        last_keys: None,
        truncated: false,
    };
    let mut stats = super::super::stats::QueryWorkStats::default();
    if limit == 0 {
        return Ok((page, stats));
    }
    let columns = format!("{ITEM_COLUMNS},{}", keys_sql(&spec[..spec.len() - 1]));
    for segment in segments(query) {
        let ordinal = (segment.tier, segment.rank);
        if after
            .as_ref()
            .is_some_and(|after| ordinal < (after.tier, after.rank))
        {
            continue;
        }
        let seek = after
            .as_ref()
            .filter(|after| ordinal == (after.tier, after.rank));
        let (mut plan, order) = segment_query(query, base, segment, seek);
        if skip > 0 {
            let count: u64 = conn.query_row(
                &plan.select("COUNT(*)"),
                params_from_iter(plan.bindings.iter().cloned()),
                |row| row.get(0),
            )?;
            if count <= skip as u64 {
                skip -= count as usize;
                continue;
            }
        }
        let mut bindings = Bindings::from_values(plan.bindings);
        let rows = limit
            .saturating_sub(page.items.len())
            .saturating_add(usize::from(cursor_page));
        let rows = bindings.push(i64::try_from(rows).unwrap_or(i64::MAX).into());
        let offset = bindings.push(i64::try_from(skip).unwrap_or(i64::MAX).into());
        plan.bindings = bindings.values;
        let sql = format!(
            "{} ORDER BY {order} LIMIT {rows} OFFSET {offset}",
            plan.select(&columns)
        );
        let mut statement = conn.prepare(&sql)?;
        {
            let mut rows = statement.query(params_from_iter(plan.bindings))?;
            while let Some(row) = rows.next()? {
                stats.rows_returned += 1;
                if page.items.len() == limit {
                    page.truncated = true;
                    break;
                }
                page.items.push(item_from_row(row)?);
                page.last_keys = Some(read_keys(row, &spec[..spec.len() - 1], ITEM_KEY_OFFSET)?);
            }
        }
        stats.fullscan_steps +=
            u64::from(statement.get_status(rusqlite::StatementStatus::FullscanStep) as u32);
        stats.vm_steps += u64::from(statement.get_status(rusqlite::StatementStatus::VmStep) as u32);
        stats.sort_operations +=
            u64::from(statement.get_status(rusqlite::StatementStatus::Sort) as u32);
        if page.truncated || (!cursor_page && page.items.len() == limit) {
            break;
        }
        skip = 0;
    }
    Ok((page, stats))
}

fn visit_all(
    conn: &Connection,
    query: &LibraryQuery,
    spec: &[OrderTerm],
    mut visit: impl FnMut(&Row<'_>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let base = ItemQuerySql::build_for_order(query.library_id, &query.filter, query.user_id, true)?;
    let columns = format!("{},i.title", keys_sql(spec));
    for segment in segments(query) {
        let (plan, order) = segment_query(query, &base, segment, None);
        let mut statement = conn.prepare(&format!("{} ORDER BY {order}", plan.select(&columns)))?;
        let mut rows = statement.query(params_from_iter(plan.bindings))?;
        while let Some(row) = rows.next()? {
            visit(row)?
        }
    }
    Ok(())
}

pub(super) fn titles(conn: &Connection, query: &LibraryQuery) -> Result<Vec<String>, StoreError> {
    let spec = order_spec(query);
    let mut titles = Vec::new();
    visit_all(conn, query, &spec, |row| {
        titles.push(row.get(spec.len())?);
        Ok(())
    })?;
    Ok(titles)
}

pub(super) fn index(
    conn: &Connection,
    query: &LibraryQuery,
) -> Result<Vec<IndexBucket>, StoreError> {
    let spec = order_spec(query);
    let scope = cursor::scope(query)?;
    let mut buckets: Vec<IndexBucket> = Vec::new();
    let mut previous_cursor = None;
    visit_all(conn, query, &spec, |row| {
        let mut keys = read_keys(row, &spec, 0)?;
        let Some(CursorKey::Text(media)) = keys.pop() else {
            return Err(malformed_cursor());
        };
        let title: String = row.get(spec.len())?;
        let initial = bucket_initial(&title);
        let current_cursor = cursor::encode(&scope, keys, &media)?;
        match buckets.last_mut() {
            Some(last) if last.initial == initial => last.count += 1,
            _ => buckets.push(IndexBucket {
                initial,
                count: 1,
                cursor: previous_cursor.clone(),
            }),
        }
        previous_cursor = Some(current_cursor);
        Ok(())
    })?;
    Ok(buckets)
}
