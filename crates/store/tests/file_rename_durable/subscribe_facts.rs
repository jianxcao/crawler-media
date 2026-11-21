use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use domain::{
    Coverage, FetchMode, Filter, FilterId, Media, MediaId, MediaKind, QualityFact, Subscribe,
    SubscribeFacts, SubscribeId, UserId,
};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

struct TvSubFx {
    _tmp: tempfile::TempDir,
    store: Arc<Store>,
    root: PathBuf,
    media_id: MediaId,
    sub_id: SubscribeId,
    f1: domain::LibraryFile,
    e01_old: PathBuf,
    e02_old: PathBuf,
}

fn seed_tv_subscribe() -> TvSubFx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let store = Arc::new(Store::open(tmp.path().join("data")).unwrap());
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media_id = MediaId::new();
    store
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "Show".into(),
            year: Some(2024),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let e01_old = root.join("S01E01.mkv");
    let e02_old = root.join("S01E02.mkv");
    std::fs::write(&e01_old, b"e01").unwrap();
    std::fs::write(&e02_old, b"e02").unwrap();
    let f1 = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id,
            path: e01_old.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let sub_id = SubscribeId::new();
    let filter_id = FilterId::new();
    store
        .insert_filter(&Filter {
            id: filter_id,
            name: "f".into(),
            atoms: Vec::new(),
            keep_old_versions: false,
        })
        .unwrap();
    store
        .insert_subscribe(&Subscribe {
            id: sub_id,
            user_id: UserId::new(),
            media_id,
            coverage: Coverage::Tv {
                season: 1,
                episode_from: 1,
                episode_to: Some(2),
            },
            fetch_mode: FetchMode::Search,
            filter_id,
            wash_cut: true,
            wash_cut_filter_id: None,
            full_season_pack: false,
            downloader_id: None,
            library_id: None,
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
            keep_old_versions: false,
        })
        .unwrap();
    let mut initial = SubscribeFacts::default();
    initial.upsert(
        Some(1),
        Some(1),
        QualityFact {
            score: 80,
            path: Some(e01_old.to_string_lossy().into_owned()),
        },
    );
    initial.upsert(
        Some(1),
        Some(2),
        QualityFact {
            score: 80,
            path: Some(e02_old.to_string_lossy().into_owned()),
        },
    );
    store.save_subscribe_facts(sub_id, &initial).unwrap();
    TvSubFx {
        _tmp: tmp,
        store,
        root,
        media_id,
        sub_id,
        f1,
        e01_old,
        e02_old,
    }
}

#[test]
fn rename_fact_path_update_preserves_concurrent_transfer_upgrade_and_third_path() {
    let fx = seed_tv_subscribe();
    let e01_renamed = fx.root.join("S01E01.renamed.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.f1.file_id, &fx.f1.version, &fx.e01_old, &e01_renamed, 1)
        .unwrap();
    let receipt = store::library_write::execute_rename_disk(&req).unwrap();
    fx.store.record_rename_disk(&receipt, 2).unwrap();

    let e02_upgraded = fx.root.join("S01E02.4K.mkv");
    let writer_held = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
        let store_r = Arc::clone(&fx.store);
        let held = Arc::clone(&writer_held);
        let rename_id = req.rename_id.clone();
        let rename_handle = scope.spawn(move || {
            held.wait();
            store_r.complete_file_rename(&rename_id, 3).unwrap();
        });
        fx.store
            .with_writer(store::Db::Subscribe, |conn| {
                writer_held.wait();
                // Allow a non-atomic read+write path to load the stale E02 snapshot first.
                std::thread::sleep(std::time::Duration::from_millis(30));
                conn.execute(
                    "UPDATE subscribe_facts SET score = 120, path = ?2
                     WHERE subscribe_id = ?1 AND season = 1 AND episode = 2",
                    rusqlite::params![fx.sub_id.to_string(), e02_upgraded.to_string_lossy()],
                )?;
                Ok(())
            })
            .unwrap();
        rename_handle.join().unwrap();
    });

    let after = fx.store.load_subscribe_facts(fx.sub_id).unwrap();
    assert_eq!(
        after.get(Some(1), Some(1)),
        Some(&QualityFact {
            score: 80,
            path: Some(e01_renamed.to_string_lossy().into_owned()),
        })
    );
    assert_eq!(
        after.get(Some(1), Some(2)),
        Some(&QualityFact {
            score: 120,
            path: Some(e02_upgraded.to_string_lossy().into_owned()),
        }),
        "concurrent E02 upgrade must not be overwritten by E01 rename"
    );

    // Same episode already upgraded to a third path: replaying the rename leaves it intact.
    let e01_third = fx.root.join("S01E01.4K.mkv");
    let mut third = after.clone();
    third.replace(
        Some(1),
        Some(1),
        QualityFact {
            score: 150,
            path: Some(e01_third.to_string_lossy().into_owned()),
        },
    );
    fx.store.save_subscribe_facts(fx.sub_id, &third).unwrap();
    fx.store
        .rewrite_fact_paths(
            fx.media_id,
            &fx.e01_old.to_string_lossy(),
            &e01_renamed.to_string_lossy(),
        )
        .unwrap();
    let final_facts = fx.store.load_subscribe_facts(fx.sub_id).unwrap();
    assert_eq!(
        final_facts.get(Some(1), Some(1)),
        Some(&QualityFact {
            score: 150,
            path: Some(e01_third.to_string_lossy().into_owned()),
        })
    );
    assert_eq!(
        final_facts.get(Some(1), Some(2)),
        Some(&QualityFact {
            score: 120,
            path: Some(e02_upgraded.to_string_lossy().into_owned()),
        })
    );
    let _ = fx.e02_old;
}

#[test]
fn transfer_diff_commit_after_rename_preserves_renamed_episode_path() {
    let fx = seed_tv_subscribe();
    let baseline = fx.store.load_subscribe_facts(fx.sub_id).unwrap();

    // Rename E01 commits before Transfer persists its E02 upgrade.
    let e01_renamed = fx.root.join("S01E01.renamed.mkv");
    let req = fx
        .store
        .begin_file_rename(fx.f1.file_id, &fx.f1.version, &fx.e01_old, &e01_renamed, 1)
        .unwrap();
    let _pub = fx.store.apply_file_rename(&req, 2).unwrap();

    // Transfer's outcome was built from `baseline` (holding E01's old path) and upgraded E02.
    let e02_upgraded = fx.root.join("S01E02.4K.mkv");
    let mut outcome = baseline.clone();
    outcome.replace(
        Some(1),
        Some(2),
        QualityFact {
            score: 120,
            path: Some(e02_upgraded.to_string_lossy().into_owned()),
        },
    );
    fx.store
        .save_subscribe_facts_diff(fx.sub_id, &baseline, &outcome)
        .unwrap();

    let after = fx.store.load_subscribe_facts(fx.sub_id).unwrap();
    assert_eq!(
        after.get(Some(1), Some(1)),
        Some(&QualityFact {
            score: 80,
            path: Some(e01_renamed.to_string_lossy().into_owned()),
        }),
        "Transfer diff save must not overwrite E01's concurrent rename back to old path"
    );
    assert_eq!(
        after.get(Some(1), Some(2)),
        Some(&QualityFact {
            score: 120,
            path: Some(e02_upgraded.to_string_lossy().into_owned()),
        })
    );
    let _ = fx.e02_old;
}
