use super::*;

#[test]
fn movie_fill_completes_after_one_acceptable_torrent() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/1",
    );
    let src = tmp.path().join("src.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &t, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![t.clone()],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert!(outcome.completed);
    assert_eq!(dl.added().len(), 1);
    assert_eq!(outcome.ledger.len(), 1);
    assert_eq!(outcome.ledger[0].media_id, media.id);
    assert_eq!(outcome.facts.movie().unwrap().score, 100);
}

#[test]
fn tv_fill_does_not_send_already_owned_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_tv();
    let filter = movie_filter();
    let sub = tv_sub(&media, &filter, false, false);
    let e1 = torrent(
        "The.Expanse.S01E01.1080p.BluRay.x264",
        "https://pt.example/dl/e1",
    );
    let e2 = torrent(
        "The.Expanse.S01E02.1080p.BluRay.x264",
        "https://pt.example/dl/e2",
    );
    let src = tmp.path().join("e2.mkv");
    write_probed(&src, "1080p", "h264", "");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &e2, src);

    let mut facts = SubscribeFacts::default();
    facts.upsert(
        Some(1),
        Some(1),
        QualityFact {
            score: 50,
            path: Some("/already/e1.mkv".into()),
        },
    );

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![e1, e2.clone()],
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    let added = dl.added();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].enclosure, e2.enclosure);
    assert!(outcome.facts.get(Some(1), Some(1)).is_some());
    assert!(outcome.facts.get(Some(1), Some(2)).is_some());
    assert!(!outcome.completed);
}
#[test]
fn quality_match_does_not_download_the_wrong_media() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let wrong = torrent(
        "Dune.Part.Two.2024.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/dune",
    );
    let source = tmp.path().join("dune.mkv");
    fs::write(&source, b"fixture").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &wrong, source);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![wrong],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert!(dl.added().is_empty());
    assert!(outcome.ledger.is_empty());
    assert!(!outcome.completed);
}

#[test]
fn admit_and_add_does_not_wait_for_downloader_files() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/later",
    );
    let src = tmp.path().join("later.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    let library_root = tmp.path().join("lib");

    let added = subscribe::admit_and_add(run_input(
        &sub,
        &media,
        &filter,
        vec![t.clone()],
        SubscribeFacts::default(),
        &dl,
        &library_root,
    ))
    .unwrap();
    assert_eq!(dl.added().len(), 1);
    assert_eq!(added.chosen.len(), 1);
    assert!(added.outcome.ledger.is_empty());
    assert!(!added.outcome.completed);

    map_file(&dl, &t, src);
    let outcome = subscribe::collect_completed(added, &FixedProbe).unwrap();
    assert_eq!(outcome.ledger.len(), 1);
    assert!(outcome.completed);
}
