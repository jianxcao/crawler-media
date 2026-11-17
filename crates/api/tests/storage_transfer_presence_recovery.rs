#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;

use api::probe_manager::{ProbeManager, TracksProbe};
use axum::http::StatusCode;
use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::library_write::WriteScope;
use support::library_presence::PresenceFixture;

fn reserve(fix: &PresenceFixture, name: &str) -> FileCoverage {
    let store = fix.state.store();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "Interrupted Transfer".into(),
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
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == fix.library_id)
        .unwrap();
    let candidate = FileCoverage {
        path_key: String::new(),
        file: LibraryFile {
            file_id: LedgerId::new(),
            library_id: fix.library_id.parse().unwrap(),
            root_id: root.id,
            media_id: media.id,
            kind: MediaKind::Movie,
            path: fix.root.join(name),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Pending,
            size_bytes: None,
            stat_known: false,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: None,
            transfer_mode: None,
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
        units: vec![UnitKey::WHOLE],
    };
    store
        .reserve_transfer(
            &WriteScope {
                library_id: candidate.file.library_id,
                config_revision: 0,
            },
            &candidate,
            Path::new("/downloads/interrupted.mkv"),
            library::TransferMode::Copy,
            1_000,
        )
        .unwrap()
}

struct RecoveredTracks;
impl TracksProbe for RecoveredTracks {
    fn probe(
        &self,
        _: &Path,
        _: &library::WorkControl,
    ) -> Result<(library::Tracks, Option<i64>), library::LibraryError> {
        Ok((
            library::Tracks {
                video: Some(library::VideoTrack {
                    codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(120_000),
        ))
    }
}

#[test]
fn verify_recovers_placed_transfer_source_and_probe_work() {
    // Construct outside Tokio: no background dispatcher can consume the obligation
    // before the test observes the committed recovery facts.
    let mut fix = PresenceFixture::new();
    let reserved = reserve(&fix, "Recovered.mkv");
    let store = fix.state.store();
    assert!(store.scan_probe_obligations(10).unwrap().is_empty());
    std::fs::write(&reserved.file.path, b"completed Transfer bytes").unwrap();
    let source = library::observe_source(&reserved.file.path).unwrap();
    fix.state.probe = Arc::new(
        ProbeManager::new(store.clone())
            .unwrap()
            .with_tracks_probe(Arc::new(RecoveredTracks)),
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (status, body) = fix
            .request(
                "POST",
                &format!("/api/v1/libraries/{}/verify", fix.library_id),
                Some(&fix.admin_token),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let recovered = store
            .file_coverage_by_id(reserved.file.file_id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.file.publication, FilePublication::Published);
        assert_eq!(recovered.file.version.source_version, source.source_version);
        assert_eq!(
            recovered.file.version.revision,
            reserved.file.version.revision + 1
        );
        assert_eq!(recovered.file.size_bytes, source.size_bytes);
        assert_eq!(recovered.file.modified_ns, source.modified_ns);
        assert_eq!(recovered.units, reserved.units);
        let obligations = store.scan_probe_obligations(10).unwrap();
        assert_eq!(obligations.len(), 1);
        assert_eq!(obligations[0].ledger_id, reserved.file.file_id);
        assert_eq!(
            api::scan_probe_dispatch::dispatch_once(fix.state.clone())
                .await
                .unwrap(),
            1
        );
        assert!(fix.state.probe.try_start_workers());
        let id = reserved.file.file_id.to_string();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while store.get_file_meta(&id).unwrap().is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("recovered source must accept probe publication");
        fix.state.probe.shutdown_workers().await.unwrap();
        let tracks = store.get_file_meta(&id).unwrap().unwrap();
        assert_eq!(tracks.video.unwrap().codec.as_deref(), Some("h264"));
        assert_eq!(
            store
                .file_coverage_by_id(reserved.file.file_id)
                .unwrap()
                .unwrap()
                .file
                .duration_ms,
            Some(120_000)
        );
    });
}

#[test]
fn verify_keeps_transfer_pending_when_present_source_cannot_be_observed() {
    let fix = PresenceFixture::new();
    let reserved = reserve(&fix, "Unreadable.mkv");
    fix.mock_files
        .files
        .lock()
        .insert(reserved.file.path.clone(), Ok(true));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let (status, body) = fix
            .request(
                "POST",
                &format!("/api/v1/libraries/{}/verify", fix.library_id),
                Some(&fix.admin_token),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    });
    let store = fix.state.store();
    let current = store
        .file_coverage_by_id(reserved.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.publication, FilePublication::Pending);
    assert_eq!(current.file.version, reserved.file.version);
    assert!(store.scan_probe_obligations(10).unwrap().is_empty());
}
