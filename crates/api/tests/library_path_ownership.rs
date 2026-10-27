//! Library ownership boundaries that survive a root rewrite or a Library
//! deletion. A published file's owner is its recorded `library_files.library_id`;
//! these tests pin the guards that keep that record from being stranded.

use api::Store;
use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource};
use store::test_support::{FixtureFile, publish_fixture_file};

#[test]
fn root_path_is_globally_unique_across_libraries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let shared = tmp.path().join("media");
    let shared_path = shared.to_str().unwrap();
    let owner = store
        .create_library(
            MediaKind::Tv,
            "owner",
            &[shared_path],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    // `library_roots.path_key TEXT NOT NULL UNIQUE`: a root belongs to exactly
    // one Library, so a second Library cannot claim the same path and no file
    // can have two candidate owners.
    let duplicate = store.create_library(
        MediaKind::Tv,
        "duplicate",
        &[shared_path],
        "everyone",
        true,
        &[],
    );
    assert!(
        matches!(duplicate, Err(api::store::StoreError::Sqlite(_))),
        "two Libraries must not share a root path"
    );
    assert_eq!(
        store
            .library_for_path_strict(&shared.join("episode.mkv"), MediaKind::Tv)
            .unwrap()
            .map(|library| library.id),
        Some(owner.id),
        "a new file is still classified by root path at admission"
    );
}

#[test]
fn restricted_root_cannot_move_while_owned_ledger_rows_remain() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let old_root = tmp.path().join("private-old");
    let new_root = tmp.path().join("private-new");
    let restricted = store
        .create_library(
            MediaKind::Movie,
            "restricted",
            &[old_root.to_str().unwrap()],
            "selected",
            true,
            &[],
        )
        .unwrap();
    let path = old_root.join("secret.mkv");
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "secret".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let published = publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(published.library_id.to_string(), restricted.id);

    let update = store.update_library(
        &restricted.id,
        None,
        Some(&[new_root.to_str().unwrap()]),
        None,
    );
    assert!(matches!(update, Err(api::store::StoreError::Protected(_))));

    let recorded = store.file_coverage_by_id(published.file_id).unwrap().unwrap();
    assert_eq!(
        recorded.file.library_id.to_string(),
        restricted.id,
        "the failed root update must preserve the row's restricted owner"
    );
}

#[tokio::test]
async fn deleting_library_with_owned_ledger_is_blocked_with_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let restricted_root = tmp.path().join("restricted");
    std::fs::create_dir_all(&restricted_root).unwrap();

    let restricted = store
        .create_library(
            MediaKind::Movie,
            "Restricted",
            &[restricted_root.to_str().unwrap()],
            "selected",
            false,
            &[],
        )
        .unwrap();

    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Restricted".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let media_id = media.id;
    let path = restricted_root.join("secret.mkv");
    publish_fixture_file(
        &store,
        &FixtureFile {
            file_id: Some(domain::LedgerId::new()),
            media_id,
            path: path.to_str().unwrap().into(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("hevc".into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();

    // 尝试删除仍有台账文件的媒体库：必须返回 409 Conflict 或 Protected Error
    let err = store.delete_library(&restricted.id);
    assert!(
        matches!(err, Err(api::store::StoreError::Protected(_))),
        "仍有台账文件的媒体库必须禁止直接删除，防止文件失去受限保护回退到所有人可见的默认库"
    );
}
