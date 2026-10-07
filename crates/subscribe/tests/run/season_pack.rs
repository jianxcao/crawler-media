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

    assert_eq!(outcome.ledger.len(), 3);
    let mut episodes: Vec<Option<u32>> = outcome.ledger.iter().map(|row| row.episode).collect();
    episodes.sort_unstable();
    assert_eq!(episodes, vec![Some(1), Some(2), Some(3)]);
    assert!(outcome.completed);
}
