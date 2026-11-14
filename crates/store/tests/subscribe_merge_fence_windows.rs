//! Failure after an abort window must retain durable exclusion of late creators.
use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId,
};
use store::Store;

fn media(store: &Store) -> MediaId {
    let id = MediaId::new();
    store
        .insert_media(&Media {
            id,
            kind: MediaKind::Movie,
            title: "fence-window".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    id
}
fn subscribe(media_id: MediaId) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    }
}
#[test]
fn failed_later_abort_window_keeps_merge_fence_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let survivor = media(&store);
    let loser = media(&store);
    let unrelated = media(&store);
    let mut creators = Vec::new();
    for _ in 0..65 {
        let sub = subscribe(loser);
        let token = store.begin_subscribe_creation(&sub).unwrap();
        store.retain_subscribe_creation(&token).unwrap();
        creators.push((sub, token));
    }
    let unaffected = subscribe(unrelated);
    let unaffected_token = store.begin_subscribe_creation(&unaffected).unwrap();
    store.retain_subscribe_creation(&unaffected_token).unwrap();
    let last = &creators.last().unwrap().0;
    let db = rusqlite::Connection::open(dir.path().join("subscribe.db")).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER reject_later_abort BEFORE UPDATE OF state ON subscribe_reference_ops \
         WHEN OLD.subscribe_id='{}' AND NEW.state='aborting' \
         BEGIN SELECT RAISE(ABORT,'later abort window fault'); END;",
        last.id
    ))
    .unwrap();
    let op = store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: SubscribeId::new().to_string(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1,
        )
        .unwrap();
    assert!(
        measured(&store, "failed-drain65", || store
            .advance_media_merge(&op, 2))
        .is_err()
    );
    let peer = Store::open(dir.path()).unwrap();
    let (last_sub, last_token) = creators.last().unwrap();
    assert!(
        peer.commit_subscribe_creation(last_token, last_sub, false)
            .is_err(),
        "durable merge fence must reject the later retained token even when abort draining failed"
    );
    peer.commit_subscribe_creation(&unaffected_token, &unaffected, false)
        .unwrap();
    assert!(peer.get_subscribe(unaffected.id).unwrap().is_some());
    assert!(peer.unfinished_media_merges().unwrap().contains(&op));
    db.execute_batch("DROP TRIGGER reject_later_abort").unwrap();
    assert_eq!(
        measured(&peer, "retry65", || peer.advance_media_merge(&op, 3))
            .unwrap()
            .state,
        "succeeded"
    );
    for (sub, token) in &creators {
        assert!(peer.commit_subscribe_creation(token, sub, false).is_err());
        assert!(peer.get_subscribe(sub.id).unwrap().is_none());
    }
    assert!(
        Store::open(dir.path())
            .unwrap()
            .get_subscribe(unaffected.id)
            .unwrap()
            .is_some()
    );
}

fn measured<T>(store: &Store, label: &str, work: impl FnOnce() -> T) -> T {
    let samples = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let observed = samples.clone();
    let result = store.with_writer_diagnostics(
        move |s| {
            if s.database == store::Db::Subscribe {
                observed.borrow_mut().push((
                    s.writer_wait_ns,
                    s.writer_hold_ns,
                    s.covered_owned_write_tx_ns,
                ));
            }
        },
        work,
    );
    let samples = samples.borrow();
    assert!(!samples.is_empty());
    for (field, name) in ["writer_wait", "writer_hold", "covered_owned_write_tx"]
        .into_iter()
        .enumerate()
    {
        let mut values: Vec<_> = samples
            .iter()
            .map(|s| match field {
                0 => s.0,
                1 => s.1,
                _ => s.2,
            })
            .collect();
        values.sort_unstable();
        eprintln!(
            "M1 {label} {name} n={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
            values.len(),
            values[(values.len() - 1) / 2] as f64 / 1e6,
            values[(values.len() - 1) * 95 / 100] as f64 / 1e6,
            values[values.len() - 1] as f64 / 1e6
        );
    }
    result
}

#[test]
#[ignore = "manual writer timing of many independent retained creators"]
fn measure_merge_fence_drain_with_1025_retained_creators() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let survivor = media(&store);
    let loser = media(&store);
    let mut last = None;
    for _ in 0..1025 {
        let sub = subscribe(loser);
        let token = store.begin_subscribe_creation(&sub).unwrap();
        store.retain_subscribe_creation(&token).unwrap();
        last = Some((sub, token));
    }
    let op = store
        .begin_media_merge(
            &store::playback::MediaMergeRequest {
                request_id: SubscribeId::new().to_string(),
                survivor,
                loser,
                survivor_revision: 0,
                loser_revision: 0,
                refresh_targets: Default::default(),
            },
            1,
        )
        .unwrap();
    assert_eq!(
        measured(&store, "drain1025", || store.advance_media_merge(&op, 2))
            .unwrap()
            .state,
        "succeeded"
    );
    let (sub, token) = last.unwrap();
    assert!(
        Store::open(dir.path())
            .unwrap()
            .commit_subscribe_creation(&token, &sub, false)
            .is_err()
    );
}
