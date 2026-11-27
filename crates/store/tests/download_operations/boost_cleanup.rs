use domain::{DownloaderId, SiteId};
use store::download_operations::*;
use store::plugins::PluginError;
use store::{Db, Store, StoreError};

use super::accept_input;

fn seed_boost_task_for_cleanup(
    store: &Store,
    op_id: &str,
    tf_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    hash: &str,
    bytes: i64,
) -> BoostTaskFactRecord {
    store
        .accept_download_operation(&accept_input(
            op_id,
            &format!("ik-{op_id}"),
            None,
            Some(dl_id),
        ))
        .unwrap();
    store
        .record_download_ownership(&RecordDownloadOwnershipInput {
            downloader_id: dl_id,
            ownership_tag: format!("cm-op:{op_id}"),
            info_hash: Some(hash.into()),
            operation_id: op_id.into(),
            subscribe_id: None,
            plugin_id: Some("builtin.boost".into()),
            subject: "subject:1".into(),
            site_id: Some(site_id),
            enclosure: format!("https://pt.example/{op_id}.torrent"),
            ownership_state: DownloadOwnershipState::Owned,
            provenance_kind: DownloadProvenanceKind::CreatedFresh,
            verified_bytes: Some(bytes),
            bytes_digest: Some(format!("dig-{op_id}")),
            now_utc: 100,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: tf_id.into(),
            plugin_id: "builtin.boost".into(),
            quota_key: "pool:main".into(),
            downloader_id: dl_id,
            ownership_tag: format!("cm-op:{op_id}"),
            info_hash: hash.into(),
            site_id: Some(site_id),
            state: BoostTaskState::Seeding,
            occupied_bytes: bytes,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 100,
            expected_revision: None,
            now_utc: 100,
        })
        .unwrap()
}

#[test]
fn expired_cleanup_plan_resets_and_reprepares_preserving_retained_bytes_caveat_1() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    let tf =
        seed_boost_task_for_cleanup(&store, "op-c1", "tf-c1", dl_id, site_id, "aaaa1111", 4096);

    let retained = store
        .mark_boost_task_retained_with_paths(
            "tf-c1",
            tf.revision,
            4096,
            Some("{\"paths\":[\"/host/dl/c1.mkv\"]}"),
            200,
        )
        .unwrap();
    assert_eq!(retained.state, BoostTaskState::RetainedOnly);
    assert_eq!(retained.retained_bytes, 4096);
    assert_eq!(
        retained.cleanup_plan_json.as_deref(),
        Some("{\"paths\":[\"/host/dl/c1.mkv\"]}")
    );

    let planned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-c1".into(),
            expected_revision: retained.revision,
            cleanup_plan_handle: "plan-1".into(),
            cleanup_plan_json: "{\"paths\":[\"/host/dl/c1.mkv\"]}".into(),
            ttl_secs: 60,
            now_utc: 300,
        })
        .unwrap();
    assert_eq!(planned.state, BoostTaskState::CleanupPlanned);

    assert!(matches!(
        store.prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-c1".into(),
            expected_revision: planned.revision,
            cleanup_plan_handle: "plan-dup".into(),
            cleanup_plan_json: "{\"paths\":[\"/host/dl/c1.mkv\"]}".into(),
            ttl_secs: 60,
            now_utc: 330,
        }),
        Err(StoreError::Plugin(
            PluginError::Conflict | PluginError::Denied
        ))
    ));
    assert!(matches!(
        store.complete_boost_cleanup("plan-1", planned.revision, 361),
        Err(StoreError::Plugin(PluginError::Conflict))
    ));

    let replanned = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-c1".into(),
            expected_revision: planned.revision,
            cleanup_plan_handle: "plan-2".into(),
            cleanup_plan_json: "{\"paths\":[\"/host/dl/c1.mkv\"]}".into(),
            ttl_secs: 120,
            now_utc: 361,
        })
        .unwrap();
    assert_eq!(replanned.state, BoostTaskState::CleanupPlanned);
    assert_eq!(replanned.retained_bytes, 4096);
    assert_eq!(replanned.cleanup_plan_handle.as_deref(), Some("plan-2"));
    assert!(replanned.revision > planned.revision);

    let reset = store
        .reset_expired_boost_cleanup_plan("tf-c1", 500)
        .unwrap();
    assert_eq!(reset.state, BoostTaskState::RetainedOnly);
    assert_eq!(reset.retained_bytes, 4096);
    assert!(reset.cleanup_plan_handle.is_none());
}

#[test]
fn plugin_scoped_boost_snapshot_filters_by_plugin_site_and_downloader_off_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl1 = DownloaderId::new();
    let dl2 = DownloaderId::new();
    let s1 = SiteId::new();
    let s2 = SiteId::new();

    for (idx, (pid, qk, dl, sid, bytes)) in [
        ("builtin.boost", "qk-1", dl1, s1, 1000i64),
        ("builtin.boost", "qk-2", dl2, s2, 2000i64),
        ("other.plugin", "qk-3", dl1, s1, 9999i64),
    ]
    .into_iter()
    .enumerate()
    {
        let op_id = format!("op-sn-{idx}");
        store
            .accept_download_operation(&accept_input(
                &op_id,
                &format!("ik-sn-{idx}"),
                None,
                Some(dl),
            ))
            .unwrap();
        store
            .record_download_ownership(&RecordDownloadOwnershipInput {
                downloader_id: dl,
                ownership_tag: format!("cm-op:{op_id}"),
                info_hash: Some(format!("hash-sn-{idx}")),
                operation_id: op_id.clone(),
                subscribe_id: None,
                plugin_id: Some(pid.into()),
                subject: "sub".into(),
                site_id: Some(sid),
                enclosure: format!("https://pt.example/{op_id}.torrent"),
                ownership_state: DownloadOwnershipState::Owned,
                provenance_kind: DownloadProvenanceKind::CreatedFresh,
                verified_bytes: Some(bytes),
                bytes_digest: Some("dig".into()),
                now_utc: 100,
            })
            .unwrap();
        store
            .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
                task_fact_id: format!("tf-sn-{idx}"),
                plugin_id: pid.into(),
                quota_key: qk.into(),
                downloader_id: dl,
                ownership_tag: format!("cm-op:{op_id}"),
                info_hash: format!("hash-sn-{idx}"),
                site_id: Some(sid),
                state: BoostTaskState::Seeding,
                occupied_bytes: bytes,
                retained_bytes: 0,
                hr_active: Some(false),
                hr_until: None,
                hold_until: None,
                free_until: None,
                protection_state: BoostProtectionState::Eligible,
                protection_reason: None,
                observed_at: 100,
                expected_revision: None,
                now_utc: 100,
            })
            .unwrap();
    }

    let snap_all = store
        .get_boost_snapshot_for_plugin("builtin.boost", &[], &[])
        .unwrap();
    assert_eq!(snap_all.used_bytes, 3000);
    assert_eq!(snap_all.tasks.len(), 2);

    let snap_s1 = store
        .get_boost_snapshot_for_plugin("builtin.boost", &[s1], &[dl1])
        .unwrap();
    assert_eq!(snap_s1.used_bytes, 1000);
    assert_eq!(snap_s1.tasks.len(), 1);
    assert_eq!(snap_s1.tasks[0].task_fact_id, "tf-sn-0");
    let _ = Db::Subscribe;
}

#[test]
fn reserve_boost_provisional_rejects_duplicate_active_hash_with_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    store
        .accept_download_operation(&accept_input("op-r1", "ik-r1", None, Some(dl_id)))
        .unwrap();
    store
        .accept_download_operation(&accept_input("op-r2", "ik-r2", None, Some(dl_id)))
        .unwrap();

    store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-r1".into(),
            operation_id: "op-r1".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: Some(site_id),
            downloader_id: dl_id,
            info_hash: Some("AABBCCDD".into()),
            estimated_bytes: 1024,
            expires_at: Some(2000),
            now_utc: 1000,
        })
        .unwrap();

    let err = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-r2".into(),
            operation_id: "op-r2".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: Some(site_id),
            downloader_id: dl_id,
            info_hash: Some("aabbccdd".into()),
            estimated_bytes: 1024,
            expires_at: Some(2000),
            now_utc: 1001,
        })
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Plugin(PluginError::Conflict)),
        "expected Conflict for duplicate active hash, got {err:?}"
    );
}

#[test]
fn boost_task_fact_reconcile_handles_active_duplicate_without_unique_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    let initial =
        seed_boost_task_for_cleanup(&store, "op-d1", "tf-d1", dl_id, site_id, "deadbeef", 4096);

    store
        .accept_download_operation(&accept_input("op-d2", "ik-d2", None, Some(dl_id)))
        .unwrap();
    store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: "op-d2".into(),
            expected_revision: 1,
            next_state: DownloadOperationState::Submitting,
            downloader_id: Some(dl_id),
            endpoint_fingerprint: None,
            info_hash: Some("deadbeef".into()),
            bytes_digest: Some("dig-d2".into()),
            verified_bytes: Some(4096),
            outcome_code: None,
            error_message: None,
            submitted_at: None,
            now_utc: 105,
        })
        .unwrap();
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: "tf-d2".into(),
            plugin_id: "builtin.boost".into(),
            quota_key: "pool:main".into(),
            downloader_id: dl_id,
            ownership_tag: "cm-op:op-d1".into(),
            info_hash: "DEADBEEF".into(),
            site_id: Some(site_id),
            state: BoostTaskState::Downloading,
            occupied_bytes: 4096,
            retained_bytes: 0,
            hr_active: Some(false),
            hr_until: None,
            hold_until: None,
            free_until: None,
            protection_state: BoostProtectionState::Eligible,
            protection_reason: None,
            observed_at: 110,
            expected_revision: None,
            now_utc: 110,
        })
        .unwrap();
    store
        .reconcile_download_operation(&ReconcileDownloadOperationInput {
            operation_id: "op-d2".into(),
            expected_revision: 2,
            resolved_state: DownloadOperationState::Submitted,
            info_hash: Some("deadbeef".into()),
            outcome_code: Some("existing_owned".into()),
            error_message: None,
            ownership: None,
            boost_task_fact: None,
            now_utc: 110,
        })
        .unwrap();
    let active = store.get_boost_task_fact("tf-d1").unwrap().unwrap();
    assert_eq!(active.observed_at, 110);
    assert!(active.revision > initial.revision);
    assert_eq!(
        store
            .get_boost_snapshot_facts("pool:main", &[])
            .unwrap()
            .reserved_bytes,
        0
    );
}

#[test]
fn boost_task_fact_reactivates_cleaned_row_with_same_info_hash() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    let initial =
        seed_boost_task_for_cleanup(&store, "op-d1", "tf-d1", dl_id, site_id, "deadbeef", 4096);

    let ret = store
        .mark_boost_task_retained_with_paths(
            "tf-d1",
            initial.revision,
            4096,
            Some("{\"paths\":[\"/host/dl/d1.mkv\"]}"),
            120,
        )
        .unwrap();
    let plan = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-d1".into(),
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-d1".into(),
            cleanup_plan_json: "{\"paths\":[\"/host/dl/d1.mkv\"]}".into(),
            ttl_secs: 60,
            now_utc: 125,
        })
        .unwrap();
    store
        .complete_boost_cleanup("plan-d1", plan.revision, 130)
        .unwrap();

    let reactivated =
        seed_boost_task_for_cleanup(&store, "op-d3", "tf-d3", dl_id, site_id, "deadbeef", 4096);
    assert_eq!(reactivated.task_fact_id, "tf-d3");
    assert_eq!(reactivated.state, BoostTaskState::Seeding);
    assert_eq!(reactivated.occupied_bytes, 4096);
    assert_eq!(reactivated.retained_bytes, 0);
    assert!(reactivated.cleanup_plan_handle.is_none());
}

fn seed_fact_in_state(
    store: &Store,
    op_id: &str,
    tf_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    hash: &str,
    state: BoostTaskState,
) -> BoostTaskFactRecord {
    let tf = seed_boost_task_for_cleanup(store, op_id, tf_id, dl_id, site_id, hash, 4096);
    store
        .upsert_boost_task_fact(&UpsertBoostTaskFactInput {
            task_fact_id: tf.task_fact_id,
            plugin_id: tf.plugin_id,
            quota_key: tf.quota_key,
            downloader_id: tf.downloader_id,
            ownership_tag: tf.ownership_tag,
            info_hash: tf.info_hash,
            site_id: tf.site_id,
            state,
            occupied_bytes: tf.occupied_bytes,
            retained_bytes: tf.retained_bytes,
            hr_active: tf.hr_active,
            hr_until: tf.hr_until,
            hold_until: tf.hold_until,
            free_until: tf.free_until,
            protection_state: tf.protection_state,
            protection_reason: tf.protection_reason,
            observed_at: 101,
            expected_revision: Some(tf.revision),
            now_utc: 101,
        })
        .unwrap()
}

fn assert_provisional_reserve_ok_and_release(
    store: &Store,
    op_id: &str,
    dl_id: DownloaderId,
    site_id: SiteId,
    hash: &str,
    now: i64,
) {
    store
        .accept_download_operation(&accept_input(
            op_id,
            &format!("ik-{op_id}"),
            None,
            Some(dl_id),
        ))
        .unwrap();
    let rec = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: format!("res-{op_id}"),
            operation_id: op_id.into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: Some(site_id),
            downloader_id: dl_id,
            info_hash: Some(hash.into()),
            estimated_bytes: 4096,
            expires_at: Some(2000),
            now_utc: now,
        })
        .unwrap();
    store
        .release_boost_reservation(op_id, rec.revision, now + 1)
        .unwrap();
}

#[test]
fn reserve_and_commit_reject_active_boost_task_fact_hash() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let dl_other = DownloaderId::new();
    let site_id = SiteId::new();

    for (idx, (st, h)) in [
        (BoostTaskState::Downloading, "hash-dl"),
        (BoostTaskState::Seeding, "hash-seed"),
        (BoostTaskState::Paused, "hash-pause"),
    ]
    .into_iter()
    .enumerate()
    {
        seed_fact_in_state(
            &store,
            &format!("op-a-{idx}"),
            &format!("tf-a-{idx}"),
            dl_id,
            site_id,
            h,
            st,
        );
        store
            .accept_download_operation(&accept_input(
                &format!("op-dup-{idx}"),
                &format!("ik-dup-{idx}"),
                None,
                Some(dl_id),
            ))
            .unwrap();
        let err = store
            .reserve_boost_provisional(&ReserveBoostProvisionalInput {
                reservation_id: format!("res-dup-{idx}"),
                operation_id: format!("op-dup-{idx}"),
                quota_key: "pool:main".into(),
                plugin_id: "builtin.boost".into(),
                site_id: Some(site_id),
                downloader_id: dl_id,
                info_hash: Some(h.to_ascii_uppercase()),
                estimated_bytes: 4096,
                expires_at: Some(2000),
                now_utc: 200,
            })
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Plugin(PluginError::Conflict)),
            "state {st:?} must reject duplicate reserve with Conflict, got {err:?}"
        );
    }

    // Provisional with None followed by commit with active hash must fail Conflict (before quota check).
    store
        .accept_download_operation(&accept_input("op-c1", "ik-c1", None, Some(dl_id)))
        .unwrap();
    let prov = store
        .reserve_boost_provisional(&ReserveBoostProvisionalInput {
            reservation_id: "res-c1".into(),
            operation_id: "op-c1".into(),
            quota_key: "pool:main".into(),
            plugin_id: "builtin.boost".into(),
            site_id: Some(site_id),
            downloader_id: dl_id,
            info_hash: None,
            estimated_bytes: 0,
            expires_at: Some(2000),
            now_utc: 205,
        })
        .unwrap();
    let commit_err = store
        .commit_boost_final_reservation(&CommitBoostFinalInput {
            operation_id: "op-c1".into(),
            expected_reservation_revision: prov.revision,
            info_hash: "HASH-SEED".into(),
            bytes_digest: "dig-c1".into(),
            verified_bytes: 999_999,
            budget_bytes: 1,
            max_add_per_hour: Some(1),
            mark_operation_submitting: true,
            now_utc: 206,
        })
        .unwrap_err();
    assert!(
        matches!(commit_err, StoreError::Plugin(PluginError::Conflict)),
        "expected Conflict before QuotaExceeded, got {commit_err:?}"
    );
    assert_eq!(store.count_boost_adds_since("builtin.boost", 0).unwrap(), 0);

    // Distinct downloader_id or quota_key must succeed.
    for (idx, (dl, qk)) in [(dl_other, "pool:main"), (dl_id, "pool:other")]
        .into_iter()
        .enumerate()
    {
        let op_id = format!("op-iso-{idx}");
        store
            .accept_download_operation(&accept_input(
                &op_id,
                &format!("ik-iso-{idx}"),
                None,
                Some(dl),
            ))
            .unwrap();
        store
            .reserve_boost_provisional(&ReserveBoostProvisionalInput {
                reservation_id: format!("res-iso-{idx}"),
                operation_id: op_id,
                quota_key: qk.into(),
                plugin_id: "builtin.boost".into(),
                site_id: Some(site_id),
                downloader_id: dl,
                info_hash: Some("HASH-SEED".into()),
                estimated_bytes: 4096,
                expires_at: Some(2000),
                now_utc: 210,
            })
            .unwrap();
    }
}

#[test]
fn reserve_allows_non_active_boost_task_fact_reactivation() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    let tf = seed_boost_task_for_cleanup(&store, "op-r", "tf-r", dl_id, site_id, "hash-seed", 4096);

    let ret = store
        .mark_boost_task_retained_with_paths("tf-r", tf.revision, 4096, None, 220)
        .unwrap();
    assert_provisional_reserve_ok_and_release(&store, "op-re-1", dl_id, site_id, "HASH-SEED", 221);

    let plan = store
        .prepare_boost_cleanup_plan(&PrepareBoostCleanupPlanInput {
            task_fact_id: "tf-r".into(),
            expected_revision: ret.revision,
            cleanup_plan_handle: "plan-re".into(),
            cleanup_plan_json: "{\"paths\":[]}".into(),
            ttl_secs: 600,
            now_utc: 225,
        })
        .unwrap();
    assert_provisional_reserve_ok_and_release(&store, "op-re-2", dl_id, site_id, "HASH-SEED", 226);

    store
        .complete_boost_cleanup("plan-re", plan.revision, 230)
        .unwrap();
    assert_provisional_reserve_ok_and_release(&store, "op-re-3", dl_id, site_id, "HASH-SEED", 231);
}

#[test]
fn load_boost_protection_sets_includes_symlink_display_paths_and_rename_path_keys() {
    use domain::{Media, MediaId, MediaKind};
    use store::test_support::{FixtureFile, publish_fixture_file};

    let dir = tempfile::tempdir().unwrap();
    let real_root = dir.path().join("real_pool");
    let link_root = dir.path().join("symlink_pool");
    std::fs::create_dir_all(&real_root).unwrap();
    std::os::unix::fs::symlink(&real_root, &link_root).unwrap();

    let store = Store::open(dir.path().join("data")).unwrap();
    store
        .create_library(
            MediaKind::Movie,
            "SymlinkMovies",
            &[link_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let (m1, m2) = (MediaId::new(), MediaId::new());
    for (id, title) in [(m1, "One"), (m2, "Two")] {
        store
            .insert_media(&Media {
                id,
                kind: MediaKind::Movie,
                title: title.into(),
                year: Some(2024),
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
    }

    let f1_display = link_root.join("One.2024.mkv");
    let f2_display = link_root.join("Two.2024.mkv");
    std::fs::write(&f1_display, b"one").unwrap();
    std::fs::write(&f2_display, b"two").unwrap();
    let raw_src = format!("{}//staging/one_src.mkv", link_root.display());

    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: m1,
            path: f1_display.display().to_string(),
            source_path: Some(raw_src.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let f2 = publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: m2,
            path: f2_display.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();

    let rename_to = link_root.join("Two.2024.Renamed.mkv");
    let req = store
        .begin_file_rename(f2.file_id, &f2.version, &f2.path, &rename_to, 1_000)
        .unwrap();

    let real_canon = std::fs::canonicalize(&real_root).unwrap();
    let f1_path_key = real_canon.join("One.2024.mkv").display().to_string();
    let src_canon = real_canon.join("staging/one_src.mkv").display().to_string();
    let from_path_key = real_canon.join("Two.2024.mkv").display().to_string();
    let to_path_key = real_canon
        .join("Two.2024.Renamed.mkv")
        .display()
        .to_string();
    assert_ne!(f1_display.display().to_string(), f1_path_key);
    assert_ne!(req.from_path.display().to_string(), from_path_key);
    assert_ne!(req.to_path.display().to_string(), to_path_key);

    let sets = store.load_boost_protection_sets().unwrap();
    assert!(sets.library_paths.contains(&f1_path_key));
    assert!(
        sets.library_paths
            .contains(&f1_display.display().to_string())
    );
    assert!(sets.library_paths.contains(&src_canon));

    for expected in [
        req.from_path.display().to_string(),
        from_path_key,
        req.to_path.display().to_string(),
        to_path_key,
    ] {
        assert!(
            sets.transfer_paths.contains(&expected),
            "transfer_paths missing {expected}"
        );
        assert!(
            sets.library_paths.contains(&expected),
            "library_paths missing {expected}"
        );
    }
}

#[test]
fn boost_task_fact_lookup_propagates_decode_errors_instead_of_swallowing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let dl_id = DownloaderId::new();
    let site_id = SiteId::new();
    seed_boost_task_for_cleanup(&store, "op-err", "tf-err", dl_id, site_id, "abcd1234", 1024);

    store
        .with_writer(Db::Subscribe, |conn| {
            conn.execute(
                "UPDATE boost_task_facts SET site_id = 'invalid-site-uuid' WHERE task_fact_id = 'tf-err'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    assert!(
        store
            .get_boost_task_fact_by_hash("builtin.boost", dl_id, "abcd1234")
            .is_err()
    );
    assert!(
        store
            .find_boost_task_fact_by_handle("builtin.boost", "tf-err")
            .is_err()
    );
}
