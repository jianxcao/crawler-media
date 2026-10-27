//! Facet dimensions in SQL: ten candidate groups, each counted with every condition
//! **except its own** applied.
//!
//! A value's count is the number of distinct media the wall would show after ticking it, so
//! it is computed with the filter minus that dimension — the same "exclude own dimension"
//! semantics the UI contract has always had, but now on the same predicate builder the item
//! page uses. Candidates come from the whole library (counts may be 0, the UI greys them).

use super::super::{Store, StoreError};
use super::items::WATCH_TIER_SQL;

use super::types::{BrowseFilter, FacetCount, FacetRequest, LibraryFacets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dimension {
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
pub(super) const PRIMARY: [Dimension; 4] = [
    Dimension::Genre,
    Dimension::Country,
    Dimension::Decade,
    Dimension::Watch,
];

pub(super) const ALL: [Dimension; 10] = [
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
    pub(super) fn key(self) -> &'static str {
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

    pub(super) fn term_kind(self) -> Option<&'static str> {
        match self {
            Self::Genre => Some("genre"),
            Self::Country => Some("country"),
            Self::Resolution => Some("resolution"),
            _ => None,
        }
    }

    /// Dimensions with a documented closed candidate set: `(value, condition, always)`.
    /// `always` is the contract rule that a candidate the UI renders as a fixed bucket is
    /// returned even when nothing matches it, so the frontend can grey it out instead of
    /// omitting it (`docs/api-contracts/self.md`: 候选全集…含 `count: 0`). `favorite` is the
    /// one watch value that is not a fixed bucket, so it appears only once someone favourites
    /// something.
    pub(super) fn conditional_values(self) -> Vec<(String, String, bool)> {
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
                .map(|value| (value.to_string(), format!("i.rating_sort >= {value}"), true))
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
            Self::Watch => vec![
                ("unwatched".into(), format!("({WATCH_TIER_SQL}) = 0"), true),
                ("watching".into(), format!("({WATCH_TIER_SQL}) = 1"), true),
                ("played".into(), format!("({WATCH_TIER_SQL}) = 2"), true),
                (
                    "favorite".into(),
                    "COALESCE(u.favorite, 0) = 1".into(),
                    false,
                ),
            ],
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
pub(super) fn without(filter: &BrowseFilter, dimension: Dimension) -> BrowseFilter {
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
        self.library_facets_inner(request)
    }

    pub fn library_facets_with_stats(
        &self,
        request: &FacetRequest,
    ) -> Result<(LibraryFacets, super::items::QueryWorkStats), StoreError> {
        let observer = super::items::stats::QueryWorkScope::new(&[&self.app, &self.library]);
        let facets = self.library_facets_inner(request)?;
        let rows = facets.groups.values().map(Vec::len).sum::<usize>() as u64;
        Ok((facets, observer.stats(rows)))
    }

    fn library_facets_inner(&self, request: &FacetRequest) -> Result<LibraryFacets, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let result = library_facets_on(&tx, request)?;
        tx.commit()?;
        Ok(result)
    }
}

pub(crate) fn library_facets_on(
    conn: &rusqlite::Connection,
    request: &FacetRequest,
) -> Result<LibraryFacets, StoreError> {
    super::items::page::ensure_library_query_ready_on(conn, request.library_id)?;
    request.filter.validate()?;
    super::members::read_facets(conn, request)
}

pub(super) fn sort_counts(dimension: Dimension, mut counts: Vec<(String, u64)>) -> Vec<FacetCount> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::gate::DEFAULT_GATE_TIMEOUT;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    #[allow(dead_code)]
    mod support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/query_work/support.rs"
        ));
    }

    #[test]
    fn filtered_facets_do_not_read_cold_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let library = support::seed(&store, domain::MediaKind::Movie, 4);
        store
            .library
            .authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
                AuthAction::Read {
                    table_name: "media_metadata",
                    ..
                }
                | AuthAction::Read {
                    table_name: "library_metadata_inputs",
                    ..
                }
                | AuthAction::Read {
                    table_name: "media_metadata_overrides",
                    ..
                }
                | AuthAction::Read {
                    table_name: "media_metadata_effective",
                    ..
                } => Authorization::Deny,
                _ => Authorization::Allow,
            }));
        for (genres, expected) in [
            (vec!["Drama".into(), "Comedy".into()], 4),
            (vec!["Drama".into()], 2),
            (vec!["absent".into()], 0),
        ] {
            let actual = store
                .library_facets(&FacetRequest {
                    library_id: library,
                    user_id: None,
                    tier: super::super::types::FacetTier::All,
                    filter: BrowseFilter {
                        genres,
                        ..Default::default()
                    },
                })
                .unwrap();
            assert_eq!(actual.total, expected);
        }
        store
            .library
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    }

    #[test]
    fn facets_use_one_snapshot_when_second_connection_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let library = support::seed(&store, domain::MediaKind::Movie, 4);
        let writer = Store::open(tmp.path()).unwrap();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            start_rx
                .recv_timeout(DEFAULT_GATE_TIMEOUT)
                .expect("the facet read never started");
            let db = rusqlite::Connection::open(writer.data_dir().join("library.db")).unwrap();
            let media: String = db.query_row("SELECT media_id FROM library_items WHERE library_id=?1 AND title='Item 000000'",[library.to_string()],|r|r.get(0)).unwrap();
            db.execute("UPDATE library_files SET publication_state='pending' WHERE library_id=?1 AND media_id=?2",rusqlite::params![library.to_string(),media]).unwrap();
            writer
                .begin_library_integrity(library, true, "a7-snapshot-writer", 3000)
                .unwrap();
            // Bounded: an operation that never leaves `applying` must fail this
            // test with a name, not spin a core forever.
            let mut settled = false;
            for _ in 0..1000 {
                if writer
                    .advance_library_operation("a7-snapshot-writer", 3001, 100)
                    .unwrap()
                    .state
                    != "applying"
                {
                    settled = true;
                    break;
                }
            }
            assert!(
                settled,
                "integrity operation stayed in `applying` for 1000 steps"
            );
            done_tx.send(()).unwrap();
        });
        let mut triggered = false;
        store.library.authorizer(Some(move |ctx: AuthContext<'_>| {
            if !triggered
                && matches!(
                    ctx.action,
                    AuthAction::Read {
                        table_name: "library_query_counts",
                        ..
                    }
                )
            {
                triggered = true;
                start_tx.send(()).unwrap();
                done_rx
                    .recv_timeout(DEFAULT_GATE_TIMEOUT)
                    .expect("the concurrent integrity operation never completed");
            }
            Authorization::Allow
        }));
        let request = FacetRequest {
            library_id: library,
            user_id: None,
            filter: BrowseFilter::default(),
            tier: super::super::types::FacetTier::All,
        };
        let first = store.library_facets(&request).unwrap();
        worker.join().unwrap();
        store
            .library
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        assert_eq!(
            first.groups["genres"]
                .iter()
                .find(|v| v.value == "Drama")
                .unwrap()
                .count,
            2
        );
        let second = store.library_facets(&request).unwrap();
        assert_eq!(
            second.groups["genres"]
                .iter()
                .find(|v| v.value == "Drama")
                .unwrap()
                .count,
            1
        );
    }
}
