use super::*;

#[test]
fn full_season_pack_is_a_flag_on_the_same_subscribe() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_tv();
    let filter = movie_filter();
    let sub = tv_sub(&media, &filter, false, true);
    let single = torrent(
        "The.Expanse.S01E01.1080p.BluRay.x264",
        "https://pt.example/dl/e1",
    );
    let pack = torrent(
        "The.Expanse.S01E01-E03.1080p.BluRay.x264",
        "https://pt.example/dl/pack",
    );
    let src = tmp.path().join("pack.mkv");
    write_probed(&src, "1080p", "h264", "");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &pack, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![single, pack.clone()],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert_eq!(dl.added().len(), 1);
    assert_eq!(dl.added()[0].enclosure, pack.enclosure);
    assert!(outcome.facts.get(Some(1), Some(1)).is_some());
    assert!(outcome.facts.get(Some(1), Some(2)).is_some());
    assert!(outcome.facts.get(Some(1), Some(3)).is_some());
    assert!(outcome.completed);
}
#[test]
fn full_season_pack_transfers_each_episode_file_to_its_own_ledger_slot() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_tv();
    let filter = movie_filter();
    let sub = tv_sub(&media, &filter, false, true);
    let pack = torrent(
        "The.Expanse.S01E01-E03.1080p.BluRay.x264",
        "https://pt.example/dl/pack-files",
    );
    let files: Vec<PathBuf> = (1..=3)
        .map(|episode| {
            let path = tmp.path().join(format!(
                "The.Expanse.S01E{episode:02}.1080p.BluRay.x264.mkv"
            ));
            fs::write(&path, b"fixture").unwrap();
            path
        })
        .collect();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&pack.enclosure, files);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![pack],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert_eq!(outcome.files.len(), 3);
    let mut episodes: Vec<i32> = outcome
        .files
        .iter()
        .flat_map(|coverage| coverage.units.iter().map(|unit| unit.episode))
        .collect();
    episodes.sort_unstable();
    assert_eq!(episodes, vec![1, 2, 3]);
    assert!(outcome.completed);
}

#[test]
fn single_file_season_pack_populates_covered_units_for_window() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_tv();
    let filter = movie_filter();

    // Window 5..=8 with single-file season compilation `Show.S01.1080p.mkv`.
    let mut sub_5_8 = tv_sub(&media, &filter, false, true);
    sub_5_8.coverage = Coverage::Tv {
        season: 1,
        episode_from: 5,
        episode_to: Some(8),
    };
    let pack = torrent(
        "The.Expanse.S01.1080p.BluRay.x264",
        "https://pt.example/dl/s01-single-5-8",
    );
    let src = tmp.path().join("The.Expanse.S01.1080p.mkv");
    write_probed(&src, "1080p", "h264", "");
    let dl = MemoryDownloader::new(tmp.path().join("stage-5-8"));
    map_file(&dl, &pack, src);
    let outcome = run(run_input(
        &sub_5_8,
        &media,
        &filter,
        vec![pack],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib-5-8"),
    ))
    .unwrap();
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(
        outcome.files[0].units,
        vec![
            domain::UnitKey::new(1, 5),
            domain::UnitKey::new(1, 6),
            domain::UnitKey::new(1, 7),
            domain::UnitKey::new(1, 8),
        ]
    );
    assert!(outcome.completed);

    // Window 1..=4 with single-file season compilation `Show.S01.1080p.mkv`.
    let mut sub_1_4 = tv_sub(&media, &filter, false, true);
    sub_1_4.coverage = Coverage::Tv {
        season: 1,
        episode_from: 1,
        episode_to: Some(4),
    };
    let pack_1_4 = torrent(
        "The.Expanse.S01.1080p.BluRay.x264",
        "https://pt.example/dl/s01-single-1-4",
    );
    let src_1_4 = tmp.path().join("The.Expanse.S01.1080p.1-4.mkv");
    write_probed(&src_1_4, "1080p", "h264", "");
    let dl_1_4 = MemoryDownloader::new(tmp.path().join("stage-1-4"));
    map_file(&dl_1_4, &pack_1_4, src_1_4);
    let outcome_1_4 = run(run_input(
        &sub_1_4,
        &media,
        &filter,
        vec![pack_1_4],
        SubscribeFacts::default(),
        &dl_1_4,
        &tmp.path().join("lib-1-4"),
    ))
    .unwrap();
    assert_eq!(outcome_1_4.files.len(), 1);
    assert_eq!(
        outcome_1_4.files[0].units,
        vec![
            domain::UnitKey::new(1, 1),
            domain::UnitKey::new(1, 2),
            domain::UnitKey::new(1, 3),
            domain::UnitKey::new(1, 4),
        ]
    );
    assert!(outcome_1_4.completed);

    // Multi-episode file `S01E01-E02` continues using `episode_span()`.
    let sub_multi = tv_sub(&media, &filter, false, false);
    let multi = torrent(
        "The.Expanse.S01E01-E02.1080p.BluRay.x264",
        "https://pt.example/dl/multi-1-2",
    );
    let src_multi = tmp.path().join("The.Expanse.S01E01-E02.1080p.mkv");
    write_probed(&src_multi, "1080p", "h264", "");
    let dl_multi = MemoryDownloader::new(tmp.path().join("stage-multi"));
    map_file(&dl_multi, &multi, src_multi);
    let outcome_multi = run(run_input(
        &sub_multi,
        &media,
        &filter,
        vec![multi],
        SubscribeFacts::default(),
        &dl_multi,
        &tmp.path().join("lib-multi"),
    ))
    .unwrap();
    assert_eq!(outcome_multi.files.len(), 1);
    assert_eq!(
        outcome_multi.files[0].units,
        vec![domain::UnitKey::new(1, 1), domain::UnitKey::new(1, 2)]
    );
}
