//! Content generations and file/unit selection never silently reuse old URLs.
use super::*;

#[test]
fn switching_units_keeps_one_file_generation_and_changing_one_unit_preserves_the_union() {
    let f = Fixture::new(&[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    let file = &f.files[0];
    let marker = |unit, start| store::StoredFileMarker {
        file_id: file.file_id,
        version: file.version.clone(),
        unit,
        intro_start_ms: Some(start),
        intro_end_ms: Some(start + 5_000),
        outro_start_ms: None,
        outro_end_ms: None,
        source: "verified".into(),
        locked: true,
        updated_at: 0,
    };
    f.store
        .put_fixture_marker(&marker(UnitKey::new(1, 1), 1_000))
        .unwrap();
    f.store
        .put_fixture_marker(&marker(UnitKey::new(1, 2), 40_000))
        .unwrap();
    f.publish(0);
    let before = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut second_url = String::new();
        for unit in [
            UnitKey::new(1, 1),
            UnitKey::new(1, 2),
            UnitKey::new(1, 1),
            UnitKey::new(1, 2),
        ] {
            let items = f.chapters(unit).await;
            let intro = items
                .iter()
                .find(|chapter| chapter["marker_type"] == "IntroStart")
                .unwrap();
            assert_eq!(
                intro["start_ms"],
                if unit.episode == 1 { 1_000 } else { 40_000 }
            );
            let url = intro["image_url"].as_str().unwrap();
            assert_eq!(f.get(url).await.status(), StatusCode::OK);
            if unit.episode == 2 {
                second_url = url.to_string();
            }
            assert_eq!(
                f.store
                    .current_chapter_frame_task(file.file_id)
                    .unwrap()
                    .unwrap()
                    .task
                    .id,
                before.task.id
            );
        }
        f.store
            .put_fixture_marker(&marker(UnitKey::new(1, 1), 10_000))
            .unwrap();
        let changed = f
            .store
            .current_chapter_frame_task(file.file_id)
            .unwrap()
            .unwrap();
        assert!(changed.task.targets.contains(&(40_000, 45_000)));
        assert!(changed.task.targets.contains(&(10_000, 15_000)));
        assert!(!changed.task.targets.contains(&(1_000, 6_000)));
        assert_eq!(f.get(&second_url).await.status(), StatusCode::NOT_FOUND);
        f.publish(0);
        let second = f.chapters(UnitKey::new(1, 2)).await;
        let intro = second
            .iter()
            .find(|chapter| chapter["marker_type"] == "IntroStart")
            .unwrap();
        assert_eq!(intro["start_ms"], 40_000);
        assert_eq!(
            f.get(intro["image_url"].as_str().unwrap()).await.status(),
            StatusCode::OK
        );
    });
}

#[test]
fn re_adopted_targets_publish_new_artifacts_and_empty_targets_reject_all_old_urls() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let first = f.publish(0);
    let file = &f.files[0];
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        f.store
            .put_cached_chapters(&file.file_id.to_string(), &[chapter(5_000, 15_000)])
            .unwrap();
        let middle = f.publish(0);
        assert_eq!(f.get(&first[0].0).await.status(), StatusCode::NOT_FOUND);
        f.store
            .put_cached_chapters(
                &file.file_id.to_string(),
                &[chapter(20_000, 30_000), chapter(0, 10_000)],
            )
            .unwrap();
        let fresh = f.publish(0);
        assert_ne!(first[0].0, fresh[0].0);
        assert_eq!(f.get(&fresh[0].0).await.status(), StatusCode::OK);
        assert_eq!(f.get(&first[0].0).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(f.get(&middle[0].0).await.status(), StatusCode::NOT_FOUND);
        f.store
            .put_cached_chapters(&file.file_id.to_string(), &[])
            .unwrap();
        assert!(f.chapters(UnitKey::new(1, 1)).await.is_empty());
        assert_eq!(f.get(&fresh[0].0).await.status(), StatusCode::NOT_FOUND);
        assert!(
            f.store
                .current_chapter_frame_task(file.file_id)
                .unwrap()
                .is_none()
        );
    });
}

#[test]
fn late_old_attempt_cannot_replace_or_remove_the_current_http_artifact() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let file = &f.files[0];
    let snapshot = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    let old = f
        .store
        .claim_chapter_frame_task(&snapshot.task.id)
        .unwrap()
        .unwrap();
    f.store.recover_chapter_frame_tasks().unwrap();
    let current = f.publish(0);
    let mtime = std::fs::metadata(&current[0].2)
        .unwrap()
        .modified()
        .unwrap();
    // The fenced worker publishes only its own immutable bytes after the new
    // attempt has already completed. Its old acknowledgement cannot change DB.
    for target in &old.targets {
        let path = chapter_frame_artifact_path(
            &file.path,
            &old.generation,
            target.index,
            target.candidate.as_ref().unwrap(),
        );
        image::RgbImage::from_pixel(2, 2, image::Rgb([255, 255, 255]))
            .save_with_format(path, image::ImageFormat::Jpeg)
            .unwrap();
    }
    assert!(
        !f.store
            .finish_chapter_frame_attempt(&old.task.id, old.attempt, &[ready(0), ready(1)])
            .unwrap()
    );
    for (index, artifact) in f.store.retire_chapter_frame_candidates(&old).unwrap() {
        std::fs::remove_file(chapter_frame_artifact_path(
            &file.path,
            &old.generation,
            index,
            &artifact,
        ))
        .unwrap();
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for (url, expected, path) in current {
            let response = f.get(&url).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .as_ref(),
                expected
            );
            assert!(path.exists());
        }
    });
    let published = f
        .store
        .current_chapter_frame_task(file.file_id)
        .unwrap()
        .unwrap();
    let path = chapter_frame_artifact_path(
        &file.path,
        &published.status.generation,
        0,
        published.targets[0].published.as_ref().unwrap(),
    );
    assert_eq!(std::fs::metadata(path).unwrap().modified().unwrap(), mtime);
}

#[test]
fn a_registered_source_change_rejects_the_previous_image_identity() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let frames = f.publish(0);
    let file = &f.files[0];
    std::fs::write(&file.path, b"different registered source bytes").unwrap();
    let new = f.register_replacement_source(0);
    assert_ne!(new.version.source_version, file.version.source_version);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_eq!(f.get(&frames[0].0).await.status(), StatusCode::NOT_FOUND);
    });
}

#[test]
fn same_source_coverage_change_rejects_the_removed_units_published_image() {
    let f = Fixture::new(&[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    let file = &f.files[0];
    f.store
        .put_fixture_marker(&store::StoredFileMarker {
            file_id: file.file_id,
            version: file.version.clone(),
            unit: UnitKey::new(1, 2),
            intro_start_ms: Some(40_000),
            intro_end_ms: Some(45_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "verified".into(),
            locked: true,
            updated_at: 0,
        })
        .unwrap();
    f.publish(0);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let items = f.chapters(UnitKey::new(1, 2)).await;
        let url = items
            .iter()
            .find(|chapter| chapter["marker_type"] == "IntroStart")
            .unwrap()["image_url"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(f.get(&url).await.status(), StatusCode::OK);
        let updated = publish_fixture_file_units(
            &f.store,
            &FixtureFile {
                media_id: file.media_id,
                path: file.path.display().to_string(),
                ..Default::default()
            },
            &[UnitKey::new(1, 1)],
        )
        .unwrap();
        assert_eq!(updated.version.source_version, file.version.source_version);
        assert_eq!(f.get(&url).await.status(), StatusCode::NOT_FOUND);
    });
}

#[test]
fn a_published_task_outside_current_chapter_facts_cannot_serve_its_image() {
    let f = Fixture::new(&[UnitKey::new(1, 1)]);
    let file = &f.files[0];
    let targets = vec![(50_000, 60_000)];
    f.store
        .ensure_chapter_frame_task(
            &store::ChapterFrameTask {
                id: "outdated-chapter-facts".into(),
                file_id: file.file_id,
                source_version: file.version.source_version.clone(),
                targets_hash: chapter_frame_generation(
                    file.file_id,
                    &file.version.source_version,
                    &targets,
                    1,
                ),
                targets,
            },
            false,
        )
        .unwrap();
    let frames = f.publish(0);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        assert_eq!(f.get(&frames[0].0).await.status(), StatusCode::NOT_FOUND);
    });
}
