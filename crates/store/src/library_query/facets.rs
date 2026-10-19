//! Facet dimensions in SQL: ten candidate groups, each counted with every condition
//! **except its own** applied.
//!
//! A value's count is the number of distinct media the wall would show after ticking it, so
//! it is computed with the filter minus that dimension — the same "exclude own dimension"
//! semantics the UI contract has always had, but now on the same predicate builder the item
//! page uses. Candidates come from the whole library (counts may be 0, the UI greys them).

use std::collections::BTreeMap;

use rusqlite::params_from_iter;

use super::super::{Store, StoreError};
use super::items::{filter_clauses, Bindings, ItemQuerySql, WATCH_TIER_SQL};
use super::types::{
    BrowseFilter, FacetCount, FacetRequest, FacetTier, LibraryFacets, LibraryQuery,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dimension {
    Genre,
    Country,
    Decade,
    Watch,
    Rating,
    Runtime,
    Language,
    Resolution,
    Hdr,
    Stock,
}

/// The four dimensions the collapsed filter panel shows.
const PRIMARY: [Dimension; 4] = [
    Dimension::Genre,
    Dimension::Country,
    Dimension::Decade,
    Dimension::Watch,
];

const ALL: [Dimension; 10] = [
    Dimension::Genre,
    Dimension::Country,
    Dimension::Decade,
    Dimension::Watch,
    Dimension::Rating,
    Dimension::Runtime,
    Dimension::Language,
    Dimension::Resolution,
    Dimension::Hdr,
    Dimension::Stock,
];

impl Dimension {
    /// Response key of this dimension: the frontend reads exactly these names.
    fn key(self) -> &'static str {
        match self {
            Self::Genre => "genres",
            Self::Country => "countries",
            Self::Decade => "decades",
            Self::Watch => "watch",
            Self::Rating => "ratings",
            Self::Runtime => "runtimes",
            Self::Language => "languages",
            Self::Resolution => "resolutions",
            Self::Hdr => "hdr",
            Self::Stock => "stock",
        }
    }

    fn term_kind(self) -> Option<&'static str> {
        match self {
            Self::Genre => Some("genre"),
            Self::Country => Some("country"),
            Self::Resolution => Some("resolution"),
            _ => None,
        }
    }

    /// Value expression of the dimensions whose candidate set is open (`languages`): every
    /// value the data carries, counted over the base filter.
    fn value_expr(self) -> Option<&'static str> {
        match self {
            Self::Language => Some("NULLIF(LOWER(i.language), '')"),
            _ => None,
        }
    }

    /// Dimensions with a documented closed candidate set: `(value, condition, always)`.
    /// `always` is the contract rule that a candidate the UI renders as a fixed bucket is
    /// returned even when nothing matches it, so the frontend can grey it out instead of
    /// omitting it (`docs/api-contracts/self.md`: 候选全集…含 `count: 0`). `favorite` is the
    /// one watch value that is not a fixed bucket, so it appears only once someone favourites
    /// something.
    fn conditional_values(self) -> Vec<(String, String, bool)> {
        match self {
            Self::Decade => [
                ("2020s", "i.year >= 2020 AND i.year < 2030"),
                ("2010s", "i.year >= 2010 AND i.year < 2020"),
                ("2000s", "i.year >= 2000 AND i.year < 2010"),
                ("1990s", "i.year >= 1990 AND i.year < 2000"),
                ("earlier", "i.year IS NOT NULL AND i.year < 1990"),
            ]
            .into_iter()
            .map(|(value, condition)| (value.to_string(), condition.to_string(), true))
            .collect(),
            Self::Runtime => [
                ("lte60", "i.runtime_sort > 0 AND i.runtime_sort <= 60"),
                ("60to90", "i.runtime_sort > 60 AND i.runtime_sort <= 90"),
                ("90to120", "i.runtime_sort > 90 AND i.runtime_sort <= 120"),
                ("gt120", "i.runtime_sort > 120"),
            ]
            .into_iter()
            .map(|(value, condition)| (value.to_string(), condition.to_string(), true))
            .collect(),
            Self::Rating => ["6", "7", "8", "9"]
                .into_iter()
                .map(|value| {
                    (
                        value.to_string(),
                        format!("i.rating_sort >= {value}"),
                        true,
                    )
                })
                .collect(),
            Self::Hdr => ["true", "false"]
                .into_iter()
                .map(|value| {
                    (
                        value.to_string(),
                        format!("i.hdr = {}", i32::from(value == "true")),
                        true,
                    )
                })
                .collect(),
            Self::Stock => vec![
                ("missing".into(), "i.missing = 1".into(), true),
                ("unscraped".into(), "i.scraped = 0".into(), true),
            ],
            Self::Watch => {
                let mut values = ["unwatched", "watching", "played"]
                    .into_iter()
                    .map(|value| {
                        (
                            value.to_string(),
                            format!("({WATCH_TIER_SQL}) = '{value}'"),
                            true,
                        )
                    })
                    .collect::<Vec<_>>();
                values.push(("favorite".into(), "COALESCE(u.favorite, 0) = 1".into(), false));
                values
            }
            _ => Vec::new(),
        }
    }

    fn label(self, value: &str) -> String {
        match self {
            Self::Genre => lookup(GENRE_LABELS, value),
            Self::Country => lookup(COUNTRY_LABELS, value),
            Self::Watch => match value {
                "unwatched" => "未观看".into(),
                "watching" => "在看".into(),
                "played" => "已看完".into(),
                "favorite" => "收藏".into(),
                _ => value.to_string(),
            },
            Self::Rating => format!("{value}分以上"),
            Self::Runtime => match value {
                "lte60" => "60分钟以内".into(),
                "60to90" => "60-90分钟".into(),
                "90to120" => "90-120分钟".into(),
                "gt120" => "120分钟以上".into(),
                _ => value.to_string(),
            },
            Self::Hdr => match value {
                "true" => "HDR".into(),
                _ => "SDR".into(),
            },
            Self::Stock => match value {
                "missing" => "文件失联".into(),
                "unscraped" => "未刮削".into(),
                _ => value.to_string(),
            },
            Self::Decade | Self::Language | Self::Resolution => value.to_string(),
        }
    }

    /// Fixed-bucket dimensions keep their semantic order; data-driven ones rank by count.
    fn rank(self, value: &str) -> usize {
        let order: &[&str] = match self {
            Self::Decade => &["earlier", "1990s", "2000s", "2010s", "2020s"],
            Self::Watch => &["unwatched", "watching", "played", "favorite"],
            Self::Rating => &["6", "7", "8", "9"],
            Self::Runtime => &["lte60", "60to90", "90to120", "gt120"],
            Self::Hdr => &["true", "false"],
            Self::Stock => &["missing", "unscraped"],
            _ => return 0,
        };
        order
            .iter()
            .position(|candidate| *candidate == value)
            .unwrap_or(usize::MAX)
    }
}

/// The filter with one dimension's own condition removed: the "exclude own dimension"
/// counting rule. Clearing the field is the only honest way to express it, because that is
/// exactly how the page query would read a request that had not ticked the value.
fn without(filter: &BrowseFilter, dimension: Dimension) -> BrowseFilter {
    let mut scoped = filter.clone();
    match dimension {
        Dimension::Genre => scoped.genres.clear(),
        Dimension::Country => scoped.countries.clear(),
        Dimension::Decade => scoped.decades.clear(),
        Dimension::Runtime => scoped.runtimes.clear(),
        Dimension::Language => scoped.languages.clear(),
        Dimension::Resolution => scoped.resolutions.clear(),
        Dimension::Stock => scoped.stock.clear(),
        Dimension::Hdr => scoped.hdr = None,
        Dimension::Rating => scoped.min_rating = None,
        Dimension::Watch => scoped.watch = None,
    }
    scoped
}

impl Store {
    /// Ten facet dimensions plus the total of the fully applied filter.
    pub fn library_facets(&self, request: &FacetRequest) -> Result<LibraryFacets, StoreError> {
        request.filter.validate()?;
        let total_query = LibraryQuery {
            library_id: request.library_id,
            user_id: request.user_id,
            filter: request.filter.clone(),
            order: super::types::BrowseOrder::Title,
            descending: false,
            page: super::types::BrowsePage::Offset {
                start: 0,
                limit: 0,
            },
            include_total: false,
        };
        let total = self.library_item_count(&total_query)?;
        let dimensions: &[Dimension] = match request.tier {
            FacetTier::Primary => &PRIMARY,
            FacetTier::All => &ALL,
        };
        let mut groups = BTreeMap::new();
        for dimension in dimensions {
            let scoped = without(&request.filter, *dimension);
            let base = ItemQuerySql::build(request.library_id, &scoped, request.user_id)?;
            let counts = match dimension.term_kind() {
                Some(kind) => term_counts(self, &base, kind)?,
                None => match dimension.value_expr() {
                    Some(expr) => value_counts(self, &base, expr)?,
                    None => conditional_counts(self, &base, &dimension.conditional_values())?,
                },
            };
            groups.insert(dimension.key().to_string(), sort_counts(*dimension, counts));
        }
        Ok(LibraryFacets { total, groups })
    }
}

fn sort_counts(dimension: Dimension, mut counts: Vec<(String, u64)>) -> Vec<FacetCount> {
    counts.sort_by(|left, right| {
        dimension
            .rank(&left.0)
            .cmp(&dimension.rank(&right.0))
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    counts
        .into_iter()
        .map(|(value, count)| FacetCount {
            label: dimension.label(&value),
            value,
            count,
        })
        .collect()
}

/// Multi-valued dimensions: every value a term carries, counted over the base filter.
fn term_counts(
    store: &Store,
    base: &ItemQuerySql,
    kind: &str,
) -> Result<Vec<(String, u64)>, StoreError> {
    let mut bindings = Bindings::from_values(base.bindings.clone());
    let kind_placeholder = bindings.push(kind.to_string().into());
    let sql = format!(
        "SELECT t.term_value, SUM(CASE WHEN {where_sql} THEN 1 ELSE 0 END) \
         FROM library_item_terms t, {from} \
         WHERE i.library_id = ?1 AND t.library_id = i.library_id AND t.media_id = i.media_id \
           AND t.term_kind = {kind_placeholder} \
         GROUP BY t.term_value ORDER BY t.term_value",
        where_sql = base.where_sql,
        from = base.from,
    );
    read_counts(store, &sql, bindings)
}

/// Scalar dimensions: one row per value present anywhere in the library, counted over the
/// base filter (a value nobody matches under the other dimensions still returns count 0).
fn value_counts(store: &Store, base: &ItemQuerySql, expr: &str) -> Result<Vec<(String, u64)>, StoreError> {
    let bindings = Bindings::from_values(base.bindings.clone());
    let sql = format!(
        "SELECT {expr}, SUM(CASE WHEN {where_sql} THEN 1 ELSE 0 END) \
         FROM {from} WHERE i.library_id = ?1 AND {expr} IS NOT NULL \
         GROUP BY 1 ORDER BY 1",
        where_sql = base.where_sql,
        from = base.from,
    );
    read_counts(store, &sql, bindings)
}

/// Fixed-bucket dimensions: one count per documented value. `always` values are returned even
/// when no item of the library satisfies their condition (count 0, the UI greys them); the
/// others exist only while the condition matches at least one item in the library.
fn conditional_counts(
    store: &Store,
    base: &ItemQuerySql,
    values: &[(String, String, bool)],
) -> Result<Vec<(String, u64)>, StoreError> {
    let mut counts = Vec::new();
    for (value, condition, always) in values {
        let bindings = Bindings::from_values(base.bindings.clone());
        let sql = format!(
            "SELECT SUM(CASE WHEN {where_sql} AND {condition} THEN 1 ELSE 0 END) \
             FROM {from} WHERE i.library_id = ?1 AND {condition}",
            where_sql = base.where_sql,
            from = base.from,
        );
        let mut statement = store.library.prepare(&sql)?;
        let total: Option<i64> =
            statement.query_row(params_from_iter(bindings.values()), |row| row.get(0))?;
        match (total, always) {
            (Some(total), _) => counts.push((value.clone(), total.max(0) as u64)),
            (None, true) => counts.push((value.clone(), 0)),
            (None, false) => {}
        }
    }
    Ok(counts)
}

fn read_counts(
    store: &Store,
    sql: &str,
    bindings: Bindings,
) -> Result<Vec<(String, u64)>, StoreError> {
    let mut statement = store.library.prepare(sql)?;
    let rows = statement.query_map(params_from_iter(bindings.values()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
    })?;
    let mut counts = Vec::new();
    for row in rows {
        let (value, count) = row?;
        if value.trim().is_empty() {
            continue;
        }
        counts.push((value, count.unwrap_or(0).max(0) as u64));
    }
    Ok(counts)
}

fn lookup(table: &[(&str, &str)], value: &str) -> String {
    table
        .iter()
        .find(|(key, _)| *key == value)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| value.to_string())
}

const GENRE_LABELS: &[(&str, &str)] = &[
    ("28", "动作"),
    ("12", "冒险"),
    ("16", "动画"),
    ("35", "喜剧"),
    ("80", "犯罪"),
    ("99", "纪录"),
    ("18", "剧情"),
    ("10751", "家庭"),
    ("14", "奇幻"),
    ("36", "历史"),
    ("27", "恐怖"),
    ("10402", "音乐"),
    ("9648", "悬疑"),
    ("10749", "爱情"),
    ("878", "科幻"),
    ("10770", "电视电影"),
    ("53", "惊悚"),
    ("10752", "战争"),
    ("37", "西部"),
    ("10759", "动作冒险"),
    ("10762", "儿童"),
    ("10763", "新闻"),
    ("10764", "真人秀"),
    ("10765", "科幻奇幻"),
    ("10766", "肥皂剧"),
    ("10767", "脱口秀"),
    ("10768", "战争政治"),
];

const COUNTRY_LABELS: &[(&str, &str)] = &[
    ("US", "美国"),
    ("GB", "英国"),
    ("CN", "中国大陆"),
    ("JP", "日本"),
    ("KR", "韩国"),
    ("FR", "法国"),
    ("DE", "德国"),
    ("HK", "中国香港"),
    ("TW", "中国台湾"),
    ("CA", "加拿大"),
    ("AU", "澳大利亚"),
    ("IN", "印度"),
    ("IT", "意大利"),
    ("ES", "西班牙"),
    ("RU", "俄罗斯"),
    ("TH", "泰国"),
    ("SG", "新加坡"),
    ("NZ", "新西兰"),
    ("SE", "瑞典"),
    ("DK", "丹麦"),
];
