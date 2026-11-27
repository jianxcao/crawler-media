use super::*;

struct ReservationRecorder(std::cell::RefCell<Option<domain::FileCoverage>>);

impl subscribe::TransferRecorder for ReservationRecorder {
    fn prepare(
        &self,
        candidate: &domain::FileCoverage,
        _: &Path,
        _: TransferMode,
    ) -> Result<domain::FileCoverage, subscribe::SubscribeError> {
        assert!(
            !candidate.file.path.exists(),
            "reservation must precede publication"
        );
        *self.0.borrow_mut() = Some(candidate.clone());
        Ok(candidate.clone())
    }

    fn published(&self, candidate: &domain::FileCoverage) -> Result<(), subscribe::SubscribeError> {
        assert!(candidate.file.path.is_file());
        assert_eq!(
            self.0.borrow().as_ref().unwrap().file.file_id,
            candidate.file.file_id
        );
        Ok(())
    }
}

#[test]
fn recorded_collection_reserves_intended_path_before_publication() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let torrent = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/recorded",
    );
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    fs::write(&source, b"video").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &torrent, source);
    let root = tmp.path().join("lib");
    let added = subscribe::admit_and_add(run_input(
        &sub,
        &media,
        &filter,
        vec![torrent],
        SubscribeFacts::default(),
        &dl,
        &root,
    ))
    .unwrap();
    let recorder = ReservationRecorder(Default::default());
    let outcome = subscribe::collect_completed_recorded(
        added,
        &FixedProbe,
        &[],
        &library::WorkControl::default(),
        Some(&recorder),
    )
    .unwrap();
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(
        outcome.files[0].file.path,
        recorder.0.borrow().as_ref().unwrap().file.path
    );
    assert!(outcome.collection_errors.is_empty());
}

#[test]
fn pending_publication_retry_reuses_reserved_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let torrent = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/pending",
    );
    let source = tmp.path().join("The.Matrix.1999.1080p.mkv");
    fs::write(&source, b"video").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &torrent, source.clone());
    let root = tmp.path().join("lib");
    let destination = root.join("reserved.mkv");
    let mut mapping = subscribe::collection_destinations::DestinationMapping::new(
        tmp.path()
            .join("stage")
            .join(source.file_name().unwrap())
            .display()
            .to_string(),
        &destination,
    );
    mapping.transfer_pending = true;
    let added = subscribe::admit_and_add(run_input(
        &sub,
        &media,
        &filter,
        vec![torrent],
        SubscribeFacts::default(),
        &dl,
        &root,
    ))
    .unwrap();
    let recorder = ReservationRecorder(Default::default());
    let outcome = subscribe::collect_completed_recorded(
        added,
        &FixedProbe,
        &[mapping],
        &library::WorkControl::default(),
        Some(&recorder),
    )
    .unwrap();
    assert!(outcome.collection_errors.is_empty());
    assert_eq!(outcome.files.len(), 1);
    assert_eq!(outcome.files[0].file.path, destination);
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn transfer_ledger_row_uses_probed_quality() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/dl/1",
    );
    let src = tmp.path().join("probed.mkv");
    write_probed(&src, "2160p", "hevc", "hdr10");
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &t, src);

    let outcome = run_with_probe(
        run_input(
            &sub,
            &media,
            &filter,
            vec![t],
            SubscribeFacts::default(),
            &dl,
            &tmp.path().join("lib"),
        ),
        &FixedProbe,
    )
    .unwrap();

    let coverage = &outcome.files[0];
    let file = &coverage.file;
    assert_eq!(file.media_id, media.id);
    assert!(file.path.exists());
    assert_eq!(file.resolution.as_deref(), Some("2160p"));
    assert_eq!(file.codec.as_deref(), Some("hevc"));
    assert_eq!(file.hdr.as_deref(), Some("hdr10"));
    assert_eq!(file.quality_source, domain::QualitySource::Probe);
    assert_eq!(coverage.units, vec![domain::UnitKey::WHOLE]);
}

#[test]
fn transfer_falls_back_to_release_when_probe_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/dl/1",
    );
    let src = tmp.path().join("opaque.mkv");
    fs::write(&src, b"\x00\x01not-json").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    map_file(&dl, &t, src);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![t],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    let file = &outcome.files[0].file;
    assert_eq!(file.resolution.as_deref(), Some("1080p"));
    assert_eq!(file.quality_source, domain::QualitySource::Release);
    assert_eq!(file.confidence, domain::Confidence::Low);
}
#[test]
fn extra_files_do_not_overwrite_the_main_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);
    let t = torrent(
        "The.Matrix.1999.2160p.BluRay.x265-GROUP",
        "https://pt.example/dl/extras",
    );
    let feature = tmp
        .path()
        .join("The.Matrix.1999.2160p.BluRay.x265-GROUP.mkv");
    let sample = tmp.path().join("sample.mkv");
    fs::write(&feature, b"feature").unwrap();
    fs::write(&sample, b"sample").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![feature, sample]);

    let outcome = run(run_input(
        &sub,
        &media,
        &filter,
        vec![t],
        SubscribeFacts::default(),
        &dl,
        &tmp.path().join("lib"),
    ))
    .unwrap();

    assert_eq!(outcome.files.len(), 2);
    let mut names: Vec<_> = outcome
        .files
        .iter()
        .map(|coverage| {
            coverage
                .file
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(names.len(), 2);
    assert_ne!(names[0], names[1]);
    for name in &names {
        assert!(name.ends_with(".mkv"));
        assert!(!name.contains("BluRay"));
    }
    for coverage in &outcome.files {
        assert!(coverage.file.path.is_file());
    }
    assert_ne!(
        fs::read(&outcome.files[0].file.path).unwrap(),
        fs::read(&outcome.files[1].file.path).unwrap()
    );
}

#[test]
fn renamed_destination_outside_library_root_or_via_symlink_falls_back_to_default_dest() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let root = tmp.path().join("lib");
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("escape_link")).unwrap();

    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);

    for (idx, bad_dest) in [
        outside.join("escaped1.mkv"),
        root.clone(),
        #[cfg(unix)]
        root.join("escape_link/escaped2.mkv"),
    ]
    .into_iter()
    .enumerate()
    {
        let sub_root = root.join(format!("case-{idx}"));
        fs::create_dir_all(&sub_root).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, sub_root.join("escape_link")).unwrap();
        let target_dest = if bad_dest == root {
            sub_root.clone()
        } else if bad_dest.ends_with("escaped2.mkv") {
            sub_root.join("escape_link/escaped2.mkv")
        } else {
            bad_dest
        };

        let t = torrent(
            "The.Matrix.1999.1080p.BluRay.x264-GROUP",
            &format!("https://pt.example/rename-bad-{idx}"),
        );
        let src_name = format!("The.Matrix.1999.1080p.{idx}.mkv");
        let src = tmp.path().join(&src_name);
        fs::write(&src, b"video").unwrap();
        let stage = tmp.path().join(format!("stage-{idx}"));
        let dl = MemoryDownloader::new(&stage);
        map_file(&dl, &t, src);

        let mut input = run_input(
            &sub,
            &media,
            &filter,
            vec![t],
            SubscribeFacts::default(),
            &dl,
            &sub_root,
        );
        input
            .renamed_destinations
            .insert(stage.join(&src_name), target_dest);
        let outcome = run_with_probe(input, &FixedProbe).unwrap();
        assert_eq!(outcome.files.len(), 1, "case {idx} must fall back cleanly");
        assert!(outcome.files[0].file.path.starts_with(&sub_root));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    // Valid renamed destination inside a not-yet-existing library_root still succeeds.
    let fresh_root = tmp.path().join("fresh/nested/lib");
    let t_ok = torrent(
        "The.Matrix.1999.1080p.BluRay.x264-GROUP",
        "https://pt.example/rename-ok",
    );
    let src_ok = tmp.path().join("The.Matrix.1999.1080p.ok.mkv");
    fs::write(&src_ok, b"video").unwrap();
    let stage_ok = tmp.path().join("stage-ok");
    let dl_ok = MemoryDownloader::new(&stage_ok);
    map_file(&dl_ok, &t_ok, src_ok);
    let custom_dest = fresh_root.join("Custom/Matrix-Renamed.mkv");
    let mut input_ok = run_input(
        &sub,
        &media,
        &filter,
        vec![t_ok],
        SubscribeFacts::default(),
        &dl_ok,
        &fresh_root,
    );
    input_ok.renamed_destinations.insert(
        stage_ok.join("The.Matrix.1999.1080p.ok.mkv"),
        custom_dest.clone(),
    );
    let outcome_ok = run_with_probe(input_ok, &FixedProbe).unwrap();
    assert_eq!(outcome_ok.files.len(), 1);
    assert_eq!(outcome_ok.files[0].file.path, custom_dest);
}

#[test]
fn pending_reservation_via_symlink_or_equal_to_root_is_rejected_by_reserve_and_publish() {
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let outside = tmp.path().join("outside");
    let root = tmp.path().join("lib");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("escape_link")).unwrap();

    let media = media_movie();
    let filter = movie_filter();
    let sub = movie_sub(&media, &filter, false);

    for (idx, bad_dest) in [
        root.clone(),
        #[cfg(unix)]
        root.join("escape_link/escaped.mkv"),
    ]
    .into_iter()
    .enumerate()
    {
        let t = torrent(
            "The.Matrix.1999.1080p.BluRay.x264-GROUP",
            &format!("https://pt.example/pending-bad-{idx}"),
        );
        let src_name = format!("The.Matrix.1999.1080p.p{idx}.mkv");
        let src = tmp.path().join(&src_name);
        fs::write(&src, b"video").unwrap();
        let stage = tmp.path().join(format!("pstage-{idx}"));
        let dl = MemoryDownloader::new(&stage);
        map_file(&dl, &t, src);

        let mut mapping = subscribe::collection_destinations::DestinationMapping::new(
            stage.join(&src_name).display().to_string(),
            &bad_dest,
        );
        mapping.transfer_pending = true;
        let added = subscribe::admit_and_add(run_input(
            &sub,
            &media,
            &filter,
            vec![t],
            SubscribeFacts::default(),
            &dl,
            &root,
        ))
        .unwrap();
        let outcome = subscribe::collect_completed_recorded(
            added,
            &FixedProbe,
            &[mapping],
            &library::WorkControl::default(),
            None,
        )
        .unwrap();
        assert!(outcome.files.is_empty(), "case {idx} must be rejected");
        assert!(!outcome.collection_errors.is_empty());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }
}
