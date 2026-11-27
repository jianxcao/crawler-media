use domain::{Coverage, FetchMode, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId};
use store::Store;
use store::download_operations::{RecordSubscribeDeferInput, SubscribeDeferState};
use tempfile::TempDir;
use uuid::Uuid;

fn seed_subscribe(store: &Store) -> (SubscribeId, String, i64) {
    let sub_id = SubscribeId::from_uuid(Uuid::new_v4());
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "R9 Defer Fixture".into(),
        year: None,
        original_title: None,
        aliases: vec![],
        tmdb_id: None,
        douban_id: None,
        bangumi_id: None,
        anilist_id: None,
        tvdb_id: None,
    };
    store.insert_media(&media).expect("insert media");
    let subscribe = Subscribe {
        id: sub_id,
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: store.list_filters().expect("filters")[0].id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    };
    store
        .insert_subscribe(&subscribe)
        .expect("insert subscribe");
    let binding = store
        .subscribe_policy_binding(sub_id)
        .expect("get binding")
        .expect("binding exists");
    (
        sub_id,
        binding.jobs_sync_generation,
        binding.policy_revision,
    )
}

#[test]
fn record_subscribe_defer_updates_plugin_hook_and_info_hash_across_retry_cap_and_reactivation() {
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(dir.path()).expect("open store");
    let (sub_id, generation, policy_rev) = seed_subscribe(&store);

    // 1. Initial defer by plugin A at `subscribe.select` without `info_hash`
    let first = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r9-1".into(),
            subscribe_id: sub_id,
            subscribe_generation: generation.clone(),
            policy_revision: policy_rev,
            enclosure: "https://pt.example/dl/100".into(),
            info_hash: None,
            plugin_id: Some("org.example.plugin-a".into()),
            hook_id: "subscribe.select".into(),
            reason: "score hold".into(),
            next_retry_at: 1_060,
            now_utc: 1_000,
        })
        .expect("first defer");
    assert_eq!(first.state, SubscribeDeferState::Deferred);
    assert_eq!(first.plugin_id.as_deref(), Some("org.example.plugin-a"));
    assert_eq!(first.hook_id, "subscribe.select");
    assert_eq!(first.info_hash, None);

    // 2. Retry defer under same binding by plugin B at `download.before` with `info_hash`
    let second = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r9-ignored".into(),
            subscribe_id: sub_id,
            subscribe_generation: generation.clone(),
            policy_revision: policy_rev,
            enclosure: "https://pt.example/dl/100".into(),
            info_hash: Some("abcd1234abcd1234abcd1234abcd1234abcd1234".into()),
            plugin_id: Some("org.example.plugin-b".into()),
            hook_id: "download.before".into(),
            reason: "quota wait".into(),
            next_retry_at: 1_120,
            now_utc: 1_065,
        })
        .expect("second defer");
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.plugin_id.as_deref(), Some("org.example.plugin-b"));
    assert_eq!(second.hook_id, "download.before");
    assert_eq!(
        second.info_hash.as_deref(),
        Some("abcd1234abcd1234abcd1234abcd1234abcd1234")
    );

    // 3. Deadline cap rejection by external hook (`plugin_id = None`) preserves known `info_hash` and updates `plugin_id`/`hook_id`
    let rejected = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r9-ignored-2".into(),
            subscribe_id: sub_id,
            subscribe_generation: generation.clone(),
            policy_revision: policy_rev,
            enclosure: "https://pt.example/dl/100".into(),
            info_hash: None,
            plugin_id: None,
            hook_id: "subscribe.select".into(),
            reason: "external hold expired".into(),
            next_retry_at: 500_000,
            now_utc: first.max_deadline_at,
        })
        .expect("deadline rejection");
    assert_eq!(rejected.state, SubscribeDeferState::Rejected);
    assert_eq!(rejected.plugin_id, None);
    assert_eq!(rejected.hook_id, "subscribe.select");
    assert_eq!(
        rejected.info_hash.as_deref(),
        Some("abcd1234abcd1234abcd1234abcd1234abcd1234")
    );

    // 4. Clear rejected -> reactivate with fresh plugin C & new `info_hash`
    let cleared = store
        .clear_rejected_subscribe_defer(sub_id, &rejected.enclosure, first.max_deadline_at + 10)
        .expect("clear rejected");
    assert!(cleared);
    let reactivated = store
        .record_subscribe_defer(&RecordSubscribeDeferInput {
            defer_id: "def-r9-ignored-3".into(),
            subscribe_id: sub_id,
            subscribe_generation: generation,
            policy_revision: policy_rev,
            enclosure: "https://pt.example/dl/100".into(),
            info_hash: Some("9999000099990000999900009999000099990000".into()),
            plugin_id: Some("org.example.plugin-c".into()),
            hook_id: "download.before".into(),
            reason: "fresh defer".into(),
            next_retry_at: first.max_deadline_at + 100,
            now_utc: first.max_deadline_at + 20,
        })
        .expect("reactivate");
    assert_eq!(reactivated.state, SubscribeDeferState::Deferred);
    assert_eq!(reactivated.attempt_count, 1);
    assert_eq!(
        reactivated.plugin_id.as_deref(),
        Some("org.example.plugin-c")
    );
    assert_eq!(reactivated.hook_id, "download.before");
    assert_eq!(
        reactivated.info_hash.as_deref(),
        Some("9999000099990000999900009999000099990000")
    );
}
