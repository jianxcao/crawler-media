//! Q4 measurement harness: `Store::protocol_item_page` (the Jellyfin Items page) at scale.
//!
//! Manual only:
//! `PERF_SIZES=50000 cargo test --release -p api --test jellyfin_page_perf -- --ignored --nocapture`
//! Prints median/p95 per query shape; the smoke test keeps the fixture honest in debug runs.

use std::path::Path;
use std::time::{Duration, Instant};

use api::Store;
use domain::{
    Confidence, LibraryId, Media, MediaId, MediaKind, ProtocolItemQuery, ProtocolItemType,
    ProtocolParent, ProtocolSort, ProtocolSortKey, QualitySource, UserId,
};

const GENRES: &[&str] = &["Drama", "Action", "Comedy", "Horror", "Romance", "Thriller"];

fn seed(count: usize) -> (tempfile::TempDir, Store, LibraryId) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    let library: LibraryId = store
        .create_library(
            MediaKind::Movie,
            "Perf",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap()
        .id
        .parse()
        .unwrap();
    for index in 0..count {
        let title = format!("Perf {index:06}");
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: title.clone(),
            year: Some(1990 + (index % 35) as u16),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
            aliases: Vec::new(),
        };
        store.insert_media(&media).unwrap();
        write_movie(&root, &title, index);
        let spec = store::test_support::FixtureFile {
            media_id: media.id,
            path: root.join(format!("{title}.mkv")).display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        };
        store::test_support::publish_fixture_file(&store, &spec).unwrap();
    }
    (dir, store, library)
}

fn write_movie(root: &Path, title: &str, index: usize) {
    std::fs::write(root.join(format!("{title}.mkv")), b"payload").unwrap();
    let genre = GENRES[index % GENRES.len()];
    let actors: String = (0..3)
        .map(|k| {
            format!(
                "<actor><name>Actor {}</name><role>Actor</role></actor>",
                (index * 7 + k) % 2000
            )
        })
        .collect();
    let nfo = format!(
        "<movie><title>{title}</title><plot>Overview number {index}.</plot><genre>{genre}</genre>\
         <studio>Studio {}</studio>{actors}</movie>",
        index % 50
    );
    std::fs::write(root.join(format!("{title}.nfo")), nfo).unwrap();
}

fn query(library: LibraryId, user: UserId) -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: user,
        parent: ProtocolParent::Library(library),
        series_id: None,
        season: None,
        person_ids: Vec::new(),
        studio_ids: Vec::new(),
        search_term: None,
        genres: Vec::new(),
        genre_ids: Vec::new(),
        filters: Vec::new(),
        is_favorite: None,
        is_played: None,
        include_types: vec![ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        }],
        offset: 0,
        limit: 100,
    }
}

fn person_wire_id(key: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

fn shapes(library: LibraryId, user: UserId) -> Vec<(&'static str, ProtocolItemQuery)> {
    let base = query(library, user);
    let mut deep = base.clone();
    deep.offset = 40_000;
    let mut genre = base.clone();
    genre.genres = vec!["Drama".into()];
    let mut person = base.clone();
    person.person_ids = vec![person_wire_id("name:actor 7:Actor")];
    let mut search = base.clone();
    search.search_term = Some("Perf 00001".into());
    vec![
        ("first_page", base),
        ("deep_offset", deep),
        ("genre", genre),
        ("person", person),
        ("search", search),
    ]
}

#[test]
#[ignore = "manual release measurement"]
fn protocol_page_latency() {
    let sizes: Vec<usize> = std::env::var("PERF_SIZES")
        .unwrap_or_else(|_| "50000".into())
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    println!("size\tshape\ttotal\tpage\tmedian_ms\tp95_ms");
    for size in sizes {
        let (dir, store, library) = seed(size);
        if std::env::var_os("PERF_KEEP").is_some() {
            println!("kept\t{}", dir.path().display());
            std::mem::forget(dir);
        }
        for (name, q) in shapes(library, UserId::new()) {
            let mut times: Vec<Duration> = Vec::new();
            let mut last = None;
            for _ in 0..7 {
                let start = Instant::now();
                let page = store.protocol_item_page(&q).unwrap();
                times.push(start.elapsed());
                last = Some(page);
            }
            times.sort();
            let page = last.unwrap();
            println!(
                "{size}\t{name}\t{}\t{}\t{:.3}\t{:.3}",
                page.total,
                page.items.len(),
                times[3].as_secs_f64() * 1e3,
                times[6].as_secs_f64() * 1e3
            );
        }
    }
}

#[test]
fn harness_shapes_return_expected_counts() {
    let (_dir, store, library) = seed(24);
    let results: Vec<(&str, usize)> = shapes(library, UserId::new())
        .into_iter()
        .map(|(name, q)| (name, store.protocol_item_page(&q).unwrap().total as usize))
        .collect();
    assert_eq!(results[0], ("first_page", 24));
    assert_eq!(results[2], ("genre", 4), "every 6th movie is Drama");
    assert_eq!(
        results[3].1, 1,
        "Actor 7 is cast only in movie 1 at this size"
    );
    assert_eq!(results[4], ("search", 10), "Perf 000010..Perf 000019");
}
