use super::*;
use domain::{
    FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, MediaKind,
    QualitySource, UnitKey,
};

#[test]
fn observe_transfer_files_reports_effective_copy_on_hardlink_fallback_and_degraded_move() {
    let (_tmp, state, _media, _sub) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let src_move_intact = dir.path().join("src-intact.mkv");
    let dst_move_intact = dir.path().join("dst-intact.mkv");
    let dst_move_gone = dir.path().join("dst-gone.mkv");
    let dst_hl_fallback = dir.path().join("dst-hl-fallback.mkv");
    let dst_hl_real = dir.path().join("dst-hl-real.mkv");
    let dst_hl_alias = dir.path().join("dst-hl-alias.mkv");
    for p in [
        &src_move_intact,
        &dst_move_intact,
        &dst_move_gone,
        &dst_hl_fallback,
        &dst_hl_real,
    ] {
        std::fs::write(p, b"video").unwrap();
    }
    std::fs::hard_link(&dst_hl_real, &dst_hl_alias).unwrap();

    let make_cov =
        |path: std::path::PathBuf, src: Option<std::path::PathBuf>, mode: &str| FileCoverage {
            path_key: String::new(),
            file: LibraryFile {
                file_id: LedgerId::new(),
                library_id: domain::LibraryId::new(),
                root_id: String::new(),
                media_id: domain::MediaId::new(),
                kind: MediaKind::Movie,
                path,
                version: FileVersion {
                    revision: 1,
                    source_version: "v1".into(),
                },
                presence: FilePresence::Present,
                publication: FilePublication::Published,
                size_bytes: Some(5),
                stat_known: true,
                created_ns: None,
                modified_ns: None,
                source_digest: None,
                source_path: src,
                transfer_mode: Some(mode.into()),
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: domain::Confidence::High,
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

    let files = vec![
        make_cov(dst_hl_fallback, None, "hardlink"),
        make_cov(dst_hl_real, None, "hardlink"),
        make_cov(dst_move_intact, Some(src_move_intact), "move"),
        make_cov(dst_move_gone, Some(dir.path().join("removed.mkv")), "move"),
    ];
    let obs = observe_transfer_files(&state, &files, "hardlink");
    assert_eq!(
        obs[0].mode, "copy",
        "hardlink fallback (nlink==1) must report copy"
    );
    assert_eq!(
        obs[1].mode, "hardlink",
        "real hardlink (nlink>1) must report hardlink"
    );
    assert_eq!(
        obs[2].mode, "copy",
        "degraded move with source intact must report copy"
    );
    assert_eq!(
        obs[3].mode, "move",
        "move with missing source must report move"
    );
}
