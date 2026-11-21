//! P2-4: Season analysis pagination excludes multi-episode merged files (both mid-page and at page end)
//! and paginates strictly using SQL row coordinates without failing on merged files.
use domain::{Media, MediaId, MediaKind, UnitKey};
use store::Store;
use store::test_support::{FixtureFile, publish_fixture_file, publish_fixture_file_units};

#[test]
fn p2_4_season_analysis_pagination_skips_merged_files_at_page_mid_and_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = MediaId::new();
    store
        .insert_media(&Media {
            id: media,
            kind: MediaKind::Tv,
            title: "Long Season".into(),
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

    // Publish single episodes E01..E04, merged E05-E06 (mid-page), E07..E15,
    // merged E16-E17 (at the 16-row page boundary!), and E18..E20.
    let mut expected_single_episodes = Vec::new();
    for ep in 1..=20 {
        if matches!(ep, 5 | 6 | 16 | 17) {
            continue;
        }
        let path = root.join(format!("Show.S01E{ep:02}.mkv"));
        std::fs::write(&path, b"ep").unwrap();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                season: Some(1),
                episode: Some(ep),
                ..Default::default()
            },
        )
        .unwrap();
        expected_single_episodes.push(ep as i32);
    }
    for (a, b) in [(5, 6), (16, 17)] {
        let path = root.join(format!("Show.S01E{a:02}E{b:02}.mkv"));
        std::fs::write(&path, b"merged").unwrap();
        publish_fixture_file_units(
            &store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
            &[UnitKey::new(1, a), UnitKey::new(1, b)],
        )
        .unwrap();
    }

    // Paginate through the whole season using limit=16
    let mut seen_episodes = Vec::new();
    let mut cursor = None;
    loop {
        let (page, next_cursor) = store
            .analysis_season_files_page(media, 1, cursor, 16)
            .unwrap();
        let Some(next) = next_cursor else {
            break;
        };
        let last = page.last().unwrap();
        assert_eq!(
            last.units.len(),
            1,
            "page boundary must never return a multi-episode merged file: {:?}",
            last.units
        );
        cursor = Some(next);
        for row in page {
            assert_eq!(
                row.units.len(),
                1,
                "merged file must be excluded from season analysis inputs: {:?}",
                row.units
            );
            seen_episodes.push(row.units[0].episode);
        }
    }

    assert_eq!(seen_episodes, expected_single_episodes);
}
