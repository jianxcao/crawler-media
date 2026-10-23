//! Write-time protocol index: the card reduction stores Jellyfin genre/person ids and the
//! casefolded search haystack, so the Jellyfin page never derives them per request.

use domain::{Confidence, Media, MediaId, MediaKind, QualitySource};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

fn seed() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("data")).unwrap();
    let root = dir.path().join("movies");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "Idx",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Indexed Film".into(),
        year: Some(2001),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: Vec::new(),
    };
    store.insert_media(&media).unwrap();
    std::fs::write(root.join("Indexed Film.mkv"), b"payload").unwrap();
    std::fs::write(
        root.join("Indexed Film.nfo"),
        "<movie><title>Indexed Film</title><originaltitle>Film Indexé</originaltitle>\
         <plot>A Quiet OVERVIEW.</plot><genre>Drama</genre>\
         <actor><name>Jane Roe</name><role>Lead</role></actor>\
         <director>Max Mustermann</director></movie>",
    )
    .unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: root.join("Indexed Film.mkv").display().to_string(),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            ..Default::default()
        },
    )
    .unwrap();
    (dir, store, media.id.to_string())
}

fn wire(key: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes())
        .simple()
        .to_string()
}

#[test]
fn card_reduction_writes_protocol_terms_and_search_text() {
    let (dir, _store, media) = seed();
    let conn = rusqlite::Connection::open(dir.path().join("data/library.db")).unwrap();
    let terms = |kind: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT term_value FROM library_item_terms
                 WHERE media_id = ?1 AND term_kind = ?2 ORDER BY term_value",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![media, kind], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    };
    let drama = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"Drama")
        .simple()
        .to_string();
    let genres = terms("jellyfin_genre");
    assert!(genres.contains(&format!("id:{drama}")), "{genres:?}");
    assert!(genres.contains(&"name:drama".to_string()), "{genres:?}");
    let people = terms("person_id");
    assert!(people.contains(&wire("name:jane roe:Actor")), "{people:?}");
    assert!(
        people.contains(&wire("name:max mustermann:Director")),
        "{people:?}"
    );
    let haystack: String = conn
        .query_row(
            "SELECT search_text FROM library_items WHERE media_id = ?1",
            [&media],
            |r| r.get(0),
        )
        .unwrap();
    for needle in ["indexed film", "film indexé", "a quiet overview."] {
        assert!(
            haystack.contains(needle),
            "{needle:?} missing from {haystack:?}"
        );
    }
}
