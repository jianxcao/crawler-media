//! Library browse over the `library_items` summary: filter predicates, ordering and
//! keyset pagination, plus the page-scoped batch reads a poster wall needs.
//!
//! One predicate builder serves the page query, the `COUNT(*)`, the first-letter index and
//! the facet dimensions, so a filter can never mean two different things in one response.
//! Everything read here is a stored fact: no request-time file observation.

use std::collections::HashMap;
use std::str::FromStr;

use domain::{LibraryId, MediaId, MediaKind, UserId};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Row, params_from_iter};
use serde::{Deserialize, Serialize};

use super::super::{Store, StoreError};
use super::types::{
    BrowseFilter, BrowseOrder, BrowsePage, ItemPage, LibraryItem, LibraryItemFacts, LibraryQuery,
    LibraryStill,
};

/// Explicit page size ceiling for cursor pages (a cursor must not ask for the world).
pub(crate) const MAX_ITEM_PAGE: usize = 500;

/// Watch tiers as an ascending integer, mirroring `library::WatchTier` order.
pub(crate) const WATCH_TIER_SQL: &str = "CASE WHEN COALESCE(u.seen, 0) = 0 THEN 0 \
     WHEN COALESCE(u.played, 0) = 1 THEN 2 ELSE 1 END";

/// `library_items` columns every card query selects, in [`item_from_row`] order.
pub(crate) const ITEM_COLUMNS: &str = "i.library_id, i.media_id, i.kind, i.title, i.title_sort, \
     i.year, i.file_count, i.available_file_count, i.missing_file_count, i.unknown_file_count, \
     i.distinct_episode_count, i.distinct_season_count, i.size_bytes, i.preferred_file_id, \
     i.has_still, i.rating_sort, i.language, i.identity_state, i.missing, i.scraped, \
     i.poster_ref, i.backdrop_ref, i.resolution_rank";

/// Number of columns in [`ITEM_COLUMNS`]; ordering keys are selected right after them.
pub(crate) const ITEM_KEY_OFFSET: usize = 23;

fn id_from<T: FromStr>(row: &Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    row.get::<_, String>(index)?.parse::<T>().map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
    })
}

/// Text column that must hold a known media kind: a value outside the enum is a broken row,
/// not a movie.
fn kind_from(row: &Row<'_>, index: usize) -> rusqlite::Result<MediaKind> {
    row.get::<_, String>(index)?.parse().map_err(|kind: String| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid media kind: {kind}"),
            )),
        )
    })
}
pub(crate) fn item_from_row(row: &Row<'_>) -> rusqlite::Result<LibraryItem> {
    let lib_id: LibraryId = id_from(row, 0)?;
    let media_id: MediaId = id_from(row, 1)?;
    let kind: MediaKind = kind_from(row, 2)?;
    let year: Option<i64> = row.get(5)?;
    let size: Option<i64> = row.get(12)?;
    let preferred: Option<String> = row.get(13)?;
    let rating: f64 = row.get(15)?;
    let poster: Option<String> = row.get(20)?;
    let backdrop: Option<String> = row.get(21)?;
    Ok(LibraryItem {
        library_id: lib_id,
        media_id,
        kind,
        title: row.get(3)?,
        title_sort: row.get(4)?,
        year: year.map(|value| value as u16),
        file_count: row.get::<_, i64>(6)?.max(0) as usize,
        available_file_count: row.get::<_, i64>(7)?.max(0) as usize,
        missing_file_count: row.get::<_, i64>(8)?.max(0) as usize,
        unknown_file_count: row.get::<_, i64>(9)?.max(0) as usize,
        distinct_episode_count: row.get::<_, i64>(10)?.max(0) as usize,
        distinct_season_count: row.get::<_, i64>(11)?.max(0) as usize,
        size_bytes: size.map(|value| value as u64),
        preferred_file_id: preferred.and_then(|value| value.parse().ok()),
        has_still: row.get::<_, i64>(14)? != 0,
        rating: (rating >= 0.0).then_some(rating),
        language: row.get(16)?,
        identity_state: row.get(17)?,
        missing: row.get::<_, i64>(18)? != 0,
        scraped: row.get::<_, i64>(19)? != 0,
        poster_ref: poster.and_then(|value| value.parse().ok()),
        backdrop_ref: backdrop.and_then(|value| value.parse().ok()),
        resolution_rank: row.get(22)?,
        // Per-page batch reads fill these (`library_item_facts`).
        studio: None,
        studio_logo: None,
    })
}

/// Bind parameters in order and hand back the `?N` placeholder to write into the SQL.
pub(crate) struct Bindings {
    values: Vec<SqlValue>,
}

impl Bindings {
    pub(crate) fn new() -> Self {
        Self { values: Vec::new() }
    }

    /// Continue binding from an existing batch (facet dimensions append their own values to
    /// the shared filter parameters, so placeholder numbers stay contiguous).
    pub(crate) fn from_values(values: Vec<SqlValue>) -> Self {
        Self { values }
    }

    pub(crate) fn push(&mut self, value: SqlValue) -> String {
        self.values.push(value);
        format!("?{}", self.values.len())
    }

    fn placeholders(&mut self, count: usize, value: impl Fn(usize) -> SqlValue) -> String {
        (0..count)
            .map(|index| self.push(value(index)))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = SqlValue> + '_ {
        self.values.iter().cloned()
    }
}

/// The FROM/WHERE of one library browse query, shared by the page query and its count.
pub(crate) struct ItemQuerySql {
    pub(crate) from: String,
    pub(crate) where_sql: String,
    pub(crate) bindings: Vec<SqlValue>,
}

impl ItemQuerySql {
    pub(crate) fn build(
        library_id: LibraryId,
        filter: &BrowseFilter,
        user_id: Option<UserId>,
    ) -> Result<Self, StoreError> {
        filter.validate()?;
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        // The user placeholder is bound even when nobody is signed in: the sentinel matches
        // no row, so every watch predicate falls through to "no user row" (unwatched and
        // not favourite) exactly like the previous in-memory semantics.
        let user = bindings.push(user_id.map(|id| id.to_string()).unwrap_or_default().into());
        let from = format!(
            "library_items i LEFT JOIN user_library_items u \
             ON u.library_id = i.library_id AND u.media_id = i.media_id AND u.user_id = {user}"
        );
        let mut clauses = vec![format!("i.library_id = {library}")];
        clauses.extend(filter_clauses(filter, &mut bindings));
        Ok(Self {
            from,
            where_sql: clauses.join(" AND "),
            bindings: bindings.values,
        })
    }

    /// `SELECT <columns> FROM <from> WHERE <where>` with no ordering or paging applied.
    fn select(&self, columns: &str) -> String {
        format!("SELECT {columns} FROM {} WHERE {}", self.from, self.where_sql)
    }
}

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
        // `validate` already rejected every value that is not `NNNNs`.
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
    /// Reject filter values the dimensions cannot express, so a bad request is an error and
    /// never a silently ignored condition.
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
                return Err(StoreError::Invalid(format!("invalid decade filter: {decade}")));
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
                return Err(StoreError::Invalid(format!("invalid stock filter: {value}")));
            }
        }
        if let Some(watch) = &self.watch {
            if !WATCH.contains(&watch.as_str()) {
                return Err(StoreError::Invalid(format!("invalid watch filter: {watch}")));
            }
        }
        if let Some(identity) = &self.identity {
            if !matches!(identity.as_str(), "provisional" | "confirmed") {
                return Err(StoreError::Invalid(format!(
                    "invalid identity filter: {identity}"
                )));
            }
        }
        if let Some(rating) = self.min_rating {
            if !rating.is_finite() || !(0.0..=10.0).contains(&rating) {
                return Err(StoreError::Invalid(
                    "rating_gte must be between 0 and 10".into(),
                ));
            }
        }
        Ok(())
    }
}

/// One ordering term: SQL expression, its own direction, and the cursor key type it yields.
#[derive(Clone, Copy)]
pub(crate) struct OrderTerm {
    pub(crate) expr: &'static str,
    pub(crate) descending: bool,
    kind: KeyKind,
}

#[derive(Clone, Copy)]
enum KeyKind {
    Int,
    Real,
    Text,
}

/// Lexical order of one browse query: the optional watch tier (always ascending, "unwatched
/// first"), then the requested sort key, then the media id as the stable tie-break — the
/// tie-break follows the sort key's direction, so ordering and the cursor seek agree.
pub(crate) fn order_spec(query: &LibraryQuery) -> Vec<OrderTerm> {
    let mut spec = Vec::new();
    if query.filter.unwatched_first {
        spec.push(OrderTerm {
            expr: WATCH_TIER_SQL,
            descending: false,
            kind: KeyKind::Int,
        });
    }
    let (expr, kind) = match query.order {
        BrowseOrder::Title => ("i.title_sort", KeyKind::Text),
        BrowseOrder::AddedAt => ("i.added_at_sort", KeyKind::Int),
        BrowseOrder::ReleaseDate => ("i.release_date_sort", KeyKind::Text),
        BrowseOrder::Rating => ("i.rating_sort", KeyKind::Real),
        BrowseOrder::Runtime => ("i.runtime_sort", KeyKind::Int),
        BrowseOrder::Size => ("i.size_sort", KeyKind::Int),
        BrowseOrder::Resolution => ("i.resolution_rank", KeyKind::Int),
        BrowseOrder::Random => ("i.stable_shuffle_key", KeyKind::Text),
        BrowseOrder::LastPlayed => ("COALESCE(u.last_played_at, 0)", KeyKind::Int),
    };
    spec.push(OrderTerm {
        expr,
        descending: query.descending,
        kind,
    });
    spec.push(OrderTerm {
        expr: "i.media_id",
        descending: query.descending,
        kind: KeyKind::Text,
    });
    spec
}

fn order_by_sql(spec: &[OrderTerm]) -> String {
    spec.iter()
        .map(|term| {
            format!(
                "{} {}",
                term.expr,
                if term.descending { "DESC" } else { "ASC" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
enum CursorKey {
    Int(i64),
    Real(f64),
    Text(String),
}

impl CursorKey {
    fn to_sql(&self) -> SqlValue {
        match self {
            Self::Int(value) => (*value).into(),
            Self::Real(value) => (*value).into(),
            Self::Text(value) => value.clone().into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PageCursor {
    /// One key per ordering term except the media-id tie-break.
    keys: Vec<CursorKey>,
    media_id: String,
}

fn malformed_cursor() -> StoreError {
    StoreError::Invalid("malformed cursor".into())
}

fn read_key(row: &Row<'_>, index: usize, kind: KeyKind) -> rusqlite::Result<CursorKey> {
    Ok(match kind {
        KeyKind::Int => CursorKey::Int(row.get(index)?),
        KeyKind::Real => CursorKey::Real(row.get(index)?),
        KeyKind::Text => CursorKey::Text(row.get(index)?),
    })
}

/// Keyset seek over the cursor's key values: `(k1 after v1) OR (k1 = v1 AND k2 after v2) …`,
/// each term compared in its own direction.
fn seek_clause(
    spec: &[OrderTerm],
    cursor: &PageCursor,
    bindings: &mut Bindings,
) -> Result<String, StoreError> {
    if cursor.keys.len() + 1 != spec.len() || MediaId::from_str(&cursor.media_id).is_err() {
        return Err(malformed_cursor());
    }
    let values: Vec<CursorKey> = cursor
        .keys
        .iter()
        .cloned()
        .chain(std::iter::once(CursorKey::Text(cursor.media_id.clone())))
        .collect();
    let mut groups = Vec::with_capacity(spec.len());
    for index in 0..spec.len() {
        let mut parts = Vec::with_capacity(index + 1);
        for earlier in 0..index {
            let value = bindings.push(values[earlier].to_sql());
            parts.push(format!("{} = {value}", spec[earlier].expr));
        }
        let value = bindings.push(values[index].to_sql());
        let operator = if spec[index].descending { "<" } else { ">" };
        parts.push(format!("{} {operator} {value}", spec[index].expr));
        groups.push(format!("({})", parts.join(" AND ")));
    }
    Ok(format!("({})", groups.join(" OR ")))
}

/// One page of rows plus the ordering keys of its last row.
struct PageRows {
    items: Vec<LibraryItem>,
    last_keys: Option<Vec<CursorKey>>,
    /// The page was cut short by the limit: more rows exist in this direction.
    truncated: bool,
}

impl Store {
    /// One page of library items: filters and ordering applied in SQL, `total` from the same
    /// predicate, and a keyset cursor when the page was truncated.
    pub fn library_item_page(&self, query: &LibraryQuery) -> Result<ItemPage, StoreError> {
        let base = ItemQuerySql::build(query.library_id, &query.filter, query.user_id)?;
        let total = if query.include_total {
            Some(count_items(self, &base)?)
        } else {
            None
        };
        let spec = order_spec(query);
        let keys = spec[..spec.len() - 1]
            .iter()
            .map(|term| term.expr)
            .collect::<Vec<_>>()
            .join(", ");
        let columns = format!("{ITEM_COLUMNS}, {keys}");
        let mut bindings = Bindings {
            values: base.bindings.clone(),
        };
        let (sql, limit) = match &query.page {
            BrowsePage::Offset { start, limit } => {
                let limit = i64::try_from(*limit).unwrap_or(i64::MAX);
                let offset = i64::try_from(*start).unwrap_or(i64::MAX);
                let limit_ph = bindings.push(limit.into());
                let offset_ph = bindings.push(offset.into());
                (
                    format!(
                        "{} ORDER BY {} LIMIT {limit_ph} OFFSET {offset_ph}",
                        base.select(&columns),
                        order_by_sql(&spec)
                    ),
                    usize::MAX,
                )
            }
            BrowsePage::Cursor { after, limit } => {
                let limit = (*limit).clamp(1, MAX_ITEM_PAGE);
                let seek = match after.as_deref() {
                    Some(raw) => {
                        let cursor: PageCursor =
                            serde_json::from_str(raw).map_err(|_| malformed_cursor())?;
                        format!(" AND {}", seek_clause(&spec, &cursor, &mut bindings)?)
                    }
                    None => String::new(),
                };
                let rows = i64::try_from(limit + 1).unwrap_or(i64::MAX);
                let rows_ph = bindings.push(rows.into());
                (
                    format!(
                        "{} {seek} ORDER BY {} LIMIT {rows_ph}",
                        base.select(&columns),
                        order_by_sql(&spec)
                    ),
                    limit,
                )
            }
        };
        let kinds: Vec<KeyKind> = spec[..spec.len() - 1].iter().map(|term| term.kind).collect();
        let page = read_page(
            self,
            &sql,
            bindings.values,
            &kinds,
            limit,
            matches!(query.page, BrowsePage::Cursor { .. }),
        )?;
        let next_cursor = match (page.truncated, page.last_keys) {
            (true, Some(keys)) => {
                let media_id = page
                    .items
                    .last()
                    .map(|item| item.media_id.to_string())
                    .ok_or_else(malformed_cursor)?;
                Some(
                    serde_json::to_string(&PageCursor { keys, media_id })
                        .map_err(|error| StoreError::Invalid(error.to_string()))?,
                )
            }
            _ => None,
        };
        Ok(ItemPage {
            items: page.items,
            total,
            next_cursor,
        })
    }

    /// Titles of every item matching the query, in browse order: the first-letter index of a
    /// poster wall is a bucket of this list, so it needs no second filter implementation.
    pub fn library_item_titles(&self, query: &LibraryQuery) -> Result<Vec<String>, StoreError> {
        let base = ItemQuerySql::build(query.library_id, &query.filter, query.user_id)?;
        let sql = format!(
            "{} ORDER BY {}",
            base.select("i.title"),
            order_by_sql(&order_spec(query))
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(base.bindings.iter().cloned()), |row| {
            row.get::<_, String>(0)
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Count of items matching a query, from the same predicate the page query uses.
    pub fn library_item_count(&self, query: &LibraryQuery) -> Result<u64, StoreError> {
        let base = ItemQuerySql::build(query.library_id, &query.filter, query.user_id)?;
        count_items(self, &base)
    }

    /// Page-scoped card facts: seasons and studio metadata for the page's media ids, one
    /// query each. Favourites come from `user_library_items` when a user is signed in.
    pub fn library_item_facts(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        user_id: Option<UserId>,
    ) -> Result<HashMap<MediaId, LibraryItemFacts>, StoreError> {
        let mut facts: HashMap<MediaId, LibraryItemFacts> = media_ids
            .iter()
            .map(|media_id| (*media_id, LibraryItemFacts::default()))
            .collect();
        if media_ids.is_empty() {
            return Ok(facts);
        }
        for media_chunk in media_ids.chunks(crate::browse_reads::MEDIA_BATCH_CHUNK) {
            self.fill_seasons(library_id, media_chunk, &mut facts)?;
            self.fill_studio(library_id, media_chunk, &mut facts)?;
            if let Some(user_id) = user_id {
                self.fill_favorites(library_id, media_chunk, user_id, &mut facts)?;
            }
        }
        Ok(facts)
    }
    fn fill_seasons(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let list = bindings.placeholders(media_ids.len(), |index| {
            media_ids[index].to_string().into()
        });
        let sql = format!(
            "SELECT media_id, season FROM library_units \
             WHERE library_id = {library} AND season >= 0 AND media_id IN ({list}) \
             GROUP BY media_id, season ORDER BY media_id, season"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (media_id, season) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.seasons.push(season.max(0) as u32);
            }
        }
        Ok(())
    }

    fn fill_studio(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let preferred_locale = super::details::library_display_locale(self, library_id)?;
        let mut bindings = Bindings::new();
        let list = bindings.placeholders(media_ids.len(), |index| {
            media_ids[index].to_string().into()
        });
        let sql = format!(
            "SELECT m.media_id, m.locale, m.details_json, s.normalized_json \
             FROM media_metadata m \
             LEFT JOIN media_metadata_sources s \
               ON s.media_id = m.media_id AND s.entity_scope = 'work' AND s.season = -1 \
              AND s.episode = -1 AND s.locale = m.locale \
             WHERE m.media_id IN ({list}) ORDER BY m.media_id, m.locale"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        // `media_metadata` has one row per locale and is ordered by locale, so the first row
        // of a media is the deterministic fallback; an exact preferred-locale row replaces it.
        let mut picked: HashMap<MediaId, (String, Option<String>, Option<String>)> = HashMap::new();
        for row in rows {
            let (media_id, locale, details, normalized) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            let exact = preferred_locale.as_deref() == Some(locale.as_str());
            let replace = match picked.get(&media_id) {
                None => true,
                Some((kept, _, _)) => exact && preferred_locale.as_deref() != Some(kept.as_str()),
            };
            if replace {
                let (studio, logo) = studio_fields(&details, normalized.as_deref());
                picked.insert(media_id, (locale, studio, logo));
            }
        }
        for (media_id, (_, studio, logo)) in picked {
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.studio = studio;
                facts.studio_logo = logo;
            }
        }
        Ok(())
    }

    fn fill_favorites(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        user_id: UserId,
        facts: &mut HashMap<MediaId, LibraryItemFacts>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let user = bindings.push(user_id.to_string().into());
        let list = bindings.placeholders(media_ids.len(), |index| {
            media_ids[index].to_string().into()
        });
        let sql = format!(
            "SELECT media_id, favorite FROM user_library_items \
             WHERE library_id = {library} AND user_id = {user} AND media_id IN ({list})"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (media_id, favorite) = row?;
            let Ok(media_id) = media_id.parse::<MediaId>() else {
                continue;
            };
            if let Some(facts) = facts.get_mut(&media_id) {
                facts.favorite = favorite != 0;
            }
        }
        Ok(())
    }

    /// Stills of the page's media ids: published files whose `has_still` is set, with the
    /// unit each file covers. One query for the whole page.
    pub fn library_item_stills(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
    ) -> Result<Vec<LibraryStill>, StoreError> {
        let mut stills = Vec::new();
        for media_chunk in media_ids.chunks(crate::browse_reads::MEDIA_BATCH_CHUNK) {
            self.read_stills_batch(library_id, media_chunk, &mut stills)?;
        }
        Ok(stills)
    }

    fn read_stills_batch(
        &self,
        library_id: LibraryId,
        media_ids: &[MediaId],
        stills: &mut Vec<LibraryStill>,
    ) -> Result<(), StoreError> {
        let mut bindings = Bindings::new();
        let library = bindings.push(library_id.to_string().into());
        let list = bindings.placeholders(media_ids.len(), |index| media_ids[index].to_string().into());
        let sql = format!(
            "SELECT f.media_id, f.file_id, u.season, u.episode \
             FROM library_files f \
             JOIN library_file_units u ON u.file_id = f.file_id AND u.library_id = f.library_id \
             WHERE f.library_id = {library} AND f.publication_state = 'published' \
               AND f.has_still = 1 AND f.media_id IN ({list}) \
             ORDER BY f.media_id, u.season, u.episode, f.file_id"
        );
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?))
        })?;
        for row in rows {
            let (media, file, season, episode) = row?;
            let (Ok(media_id), Ok(file_id)) = (media.parse::<MediaId>(), file.parse()) else { continue };
            stills.push(LibraryStill {
                media_id,
                file_id,
                season: (season >= 0).then_some(season as u32),
                episode: (episode >= 0).then_some(episode as u32),
            });
        }
        Ok(())
    }

}

fn count_items(store: &Store, base: &ItemQuerySql) -> Result<u64, StoreError> {
    let sql = format!("SELECT COUNT(*) FROM {} WHERE {}", base.from, base.where_sql);
    let count: i64 = store
        .library
        .query_row(&sql, params_from_iter(base.bindings.iter().cloned()), |row| {
            row.get(0)
        })?;
    Ok(count.max(0) as u64)
}

/// Read at most `limit` rows, reporting whether the limit cut the result short. Reads the
/// ordering keys of every row it keeps, so the caller can build the next cursor from the last
/// row it returned.
fn read_page(
    store: &Store,
    sql: &str,
    bindings: Vec<SqlValue>,
    kinds: &[KeyKind],
    limit: usize,
    cursor: bool,
) -> Result<PageRows, StoreError> {
    let mut statement = store.library.prepare(sql)?;
    let mut rows = statement.query(params_from_iter(bindings.into_iter()))?;
    let mut items = Vec::new();
    let mut last_keys = None;
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if cursor && items.len() == limit {
            truncated = true;
            break;
        }
        let item = item_from_row(row)?;
        let mut keys = Vec::with_capacity(kinds.len());
        for (index, kind) in kinds.iter().enumerate() {
            keys.push(read_key(row, ITEM_KEY_OFFSET + index, *kind)?);
        }
        items.push(item);
        last_keys = Some(keys);
    }
    Ok(PageRows {
        items,
        last_keys,
        truncated,
    })
}

/// Studio name and logo of an item: the adopted row's `details_json` first, then the adopted
/// source payload. `None` when the metadata carries neither.
fn studio_fields(
    details_json: &str,
    normalized_json: Option<&str>,
) -> (Option<String>, Option<String>) {
    let details: serde_json::Value = serde_json::from_str(details_json).unwrap_or_default();
    let studio = details
        .get("studio")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| first_string(details.get("studios")));
    let logo = details
        .get("studio_logo")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if studio.is_some() || logo.is_some() {
        return (studio, logo);
    }
    let Some(normalized) = normalized_json else {
        return (None, None);
    };
    let fields: serde_json::Value = serde_json::from_str(normalized).unwrap_or_default();
    let fields = fields.get("fields").cloned().unwrap_or(fields);
    let studio = first_string(fields.get("studios"));
    let logo = fields
        .get("studio_logo")
        .and_then(|value| value.get("value"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    (studio, logo)
}

fn first_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::Array(values) => values
            .first()
            .and_then(|value| value.as_str())
            .map(str::to_string),
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}
