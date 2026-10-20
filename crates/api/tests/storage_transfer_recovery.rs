#[path = "support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId,
    LibraryFile, LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_write::WriteScope;
use support::library_presence::PresenceFixture;

#[tokio::test]
async fn recovered_pending_a_does_not_block_b() {
    let fix = PresenceFixture::new();
    let store_arc = fix.state.store();
    let (tv_lib, root_id) = {
        let store = store_arc.lock();
        let lib = store
            .create_library(
                MediaKind::Tv,
                "TV Shows",
                &["/tv_shows"],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|r| r.library_id.as_deref() == Some(&lib.id))
            .map(|r| r.id)
            .unwrap();
        (lib, root)
    };

    let lib_id: LibraryId = tv_lib.id.parse().unwrap();
    let scope = WriteScope {
        library_id: lib_id,
        config_revision: 0,
    };

    let media_id = MediaId::new();
    let media = Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Test Show".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };

    {
        let store = store_arc.lock();
        store.insert_media(&media).unwrap();
    }

    let reserved_a_id = LedgerId::new();
    let candidate_a = FileCoverage {
        file: LibraryFile {
            file_id: reserved_a_id,
            library_id: lib_id,
            root_id: root_id.clone(),
            media_id,
            kind: MediaKind::Tv,
            path: PathBuf::from("Test Show/S01E01-E02.mkv"),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Pending,
            size_bytes: Some(1024),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: Some(PathBuf::from("/downloads/source_a.mkv")),
            transfer_mode: Some("hardlink".into()),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::new(1, 1), UnitKey::new(1, 2)],
    };

    // Step 1: reserve A in pending state
    {
        let store = store_arc.lock();
        let _ = store
            .reserve_transfer(
                &scope,
                &candidate_a,
                Path::new("/downloads/source_a.mkv"),
                library::TransferMode::Hardlink,
                1_000,
            )
            .expect("reservation A succeeds");
    }

    // Step 2: simulate restart and recover pending A
    let recovered_a = {
        let store = store_arc.lock();
        store
            .reserve_transfer(
                &scope,
                &candidate_a,
                Path::new("/downloads/source_a.mkv"),
                library::TransferMode::Hardlink,
                2_000,
            )
            .expect("recovering A succeeds")
    };

    assert_eq!(recovered_a.file.file_id, reserved_a_id);
    {
        let store = store_arc.lock();
        assert_eq!(
            store.get_file_units(recovered_a.file.file_id).unwrap(),
            vec![UnitKey::new(1, 1), UnitKey::new(1, 2)]
        );
    }

    // Step 3: publish B, which must not be blocked or rejected
    let published_b_id = LedgerId::new();
    let candidate_b = FileCoverage {
        file: LibraryFile {
            file_id: published_b_id,
            library_id: lib_id,
            root_id,
            media_id,
            kind: MediaKind::Tv,
            path: PathBuf::from("Test Show/S01E03.mkv"),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Pending,
            size_bytes: Some(2048),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: Some(PathBuf::from("/downloads/source_b.mkv")),
            transfer_mode: Some("hardlink".into()),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            release_quality: None,
            duration_ms: None,
            tracks_version: 0,
            poster_path: None,
            backdrop_path: None,
            still_path: None,
        },
        units: vec![UnitKey::new(1, 3)],
    };

    {
        let store = store_arc.lock();
        let reserved_b = store
            .reserve_transfer(
                &scope,
                &candidate_b,
                Path::new("/downloads/source_b.mkv"),
                library::TransferMode::Hardlink,
                3_000,
            )
            .expect("reservation B succeeds");

        store
            .finish_transfer(&scope, &reserved_b, &reserved_b.file.version, 4_000)
            .expect("finish transfer B succeeds");

        assert!(store.get_file_version(published_b_id).unwrap().is_some());
    }
}
