mod query_model_support;
use store::Store;
#[path = "query_work/support.rs"]
mod work_support;

use store::library_query::{BrowseFilter, FacetRequest, FacetTier};

fn count(facets: &store::library_query::LibraryFacets, group: &str, value: &str) -> u64 {
    facets.groups[group]
        .iter()
        .find(|bucket| bucket.value == value)
        .map_or(0, |bucket| bucket.count)
}

#[test]
fn empty_selected_genre_preserves_own_dimension_counts() {
    let (_tmp, store, library, _) = query_model_support::setup_count(2);
    let (state, _) = query_model_support::run(&store, library, true, "facets-empty-genre");
    assert_eq!(state, "succeeded");
    let facets = store
        .library_facets(&FacetRequest {
            library_id: library,
            user_id: None,
            filter: BrowseFilter {
                genres: vec!["absent".into()],
                ..Default::default()
            },
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(facets.total, 0);
    assert_eq!(
        facets.groups["genres"]
            .iter()
            .find(|v| v.value == "Drama")
            .unwrap()
            .count,
        2
    );
    assert_eq!(
        facets.groups["decades"]
            .iter()
            .find(|v| v.value == "2020s")
            .unwrap()
            .count,
        0
    );
}

#[test]
fn watch_only_filters_static_buckets() {
    let (tmp, store, library, ids) = query_model_support::setup_count(2);
    let comedy: domain::MetadataFields = serde_json::from_value(serde_json::json!({
        "genres":{"state":"value","value":[{"source":"fixture","name":"Comedy","filter_key":"Comedy"}]}
    })).unwrap();
    store
        .set_metadata_overrides(
            ids[1].parse().unwrap(),
            domain::EntityScope::Work,
            "en-US",
            &comedy,
            1,
        )
        .unwrap();
    let user = domain::UserId::new();
    store
        .mutate_playback(
            ids[0].parse().unwrap(),
            &store::playback::PlaybackMutation::marks(user, -1, -1, None, Some(true)),
            100,
        )
        .unwrap();
    let (state, _) = query_model_support::run(&store, library, true, "facets-watch-only");
    assert_eq!(state, "succeeded");
    let facets = store
        .library_facets(&FacetRequest {
            library_id: library,
            user_id: Some(user),
            tier: FacetTier::All,
            filter: BrowseFilter {
                watch: Some("favorite".into()),
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(facets.total, 1);
    assert_eq!(count(&facets, "genres", "Drama"), 1);
    assert_eq!(count(&facets, "genres", "Comedy"), 0);
    assert_eq!(count(&facets, "watch", "favorite"), 1);
    let excluded = store
        .library_facets(&FacetRequest {
            library_id: library,
            user_id: Some(user),
            tier: FacetTier::All,
            filter: BrowseFilter {
                genres: vec!["Comedy".into()],
                ..Default::default()
            },
        })
        .unwrap();
    assert_eq!(count(&excluded, "watch", "favorite"), 0);
    assert!(
        !store
            .library_facets(&FacetRequest {
                library_id: library,
                user_id: None,
                tier: FacetTier::All,
                filter: BrowseFilter::default(),
            })
            .unwrap()
            .groups["watch"]
            .iter()
            .any(|v| v.value == "favorite")
    );
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE user_library_items SET seen=0,played=1 WHERE user_id=?1 AND library_id=?2",
        rusqlite::params![user.to_string(), library.to_string()],
    )
    .unwrap();
    let played = store
        .library_facets(&FacetRequest {
            library_id: library,
            user_id: Some(user),
            filter: BrowseFilter {
                watch: Some("played".into()),
                ..Default::default()
            },
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(played.total, 1);
    assert_eq!(count(&played, "watch", "unwatched"), 2);
    assert_eq!(count(&played, "watch", "played"), 0);
}

#[test]
fn unfiltered_facets_read_counts_not_all_cards() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::open(dir.path()).unwrap();
    let library = work_support::seed(&store, domain::MediaKind::Movie, 400);
    let request = FacetRequest {
        library_id: library,
        user_id: None,
        filter: BrowseFilter::default(),
        tier: FacetTier::All,
    };
    let (small, small_work) = store.library_facets_with_stats(&request).unwrap();
    assert_eq!(small.total, 400);
    assert_eq!(count(&small, "genres", "Drama"), 200);
    work_support::seed_into(
        &store,
        domain::MediaKind::Movie,
        400,
        3600,
        library,
        "/query-work/movie",
    );
    let (large, large_work) = store.library_facets_with_stats(&request).unwrap();
    assert_eq!(large.total, 4000);
    assert_eq!(count(&large, "genres", "Drama"), 2000);
    assert_eq!(count(&large, "decades", "1990s"), 0);
    assert!(
        large_work.vm_steps < small_work.vm_steps + 2000,
        "facet VM grew with card count: {small_work:?} -> {large_work:?}"
    );
}

#[derive(Clone, Copy)]
struct Fact {
    genres: &'static [&'static str],
    country: &'static str,
    year: i32,
    rating: f64,
    runtime: i32,
    language: Option<&'static str>,
    resolution: &'static str,
    hdr: bool,
    missing: bool,
    played: bool,
    favorite: bool,
}

const FACTS: [Fact; 12] = [
    Fact {
        genres: &["Drama", "Comedy"],
        country: "US",
        year: 1985,
        rating: 7.4,
        runtime: 60,
        language: Some("en"),
        resolution: "720p",
        hdr: false,
        missing: false,
        played: false,
        favorite: true,
    },
    Fact {
        genres: &["Drama"],
        country: "JP",
        year: 1995,
        rating: 7.5,
        runtime: 90,
        language: Some("ja"),
        resolution: "1080p",
        hdr: false,
        missing: false,
        played: true,
        favorite: false,
    },
    Fact {
        genres: &["Comedy"],
        country: "US",
        year: 2005,
        rating: 8.0,
        runtime: 120,
        language: None,
        resolution: "2160p",
        hdr: true,
        missing: false,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Drama"],
        country: "JP",
        year: 2015,
        rating: 6.0,
        runtime: 130,
        language: Some("ja"),
        resolution: "720p",
        hdr: false,
        missing: true,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Comedy"],
        country: "US",
        year: 2025,
        rating: 9.0,
        runtime: 60,
        language: Some("en"),
        resolution: "1080p",
        hdr: false,
        missing: false,
        played: true,
        favorite: true,
    },
    Fact {
        genres: &["Drama"],
        country: "US",
        year: 1985,
        rating: 7.5,
        runtime: 90,
        language: None,
        resolution: "2160p",
        hdr: true,
        missing: false,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Comedy"],
        country: "JP",
        year: 1995,
        rating: 8.0,
        runtime: 120,
        language: Some("ja"),
        resolution: "720p",
        hdr: false,
        missing: false,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Drama", "Comedy"],
        country: "US",
        year: 2005,
        rating: 7.4,
        runtime: 130,
        language: Some("en"),
        resolution: "1080p",
        hdr: false,
        missing: false,
        played: true,
        favorite: false,
    },
    Fact {
        genres: &["Drama"],
        country: "JP",
        year: 2015,
        rating: 6.0,
        runtime: 60,
        language: None,
        resolution: "2160p",
        hdr: true,
        missing: false,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Comedy"],
        country: "US",
        year: 2025,
        rating: 9.0,
        runtime: 90,
        language: Some("en"),
        resolution: "720p",
        hdr: false,
        missing: false,
        played: false,
        favorite: false,
    },
    Fact {
        genres: &["Drama"],
        country: "JP",
        year: 1985,
        rating: 7.5,
        runtime: 120,
        language: Some("ja"),
        resolution: "1080p",
        hdr: false,
        missing: false,
        played: true,
        favorite: false,
    },
    Fact {
        genres: &["Comedy"],
        country: "US",
        year: 1995,
        rating: 8.0,
        runtime: 130,
        language: None,
        resolution: "2160p",
        hdr: true,
        missing: false,
        played: false,
        favorite: false,
    },
];

fn oracle_member(f: Fact, group: &str, value: &str) -> bool {
    match group {
        "genres" => f.genres.contains(&value),
        "countries" => f.country == value,
        "decades" => match value {
            "earlier" => f.year < 1990,
            "1990s" => (1990..2000).contains(&f.year),
            "2000s" => (2000..2010).contains(&f.year),
            "2010s" => (2010..2020).contains(&f.year),
            "2020s" => (2020..2030).contains(&f.year),
            _ => false,
        },
        "watch" => match value {
            "unwatched" => !f.played,
            "played" => f.played,
            "watching" => false,
            "favorite" => f.favorite,
            _ => false,
        },
        "ratings" => f.rating >= value.parse::<f64>().unwrap(),
        "runtimes" => match value {
            "lte60" => f.runtime <= 60,
            "60to90" => (61..=90).contains(&f.runtime),
            "90to120" => (91..=120).contains(&f.runtime),
            "gt120" => f.runtime > 120,
            _ => false,
        },
        "languages" => f.language == Some(value),
        "resolutions" => f.resolution == value,
        "hdr" => f.hdr == (value == "true"),
        "stock" => match value {
            "missing" => f.missing,
            "unscraped" => true,
            _ => false,
        },
        _ => false,
    }
}

fn oracle_check(
    actual: &store::library_query::LibraryFacets,
    selected: Option<(&str, &str)>,
    primary: bool,
) {
    let expected_total = FACTS
        .iter()
        .filter(|f| selected.is_none_or(|(g, v)| oracle_member(**f, g, v)))
        .count() as u64;
    assert_eq!(actual.total, expected_total);
    let groups = if primary {
        vec!["genres", "countries", "decades", "watch"]
    } else {
        vec![
            "genres",
            "countries",
            "decades",
            "watch",
            "ratings",
            "runtimes",
            "languages",
            "resolutions",
            "hdr",
            "stock",
        ]
    };
    assert_eq!(actual.groups.len(), groups.len());
    for group in groups {
        let buckets = match group {
            "genres" => vec!["Drama", "Comedy"],
            "countries" => vec!["US", "JP"],
            "decades" => vec!["earlier", "1990s", "2000s", "2010s", "2020s"],
            "watch" => vec!["unwatched", "watching", "played", "favorite"],
            "ratings" => vec!["6", "7", "8", "9"],
            "runtimes" => vec!["lte60", "60to90", "90to120", "gt120"],
            "languages" => vec!["en", "ja"],
            "resolutions" => vec!["720p", "1080p", "2160p"],
            "hdr" => vec!["true", "false"],
            "stock" => vec!["missing", "unscraped"],
            _ => unreachable!(),
        };
        let expected = buckets
            .iter()
            .map(|value| {
                let n = FACTS
                    .iter()
                    .filter(|f| {
                        (selected.is_none_or(|(g, v)| g == group || oracle_member(**f, g, v)))
                            && oracle_member(**f, group, value)
                    })
                    .count() as u64;
                ((*value).to_string(), n)
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let observed = actual.groups[group]
            .iter()
            .map(|b| (b.value.clone(), b.count))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(observed, expected, "selected={selected:?}, group={group}");
    }
}

fn oracle_filter(selection: Option<(&str, &str)>) -> BrowseFilter {
    let mut filter = BrowseFilter::default();
    if let Some((group, value)) = selection {
        match group {
            "genres" => filter.genres = vec![value.into()],
            "countries" => filter.countries = vec![value.into()],
            "decades" => filter.decades = vec![value.into()],
            "watch" => filter.watch = Some(value.into()),
            "ratings" => filter.min_rating = Some(7.5),
            "runtimes" => filter.runtimes = vec![value.into()],
            "languages" => filter.languages = vec![value.into()],
            "resolutions" => filter.resolutions = vec![value.into()],
            "hdr" => filter.hdr = Some(true),
            "stock" => filter.stock = vec![value.into()],
            _ => unreachable!(),
        }
    }
    filter
}

fn seed_oracle_facts() -> (tempfile::TempDir, Store, domain::LibraryId, domain::UserId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = work_support::seed(&store, domain::MediaKind::Movie, 12);
    let mut db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    for (n, f) in FACTS.iter().enumerate() {
        let media: String = tx
            .query_row(
                "SELECT media_id FROM library_items WHERE library_id=?1 AND title=?2",
                rusqlite::params![library.to_string(), format!("Item {n:06}")],
                |r| r.get(0),
            )
            .unwrap();
        let genres = f
            .genres
            .iter()
            .map(|name| serde_json::json!({"source":"fixture","name":name,"filter_key":name}))
            .collect::<Vec<_>>();
        let mut fields = serde_json::json!({"genres":{"state":"value","value":genres},"countries":{"state":"value","value":[f.country]},"year":{"state":"value","value":f.year},"rating":{"state":"value","value":f.rating},"runtime_minutes":{"state":"value","value":f.runtime}});
        if let Some(language) = f.language {
            fields["language"] = serde_json::json!({"state":"value","value":language});
        }
        tx.execute("UPDATE library_metadata_inputs SET fields_json=?1,present_fields_json=?2 WHERE library_id=?3 AND media_id=?4",rusqlite::params![fields.to_string(),"[\"genres\",\"countries\",\"year\",\"rating\",\"runtime_minutes\",\"language\"]",library.to_string(),media]).unwrap();
        tx.execute("UPDATE library_files SET resolution=?1,hdr=?2,presence_state=?3 WHERE library_id=?4 AND media_id=?5",rusqlite::params![f.resolution,if f.hdr {Some("hdr10")} else {None},if f.missing {"missing"} else {"present"},library.to_string(),media]).unwrap();
    }
    tx.commit().unwrap();
    query_model_support::run(&store, library, true, "facets-12-repair");
    let user = domain::UserId::new();
    let db = rusqlite::Connection::open(dir.path().join("library.db")).unwrap();
    for (n, f) in FACTS.iter().enumerate() {
        if f.played || f.favorite {
            let media: String = db
                .query_row(
                    "SELECT media_id FROM library_items WHERE library_id=?1 AND title=?2",
                    rusqlite::params![library.to_string(), format!("Item {n:06}")],
                    |r| r.get(0),
                )
                .unwrap();
            store
                .mutate_playback(
                    media.parse().unwrap(),
                    &store::playback::PlaybackMutation::marks(
                        user,
                        -1,
                        -1,
                        f.played.then_some(true),
                        f.favorite.then_some(true),
                    ),
                    100 + n as i64,
                )
                .unwrap();
        }
    }
    (dir, store, library, user)
}

#[test]
fn facets_match_reference_for_every_selected_dimension() {
    let (_dir, store, library, user) = seed_oracle_facts();
    for tier in [FacetTier::Primary, FacetTier::All] {
        for selection in [
            None,
            Some(("genres", "Drama")),
            Some(("countries", "JP")),
            Some(("decades", "1990s")),
            Some(("watch", "favorite")),
            Some(("watch", "played")),
            Some(("watch", "unwatched")),
            Some(("ratings", "7.5")),
            Some(("runtimes", "60to90")),
            Some(("languages", "ja")),
            Some(("resolutions", "2160p")),
            Some(("hdr", "true")),
            Some(("stock", "missing")),
        ] {
            let filter = oracle_filter(selection);
            let actual = store
                .library_facets(&FacetRequest {
                    library_id: library,
                    user_id: Some(user),
                    filter,
                    tier,
                })
                .unwrap();
            oracle_check(&actual, selection, matches!(tier, FacetTier::Primary));
        }
    }
    check_empty_common_set(&store, library, user);
    check_anonymous_watch(&store, library);
}

fn check_empty_common_set(store: &Store, library: domain::LibraryId, user: domain::UserId) {
    let empty = store
        .library_facets(&FacetRequest {
            library_id: library,
            user_id: Some(user),
            filter: BrowseFilter {
                identity: Some("confirmed".into()),
                ..Default::default()
            },
            tier: FacetTier::All,
        })
        .unwrap();
    assert_eq!(empty.total, 0);
    assert!(
        empty
            .groups
            .values()
            .flatten()
            .all(|bucket| bucket.count == 0)
    );
    assert!(
        empty.groups["genres"]
            .iter()
            .any(|bucket| bucket.value == "Drama")
    );
    assert!(
        empty.groups["watch"]
            .iter()
            .any(|bucket| bucket.value == "favorite")
    );
}

fn check_anonymous_watch(store: &Store, library: domain::LibraryId) {
    for (watch, expected) in [("played", 0), ("unwatched", 12), ("favorite", 0)] {
        let anonymous = store
            .library_facets(&FacetRequest {
                library_id: library,
                user_id: None,
                filter: BrowseFilter {
                    watch: Some(watch.into()),
                    ..Default::default()
                },
                tier: FacetTier::All,
            })
            .unwrap();
        assert_eq!(anonymous.total, expected, "anonymous watch={watch}");
        assert!(
            !anonymous.groups["watch"]
                .iter()
                .any(|bucket| bucket.value == "favorite")
        );
    }
}

#[test]
fn paired_selected_dimensions_match_reference() {
    let (_dir, store, library, user) = seed_oracle_facts();
    for (filter, selected) in [
        (
            BrowseFilter {
                genres: vec!["Drama".into()],
                countries: vec!["JP".into()],
                ..Default::default()
            },
            [("genres", "Drama"), ("countries", "JP")],
        ),
        (
            BrowseFilter {
                watch: Some("favorite".into()),
                resolutions: vec!["720p".into()],
                ..Default::default()
            },
            [("watch", "favorite"), ("resolutions", "720p")],
        ),
    ] {
        let actual = store
            .library_facets(&FacetRequest {
                library_id: library,
                user_id: Some(user),
                filter,
                tier: FacetTier::All,
            })
            .unwrap();
        let expected_total = FACTS
            .iter()
            .filter(|f| selected.iter().all(|(g, v)| oracle_member(**f, g, v)))
            .count() as u64;
        assert_eq!(actual.total, expected_total, "selected={selected:?}");
        for group in ["genres", "countries", "watch", "resolutions"] {
            for bucket in &actual.groups[group] {
                let expected = FACTS
                    .iter()
                    .filter(|f| {
                        selected
                            .iter()
                            .all(|(g, v)| *g == group || oracle_member(**f, g, v))
                            && oracle_member(**f, group, &bucket.value)
                    })
                    .count() as u64;
                assert_eq!(
                    bucket.count, expected,
                    "selected={selected:?}, group={group}, value={}",
                    bucket.value
                );
            }
        }
    }
}
