#[path = "subscription_hot_reads/support.rs"]
mod support;
use support::{Fixture, body, open};

#[tokio::test]
async fn open_season_uses_exact_real_tmdb_details_keys_and_rejects_collisions() {
    let fixture = Fixture::new();
    fixture.add(open());
    fixture.cache(
        "tmdb",
        "/tv/12?append_to_response=translations,alternative_titles&language=zh-CN",
        &body(8),
        1,
    );
    for (source, key) in [
        (
            "tmdb",
            "/tv/312?append_to_response=translations,alternative_titles&language=zh-CN",
        ),
        ("douban", "/tv/12?language=zh-CN"),
        ("tmdb", "/tv/12/season/1?language=zh-CN"),
        ("tmdb", "/tv/12/similar?language=zh-CN"),
        (
            "tmdb",
            "/tv/12?append_to_response=translations,alternative_titles&language=ja-JP",
        ),
    ] {
        fixture.cache(source, key, &body(99), 10);
    }
    let (rows, _) = fixture.list().await;
    assert_eq!(
        rows[0]["progress"]["total"], 8,
        "season total must belong to exact TMDB work/details/locale"
    );
    fixture.cache(
        "tmdb",
        "/tv/12?append_to_response=translations,alternative_titles&language=zh-CN",
        "broken JSON",
        20,
    );
    fixture.cache(
        "tmdb",
        "/tv/12?append_to_response=aggregate_credits,content_ratings,translations&language=zh-CN",
        &body(9),
        1,
    );
    let (rows, _) = fixture.list().await;
    assert_eq!(
        rows[0]["progress"]["total"], 9,
        "malformed preferred entry must allow another valid real producer key"
    );
}

#[tokio::test]
async fn list_batches_shared_media_facts_pending_times_and_cache_with_one_request_snapshot() {
    let fixture = Fixture::new();
    for _ in 0..4 {
        fixture.add(open());
    }
    let (small, small_work) = fixture.list().await;
    for _ in 0..400 {
        fixture.add(open());
    }
    let (large, large_work) = fixture.list().await;
    assert_eq!(small.as_array().unwrap().len(), 4);
    assert_eq!(large.as_array().unwrap().len(), 404);
    for row in large.as_array().unwrap() {
        assert_eq!(row["progress"]["total"], 1);
        assert_eq!(row["progress"]["imported"], 0);
        assert_eq!(row["created_at"].as_str().unwrap().len(), 24);
        assert_eq!(row["updated_at"].as_str().unwrap().len(), 24);
    }
    eprintln!("P4 batch HTTP work: {small_work:?} -> {large_work:?}");
    assert!(
        large_work.statements.unwrap() <= small_work.statements.unwrap() + 20,
        "one batch per input kind, not per Subscribe: {small_work:?} -> {large_work:?}"
    );
    assert_eq!(
        large_work.reader_checkouts,
        Some(4),
        "declared App/Library/Subscribe/Cache leases are reused"
    );
}

#[tokio::test]
async fn request_diagnostics_report_all_database_result_volume_without_unrelated_cache_bodies() {
    let fixture = Fixture::new();
    fixture.add(open());
    let (before, work_before) = fixture.list().await;
    let large_body = "媒".repeat(4096);
    for i in 0..100 {
        fixture.cache("douban", &format!("unrelated-{i}"), &large_body, i);
    }
    let (after, work_after) = fixture.list().await;
    assert_eq!(before, after);
    let before_json = serde_json::to_value(work_before).unwrap();
    let after_json = serde_json::to_value(work_after).unwrap();
    let before_bytes = before_json["sql_result_value_bytes"].as_u64().expect(
        "HTTP diagnostics must propagate the measured SQL result-value volume from All databases",
    );
    assert_eq!(
        after_json["sql_result_value_bytes"].as_u64(),
        Some(before_bytes)
    );
    assert_eq!(after_json["sql_rows"], before_json["sql_rows"]);
}

#[tokio::test]
async fn season_hints_follow_configured_locale_and_neutral_fallback() {
    let fixture = Fixture::new();
    fixture.add(open());
    fixture.cache("tmdb", "/tv/12?language=zh-CN", &body(8), 1);
    fixture.cache("tmdb", "/tv/12?language=ja-JP", &body(12), 1);
    fixture.cache("tmdb", "/tv/12", &body(7), 1);
    let store = fixture.state.store();
    let revision = store.metadata_language().unwrap().revision;
    store
        .change_metadata_language(revision, "ja-JP", "hot-read-locale", 1000)
        .unwrap();
    assert_eq!(fixture.list().await.0[0]["progress"]["total"], 12);
    store
        .delete_catalog_cache("tmdb", "/tv/12?language=ja-JP")
        .unwrap();
    assert_eq!(fixture.list().await.0[0]["progress"]["total"], 7);
    store.delete_catalog_cache("tmdb", "/tv/12").unwrap();
    assert_eq!(
        fixture.list().await.0[0]["progress"]["total"],
        1,
        "another locale must not silently supply the season hint"
    );
}

#[tokio::test]
async fn persisted_slot_progress_survives_missing_moved_and_late_path_facts() {
    use domain::{Coverage, QualityFact, SubscribeFacts};
    use store::test_support::{FixtureFile, publish_fixture_file};
    let fixture = Fixture::new();
    let sub = fixture.add(Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: Some(8),
    });
    let store = fixture.state.store();
    let root = store
        .default_library(domain::MediaKind::Tv)
        .unwrap()
        .unwrap()
        .root_paths[0]
        .clone();
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join("episode3.mkv");
    std::fs::write(&old, b"fixture-media-source").unwrap();
    let file = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: fixture.media,
            path: old.display().to_string(),
            season: Some(1),
            episode: Some(3),
            resolution: Some("2160p".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let moved = root.join("moved3.mkv");
    store
        .rename_file_within_root(file.file_id, &file.version, &old, &moved)
        .unwrap();
    store
        .verify_library_files_with(std::slice::from_ref(&root), 2000, |_| false)
        .unwrap();
    let mut facts = SubscribeFacts::default();
    for (ep, path) in [(1, root.join("never-published1.mkv")), (2, moved), (3, old)] {
        facts.upsert(
            Some(1),
            Some(ep),
            QualityFact {
                score: 100,
                path: Some(path.display().to_string()),
            },
        );
    }
    // A late path callback is persisted even though its old source moved. The
    // list must retain the same slot semantics as the public round fact loader.
    store.save_subscribe_facts(sub, &facts).unwrap();
    assert_eq!(
        store.load_subscribe_facts(sub).unwrap().entries().count(),
        3
    );
    store
        .merge_pending(
            sub,
            &[(
                80,
                store::PendingDownload {
                    torrent: domain::Torrent {
                        site_id: domain::SiteId::new(),
                        title: "Hot.Subscribe.S01E03-E04.1080p".into(),
                        enclosure: "https://fixture.invalid/hot4".into(),
                        size_bytes: None,
                        seeders: None,
                        free: false,
                        hr: Some(false),
                        imdb_id: None,
                        id: None,
                        leechers: None,
                        snatched: None,
                        upload_time: None,
                        detail_url: None,
                        category: None,
                        poster_url: None,
                    },
                    release_override: None,
                    downloader_id: None,
                    submitted_at: None,
                },
            )],
        )
        .unwrap();
    let (rows, work) = fixture.list().await;
    assert_eq!(rows[0]["progress"]["imported"], 3);
    assert_eq!(rows[0]["progress"]["total"], 8);
    assert_eq!(rows[0]["progress"]["grabbing"], 1);
    assert_eq!(rows[0]["progress"]["downloaded"], 4);
    assert_eq!(rows[0]["progress"]["missing"], 4);
    let inputs = store.subscribe_view_inputs(&[sub]).unwrap();
    for ((season, episode), fact) in facts.entries() {
        assert_eq!(inputs[&sub].facts.get(season, episode), Some(fact));
    }
    eprintln!("P4 persisted/missing/moved facts HTTP work: {work:?}");
}
