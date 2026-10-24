//! N02: transfer reservation/completion must close the scope against the
//! actual persisted file (Library, Media writability, pending CAS).
#[path = "support/mod.rs"]
mod support;

use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    LibraryId, Media, MediaId, MediaKind, QualitySource, UnitKey,
};
use std::path::{Path, PathBuf};
use store::library_write::WriteScope;
use support::library_presence::PresenceFixture;

struct Fx {
    fix: PresenceFixture,
    a: (LibraryId, String),
    b: (LibraryId, String),
    media: MediaId,
}

fn library(fix: &PresenceFixture, name: &str, root: &str) -> (LibraryId, String) {
    let store = fix.state.store();
    let store = store.lock();
    let lib = store
        .create_library(MediaKind::Tv, name, &[root], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_deref() == Some(&lib.id))
        .map(|r| r.id)
        .unwrap();
    (lib.id.parse().unwrap(), root_id)
}

fn fixture() -> Fx {
    let fix = PresenceFixture::new();
    let a = library(&fix, "A", "/tv_a");
    let b = library(&fix, "B", "/tv_b");
    let media = MediaId::new();
    fix.state
        .store()
        .lock()
        .insert_media(&Media {
            id: media,
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
    Fx { fix, a, b, media }
}

fn scope(lib: &(LibraryId, String)) -> WriteScope {
    WriteScope {
        library_id: lib.0,
        config_revision: 0,
    }
}

fn candidate(fx: &Fx, lib: &(LibraryId, String), path: &str, file_id: LedgerId) -> FileCoverage {
    FileCoverage {
        path_key: String::new(),
        file: LibraryFile {
            file_id,
            library_id: lib.0,
            root_id: lib.1.clone(),
            media_id: fx.media,
            kind: MediaKind::Tv,
            path: PathBuf::from(path),
            version: FileVersion {
                revision: 1,
                source_version: "v1".into(),
            },
            presence: FilePresence::Present,
            publication: FilePublication::Pending,
            size_bytes: Some(1),
            stat_known: true,
            created_ns: None,
            modified_ns: None,
            source_digest: None,
            source_path: Some(PathBuf::from("/dl/src.mkv")),
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
        units: vec![UnitKey::new(1, 1)],
    }
}

fn reserve(
    fx: &Fx,
    scope: &WriteScope,
    c: &FileCoverage,
) -> Result<FileCoverage, store::StoreError> {
    fx.fix.state.store().lock().reserve_transfer(
        scope,
        c,
        Path::new("/dl/src.mkv"),
        library::TransferMode::Hardlink,
        1_000,
    )
}

fn state_of(fx: &Fx, id: LedgerId) -> Option<(LibraryId, FilePublication)> {
    fx.fix
        .state
        .store()
        .lock()
        .file_coverage_by_id(id)
        .unwrap()
        .map(|c| (c.file.library_id, c.file.publication))
}

#[test]
fn reserve_rejects_candidate_outside_scope_library() {
    let fx = fixture();
    let c = candidate(&fx, &fx.b, "/tv_b/Show/S01E01.mkv", LedgerId::new());
    assert!(reserve(&fx, &scope(&fx.a), &c).is_err());
    assert_eq!(state_of(&fx, c.file.file_id), None);
}

#[test]
fn reserve_rejects_file_id_collision_with_unrelated_published_row() {
    let fx = fixture();
    let first = candidate(&fx, &fx.a, "/tv_a/Show/S01E01.mkv", LedgerId::new());
    let reserved = reserve(&fx, &scope(&fx.a), &first).unwrap();
    let store = fx.fix.state.store();
    store
        .lock()
        .finish_transfer(&scope(&fx.a), &reserved, &reserved.file.version, 2_000)
        .unwrap();
    let mut other = candidate(&fx, &fx.a, "/tv_a/Show/S01E02.mkv", first.file.file_id);
    other.units = vec![UnitKey::new(1, 2)];
    assert!(reserve(&fx, &scope(&fx.a), &other).is_err());
    assert_eq!(
        state_of(&fx, first.file.file_id),
        Some((fx.a.0, FilePublication::Published))
    );
}

#[test]
fn finish_rejects_file_of_another_library_and_non_pending_rows() {
    let fx = fixture();
    let c = candidate(&fx, &fx.b, "/tv_b/Show/S01E01.mkv", LedgerId::new());
    let reserved = reserve(&fx, &scope(&fx.b), &c).unwrap();
    let store = fx.fix.state.store();
    let wrong =
        store
            .lock()
            .finish_transfer(&scope(&fx.a), &reserved, &reserved.file.version, 2_000);
    assert!(wrong.is_err());
    assert_eq!(
        state_of(&fx, c.file.file_id),
        Some((fx.b.0, FilePublication::Pending))
    );
    store
        .lock()
        .finish_transfer(&scope(&fx.b), &reserved, &reserved.file.version, 3_000)
        .unwrap();
    let again =
        store
            .lock()
            .finish_transfer(&scope(&fx.b), &reserved, &reserved.file.version, 4_000);
    assert!(
        again.is_err(),
        "finish must CAS on publication_state = pending"
    );
}

#[test]
fn finish_rejects_frozen_media() {
    let fx = fixture();
    let c = candidate(&fx, &fx.a, "/tv_a/Show/S01E01.mkv", LedgerId::new());
    let reserved = reserve(&fx, &scope(&fx.a), &c).unwrap();
    let store = fx.fix.state.store();
    store::test_support::set_media_identity_state(&store.lock(), fx.media, "merging").unwrap();
    let result =
        store
            .lock()
            .finish_transfer(&scope(&fx.a), &reserved, &reserved.file.version, 2_000);
    assert!(result.is_err());
    assert_eq!(
        state_of(&fx, c.file.file_id),
        Some((fx.a.0, FilePublication::Pending))
    );
}
