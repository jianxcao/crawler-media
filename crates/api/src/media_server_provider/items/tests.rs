use super::*;

fn unit(
    media_id: domain::MediaId,
    season: i32,
    episode: i32,
    played: bool,
    updated_at: i64,
) -> crate::store::PlaybackUnitSnapshot {
    crate::store::PlaybackUnitSnapshot {
        media_id,
        season,
        episode,
        state: crate::store::UnitState {
            position_ms: 0,
            played,
            favorite: false,
            favorite_at: 0,
            duration_ms: None,
            audio_track: None,
            subtitle_track: None,
            play_count: 0,
            updated_at,
            played_assertion_at_ms: Some(updated_at),
        },
    }
}

fn fixture() -> (domain::Media, domain::LibraryFile) {
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: MediaKind::Tv,
        title: "Show".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let file = domain::LibraryFile {
        file_id: domain::LedgerId::new(),
        library_id: domain::LibraryId::new(),
        root_id: "root".into(),
        media_id: media.id,
        kind: media.kind,
        path: std::path::PathBuf::from("/tv/Show.S01E01.mkv"),
        version: domain::FileVersion {
            revision: 1,
            source_version: "fixture".into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Published,
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
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    (media, file)
}

fn two_episodes(media_id: domain::MediaId) -> HashMap<domain::MediaId, HashSet<(u32, u32)>> {
    let mut episodes = HashMap::new();
    episodes.insert(media_id, HashSet::from([(1_u32, 1_u32), (1, 2)]));
    episodes
}

/// 整剧已看（只有 `(-1,-1)` 单元）时，没有自己单元的集也必须算已看。
#[test]
fn series_rollup_honours_the_whole_show_mark() {
    let (media, file) = fixture();
    let units = vec![unit(
        media.id,
        crate::store::UNIT_WHOLE,
        crate::store::UNIT_WHOLE,
        true,
        10,
    )];
    let index = unit_index(&units);
    let (played, _, unplayed) =
        unit_counts_from_snapshot(&index, &file, (1, 1), &media, true, &two_episodes(media.id));
    assert!(played, "whole-show mark must make every episode played");
    assert_eq!(unplayed, Some(0));
}

/// 更晚的单集标记优先于整剧标记（显式取消已看不被覆盖）。
#[test]
fn series_rollup_keeps_a_newer_episode_mark() {
    let (media, file) = fixture();
    let units = vec![
        unit(
            media.id,
            crate::store::UNIT_WHOLE,
            crate::store::UNIT_WHOLE,
            true,
            10,
        ),
        unit(media.id, 1, 1, false, 20),
        unit(media.id, 1, 2, true, 5),
    ];
    let index = unit_index(&units);
    let (played, _, unplayed) =
        unit_counts_from_snapshot(&index, &file, (1, 1), &media, true, &two_episodes(media.id));
    assert!(
        !played,
        "a newer episode unmark must win over the whole-show mark"
    );
    assert_eq!(unplayed, Some(1));
}

#[test]
fn unknown_coverage_yields_no_playable_units() {
    let (_media, file) = fixture();
    let unknown = domain::FileCoverage {
        path_key: String::new(),
        file: file.clone(),
        units: Vec::new(),
    };
    assert!(coverage_units(&unknown).is_empty());
    let whole = domain::FileCoverage {
        path_key: String::new(),
        file,
        units: vec![domain::UnitKey::WHOLE, domain::UnitKey::new(0, 0)],
    };
    assert_eq!(coverage_units(&whole), vec![(-1, -1), (0, 0)]);
}
