//! 测试专用的读模型夹具：把旧的台账行 fixture 换成 `FileCoverage`。
//!
//! 只构造「文件事实 + 单元」这一组事实；需要落库时走 [`publish`]，它复用
//! `store::test_support::publish_fixture_file` 的规范化发布路径，并回填发布后的
//! `LibraryFile`（版本/来源版本与库里一致），避免测试拿着未落库的快照比对。

use domain::{
    Confidence, FileCoverage, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile,
    MediaId, MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file};

/// 构造一个未落库的覆盖事实：`season`/`episode` 同有同无，否则是整文件单元。
pub(super) fn coverage(
    media_id: MediaId,
    path: &str,
    season: Option<u32>,
    episode: Option<u32>,
) -> FileCoverage {
    coverage_with_id(LedgerId::new(), media_id, path, season, episode)
}

/// 同 [`coverage`]，但指定 `file_id`（调用方要预先知道台账 id 时用）。
pub(super) fn coverage_with_id(
    file_id: LedgerId,
    media_id: MediaId,
    path: &str,
    season: Option<u32>,
    episode: Option<u32>,
) -> FileCoverage {
    FileCoverage {
        file: library_file(file_id, media_id, path),
        units: units_for(season, episode),
    }
}

/// 由调度快照里的字符串 id 还原覆盖事实（`*_for_test` 回填队列用）。
pub(super) fn synthetic(ledger_id: &str) -> FileCoverage {
    coverage_with_id(
        ledger_id.parse().expect("dispatch ledger id"),
        MediaId::new(),
        "",
        None,
        None,
    )
}

/// 把夹具落库，并返回库里那份文件事实（保留原单元）。
pub(super) fn publish(store: &Store, cov: &FileCoverage) -> FileCoverage {
    let spec = fixture_spec(cov);
    let file = publish_fixture_file(store, &spec).expect("publish fixture file");
    FileCoverage {
        file,
        units: units_for(spec.season, spec.episode),
    }
}

/// 就地落库：测试常把同一份绑定既发布又用于构造 `ProbeUnit`，必须换成库里那份
/// （含 `version.source_version`），否则探测来源版本比对会拿旧快照比。
pub(super) fn publish_into(store: &Store, cov: &mut FileCoverage) {
    let published = publish(store, cov);
    *cov = published;
}

/// 覆盖事实 → 发布规格；只映射旧行 fixture 携带过的那几个字段。
pub(super) fn fixture_spec(cov: &FileCoverage) -> FixtureFile {
    let (season, episode) = cov
        .units
        .first()
        .filter(|unit| !unit.is_whole())
        .map_or((None, None), |unit| {
            (Some(unit.season as u32), Some(unit.episode as u32))
        });
    FixtureFile {
        media_id: cov.file.media_id,
        path: cov.file.path.to_string_lossy().into_owned(),
        season,
        episode,
        resolution: cov.file.resolution.clone(),
        codec: cov.file.codec.clone(),
        hdr: cov.file.hdr.clone(),
        quality_source: cov.file.quality_source,
        confidence: cov.file.confidence,
        filter_score: cov.file.filter_score,
        transfer_mode: None,
        source_path: None,
        file_id: Some(cov.file.file_id),
    }
}

fn units_for(season: Option<u32>, episode: Option<u32>) -> Vec<UnitKey> {
    match (season, episode) {
        (Some(season), Some(episode)) => vec![UnitKey::new(season as i32, episode as i32)],
        _ => vec![UnitKey::WHOLE],
    }
}

fn library_file(file_id: LedgerId, media_id: MediaId, path: &str) -> LibraryFile {
    LibraryFile {
        file_id,
        library_id: domain::LibraryId::new(),
        root_id: String::new(),
        media_id,
        kind: MediaKind::Movie,
        path: path.into(),
        version: FileVersion {
            revision: 1,
            source_version: String::new(),
        },
        presence: FilePresence::Present,
        publication: FilePublication::Published,
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
    }
}
