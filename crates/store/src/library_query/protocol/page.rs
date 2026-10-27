use domain::{ProtocolItemQuery, ProtocolSort, ProtocolSortKey};
use rusqlite::{Connection, params_from_iter, types::Value};

use super::{
    order,
    request::NormalizedProtocolRequest,
    scope::{Granularity, Scope},
    source, sql,
};
use crate::{StoreError, library_query::members::CountStrategy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageStrategy {
    IndexedSingleLibrary,
    GeneralExact,
}

#[derive(Debug)]
pub(crate) struct ProtocolReadPlan {
    pub(crate) count_strategy: CountStrategy,
    pub(crate) page_strategy: PageStrategy,
}

pub(super) fn classify(request: &NormalizedProtocolRequest, scope: &Scope) -> ProtocolReadPlan {
    let q = &request.original;
    let residual = scope.pinned
        || !q.filters.is_empty()
        || q.is_favorite.is_some()
        || q.is_played.is_some()
        || q.search_term
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
    let kind = if q.include_types.len() == 1 {
        match q.include_types[0] {
            domain::ProtocolItemType::Movie => Some("movie"),
            domain::ProtocolItemType::Series => Some("tv"),
            domain::ProtocolItemType::Video => Some("video"),
            _ => None,
        }
    } else {
        None
    };
    let genre_name = if request.studios.is_empty() && request.genres.len() == 2 {
        request
            .genres
            .iter()
            .find(|v| v.starts_with("name:"))
            .map(String::as_str)
    } else {
        None
    };
    let raw_genre = q
        .genres
        .first()
        .map(String::as_str)
        .or(match &q.parent {
            domain::ProtocolParent::Genre(value) => Some(value.as_str()),
            _ => None,
        });
    let plain_genre = raw_genre.is_some_and(|v| {
        !v.to_ascii_lowercase().starts_with("genre_")
            && uuid::Uuid::parse_str(v).is_err()
            && q.genre_ids.is_empty()
    });
    let classification = match (
        genre_name,
        request.studios.as_slice(),
        request.genres.is_empty(),
    ) {
        (Some(genre), [], _) if plain_genre && !genre[5..].is_empty() => Some(("genre", genre)),
        (None, [studio], true) => Some(("studio", studio.as_str())),
        (None, [], true) => kind.map(|kind| ("kind", kind)).or(Some(("all", "*"))),
        _ => None,
    };
    let count_strategy = if scope.library_ids.len() == 1
        && scope.granularity == Granularity::Work
        && !residual
        && request.people.is_empty()
        && q.is_folder.is_none()
        && !matches!(q.parent, domain::ProtocolParent::Folder { .. })
    {
        match classification {
            Some((dimension, value)) => CountStrategy::Marginal {
                dimension: dimension.into(),
                value: value.into(),
            },
            _ => CountStrategy::ExactMembers,
        }
    } else {
        CountStrategy::ExactMembers
    };
    let effective = effective_sorts(q, scope.granularity);
    let indexed = scope.library_ids.len() == 1
        && request.people.is_empty()
        && !q
            .search_term
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty())
        && match (scope.granularity, effective.as_slice()) {
            (Granularity::Work, []) | (Granularity::Unit, []) => true,
            (Granularity::Work, [sort]) if sort.key == ProtocolSortKey::Name => true,
            (Granularity::Work, [date, name]) if explicit_work_default(date, name) => true,
            _ => false,
        };
    ProtocolReadPlan {
        count_strategy,
        page_strategy: if indexed {
            PageStrategy::IndexedSingleLibrary
        } else {
            PageStrategy::GeneralExact
        },
    }
}

fn effective_sorts(request: &ProtocolItemQuery, granularity: Granularity) -> Vec<&ProtocolSort> {
    request
        .sort
        .iter()
        .filter(|sort| match sort.key {
            ProtocolSortKey::Noop => false,
            ProtocolSortKey::SeasonNumber | ProtocolSortKey::EpisodeNumber => {
                granularity.wants_unit()
            }
            _ => true,
        })
        .collect()
}

fn explicit_work_default(date: &ProtocolSort, name: &ProtocolSort) -> bool {
    date.key == ProtocolSortKey::DateCreated
        && date.descending
        && name.key == ProtocolSortKey::Name
        && name.descending
}

fn work_index_for_sort(request: &ProtocolItemQuery) -> &'static str {
    let effective = effective_sorts(request, Granularity::Work);
    match effective.as_slice() {
        [date, name] if explicit_work_default(date, name) => "items_protocol_default_explicit",
        [name] if name.key == ProtocolSortKey::Name && name.descending => {
            "items_protocol_name_desc"
        }
        [name] if name.key == ProtocolSortKey::Name => "items_protocol_name_asc",
        _ => "items_protocol_default",
    }
}

pub(super) fn read(
    tx: &Connection,
    request: &ProtocolItemQuery,
    scope: &Scope,
    plan: &ProtocolReadPlan,
    scoped: &str,
    rows: &str,
    work_predicate: Option<&str>,
    unit_predicate: Option<&str>,
    values: &[Value],
) -> Result<Vec<domain::ProtocolItemRef>, StoreError> {
    if request.limit == 0 {
        return Ok(Vec::new());
    }
    let order = order::order_by_sql(request, scope.granularity.wants_unit(), "r");
    let representative = if scope.library_ids.len() == 1 {
        "SELECT * FROM rows".to_string()
    } else {
        "SELECT * FROM (SELECT rows.*, ROW_NUMBER() OVER (PARTITION BY media_id,season,episode ORDER BY (kind='movie') DESC,library_id ASC) AS rn FROM rows) WHERE rn=1".to_string()
    };
    let limit_ph = format!("?{}", values.len() + 1);
    let offset_ph = format!("?{}", values.len() + 2);
    let indexed_work = plan.page_strategy == PageStrategy::IndexedSingleLibrary
        && scope.granularity == Granularity::Work;
    let page_sql = if indexed_work {
        let index = work_index_for_sort(request);
        let predicate = work_predicate.unwrap_or("1=0").replace("s.", "i.");
        let direct_order =
            order::order_by_sql(request, false, "i").replace(", i.season ASC, i.episode ASC", "");
        format!(
            "SELECT i.library_id,i.media_id,i.kind,i.title,i.protocol_title_key,i.added_at_sort,-1 AS season,-1 AS episode FROM library_items i INDEXED BY {index} WHERE {} AND COALESCE({},'') IS NOT NULL AND {predicate} ORDER BY {direct_order} LIMIT {limit_ph} OFFSET {offset_ph}",
            scope.where_sql, scope.instance_locale
        )
    } else if plan.page_strategy == PageStrategy::IndexedSingleLibrary
        && scope.granularity == Granularity::Unit
    {
        let predicate = unit_predicate.unwrap_or("1=0").replace("s.", "i.");
        let direct_order = order
            .replace("r.season", "u.season")
            .replace("r.episode", "u.episode")
            .replace("r.library_id", "i.library_id")
            .replace("r.media_id", "i.media_id");
        format!(
            "SELECT i.library_id,i.media_id,i.kind,i.title,i.protocol_title_key,i.added_at_sort,u.season,u.episode FROM library_units u INDEXED BY units_protocol_order JOIN library_items i ON i.library_id=u.library_id AND i.media_id=u.media_id WHERE u.library_id=?1 AND {} AND u.season>=0 AND u.episode>=0 AND COALESCE({},'') IS NOT NULL AND {predicate} ORDER BY {direct_order} LIMIT {limit_ph} OFFSET {offset_ph}",
            scope.where_sql, scope.instance_locale
        )
    } else {
        format!(
            "SELECT r.* FROM ({representative}) r ORDER BY {order} LIMIT {limit_ph} OFFSET {offset_ph}"
        )
    };
    let sql = format!(
        "WITH {scoped}, {rows}, page AS MATERIALIZED ({page_sql}), sourced AS MATERIALIZED (SELECT rows.*, {} AS source_file_id FROM page rows) SELECT r.library_id,r.media_id,r.season,r.episode,r.source_file_id,f.file_revision FROM sourced r LEFT JOIN library_files f ON f.file_id=r.source_file_id ORDER BY {order}",
        source::source_expr(scope)
    );
    let mut bindings = values.to_vec();
    bindings.push(Value::Integer(
        i64::try_from(request.limit).unwrap_or(i64::MAX),
    ));
    bindings.push(Value::Integer(
        i64::try_from(request.offset).unwrap_or(i64::MAX),
    ));
    let mut stmt = tx.prepare(&sql)?;
    let result = stmt
        .query_map(params_from_iter(bindings), sql::row_to_ref)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(result)
}
