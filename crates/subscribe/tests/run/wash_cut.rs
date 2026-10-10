use super::*;

#[test]
fn wash_cut_replaces_only_when_score_strictly_greater() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let old_lib = tmp.path().join("lib/old.mkv");
    fs::create_dir_all(old_lib.parent().unwrap()).unwrap();
    write_probed(&old_lib, "1080p", "h264", "");
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 50,
            path: Some(old_lib.display().to_string()),
        },
    );
    facts.set_quality(
        old_lib.display().to_string(),
        release::parse("The.Matrix.1999.1080p.BluRay.x264"),
    );

    let worse = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-OLD",
        "https://pt.example/dl/worse",
    );
    let better = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-NEW",
        "https://pt.example/dl/better",
    );
    let src = tmp.path().join("better.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &worse, tmp.path().join("missing-worse"));
    map_file(&dl, &better, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![worse, better],
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert_eq!(dl.added().len(), 1);
    assert_eq!(dl.added()[0].title, "The.Matrix.1999.2160p.BluRay.x265-NEW");
    assert_eq!(outcome.facts.movie().unwrap().score, 100);
    assert!(!old_lib.exists());
    let seed = tmp.path().join("stage");
    assert!(seed.exists());
}

struct FileProbe;

fn tagged(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = text.find(&needle)? + needle.len();
    let rest = text.get(start..)?;
    let end = rest.find('"')?;
    let value = &rest[..end];
    (!value.is_empty()).then(|| value.to_string())
}

impl library::MediaProbe for FileProbe {
    fn probe(&self, path: &std::path::Path) -> Result<library::FileQuality, library::LibraryError> {
        let text = fs::read_to_string(path).map_err(library::LibraryError::Io)?;
        Ok(library::FileQuality {
            resolution: tagged(&text, "resolution"),
            codec: tagged(&text, "codec"),
            hdr: tagged(&text, "hdr"),
        })
    }
}

#[test]
fn probed_lower_quality_does_not_delete_the_owned_file() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let old_lib = tmp.path().join("lib/old.mkv");
    fs::create_dir_all(old_lib.parent().unwrap()).unwrap();
    write_probed(&old_lib, "1080p", "h264", "");
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 50,
            path: Some(old_lib.display().to_string()),
        },
    );
    facts.set_quality(
        old_lib.display().to_string(),
        release::parse("The.Matrix.1999.1080p.BluRay.x264"),
    );
    let labeled_better = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-FAKE",
        "https://pt.example/dl/fake",
    );
    let src = tmp.path().join("fake.mkv");
    write_probed(&src, "720p", "h264", "");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &labeled_better, src);
    let library = tmp.path().join("lib");
    let mut input = run_input(
        &sub,
        &media,
        &filter,
        vec![labeled_better],
        facts,
        &dl,
        &library,
    );
    input.wash_filter = Some(&filter);

    let outcome = subscribe::run_with_probe(input, &FileProbe).unwrap();

    assert!(
        old_lib.exists(),
        "标称更高、probe 更低的新文件不能删除在位版本"
    );
    assert!(
        !outcome
            .removed_paths
            .iter()
            .any(|path| path == &old_lib.display().to_string()),
        "旧文件不能进入删除清单: {:?}",
        outcome.removed_paths
    );
}

#[test]
fn unknown_quality_keeps_old_file_and_never_submits_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let old = tmp.path().join("lib/unknown.mkv");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, b"owned bytes").unwrap();
    let mut facts = SubscribeFacts::default();
    facts.replace(
        None,
        None,
        QualityFact {
            score: 0,
            path: Some(old.display().to_string()),
        },
    );
    let candidate = torrent(
        "The.Matrix.1999.2160p.BluRay.x265",
        "https://pt.example/new",
    );
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![candidate],
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();
    assert!(dl.added().is_empty());
    assert_eq!(fs::read(&old).unwrap(), b"owned bytes");
    assert!(outcome.removed_paths.is_empty());
    assert_eq!(outcome.facts.movie().unwrap().path.as_deref(), old.to_str());
}

#[test]
fn wash_cut_preserve_removed_keeps_old_file() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let old_lib = tmp.path().join("lib/old.mkv");
    fs::create_dir_all(old_lib.parent().unwrap()).unwrap();
    write_probed(&old_lib, "1080p", "h264", "");
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 50,
            path: Some(old_lib.display().to_string()),
        },
    );
    facts.set_quality(
        old_lib.display().to_string(),
        release::parse("The.Matrix.1999.1080p.BluRay.x264"),
    );
    let better = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-NEW",
        "https://pt.example/dl/better",
    );
    let src = tmp.path().join("better.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &better, src);

    let outcome = run(RunInput {
        preserve_removed: true,
        ..run_input(
            &sub,
            &media,
            &filter,
            vec![better],
            facts,
            &dl,
            &tmp.path().join("lib"),
        )
    })
    .unwrap();

    assert_eq!(outcome.removed_paths, vec![old_lib.display().to_string()]);
    assert!(old_lib.exists(), "preserve_removed keeps the replaced file");
}

#[test]
fn wash_cut_never_decreases_recorded_score() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 100,
            path: Some("/lib/uhd.mkv".into()),
        },
    );
    let worse = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-OLD",
        "https://pt.example/dl/worse",
    );
    let dl = MemoryDownloader::new(tmp.path().join("stage"));

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![worse],
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert!(dl.added().is_empty());
    assert_eq!(outcome.facts.movie().unwrap().score, 100);
}
#[test]
fn wash_cut_remains_active_after_owning_a_movie() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, true);
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 100,
            path: Some("/lib/movie.mkv".into()),
        },
    );
    let dl = MemoryDownloader::new(tmp.path().join("stage"));

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        Vec::new(),
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert!(!outcome.completed);
}

#[test]
fn wash_cut_keep_old_versions_keeps_both_files() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let mut sub = movie_sub(&media, &filter, true);
    sub.keep_old_versions = true;
    let old_lib = tmp.path().join("lib/old.mkv");
    fs::create_dir_all(old_lib.parent().unwrap()).unwrap();
    write_probed(&old_lib, "1080p", "h264", "");
    let mut facts = SubscribeFacts::default();
    facts.upsert(
        None,
        None,
        QualityFact {
            score: 50,
            path: Some(old_lib.display().to_string()),
        },
    );
    facts.set_quality(
        old_lib.display().to_string(),
        release::parse("The.Matrix.1999.1080p.BluRay.x264"),
    );
    let better = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-NEW",
        "https://pt.example/dl/better2",
    );
    let src = tmp.path().join("better2.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &better, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![better],
        facts,
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert!(
        old_lib.exists(),
        "keep_old_versions keeps the replaced file"
    );
    assert!(
        outcome.removed_paths.is_empty(),
        "no removed paths reported"
    );
    // 旧台账行由 api 层保留（removed_paths 为空 → 不删 ledger 行）。
    assert_eq!(outcome.ledger.len(), 1, "outcome carries only the new row");
}
