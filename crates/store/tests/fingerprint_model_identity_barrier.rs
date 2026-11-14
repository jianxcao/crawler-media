use std::collections::BTreeMap;

use domain::{Media, MediaId, MediaKind};
use rusqlite::Connection;
use store::playback::MediaMergeRequest;
use store::{Store, StoredFingerprintModel, StoredFingerprintModelMember};
#[path = "support/model_bundle.rs"]
mod model_bundle;
#[path = "fingerprint_model_identity_barrier/quantized.rs"]
mod quantized;

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    survivor: MediaId,
    loser: MediaId,
    file: domain::LibraryFile,
    other: domain::LibraryFile,
    samples: Vec<store::StoredFingerprintSample>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let survivor = MediaId::new();
        let loser = MediaId::new();
        for id in [survivor, loser] {
            store
                .insert_media(&Media {
                    id,
                    kind: MediaKind::Tv,
                    title: "Fingerprint".into(),
                    year: None,
                    original_title: None,
                    aliases: vec![],
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
        }
        let library = store.default_library(MediaKind::Tv).unwrap().unwrap();
        let file = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: loser,
                path: library.root_paths[0].join("one.mkv").display().to_string(),
                season: Some(1),
                episode: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let other = store::test_support::publish_fixture_file(
            &store,
            &store::test_support::FixtureFile {
                media_id: loser,
                path: library.root_paths[0].join("two.mkv").display().to_string(),
                season: Some(1),
                episode: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        let samples = vec![
            model_bundle::capture(&store, &file, 1),
            model_bundle::capture(&store, &other, 2),
        ];
        Self {
            dir,
            store,
            survivor,
            loser,
            file,
            other,
            samples,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.path().join("library.db")).unwrap()
    }

    fn model(&self) -> StoredFingerprintModel {
        model_bundle::bundle(self.loser, &self.samples)
            .model
            .clone()
    }

    fn members(&self) -> Vec<StoredFingerprintModelMember> {
        model_bundle::bundle(self.loser, &self.samples)
            .members
            .clone()
    }

    fn begin(&self) -> String {
        self.store
            .begin_media_merge(
                &MediaMergeRequest {
                    request_id: MediaId::new().to_string(),
                    survivor: self.survivor,
                    loser: self.loser,
                    survivor_revision: 0,
                    loser_revision: 0,
                    refresh_targets: BTreeMap::new(),
                },
                1000,
            )
            .unwrap()
    }

    fn saved_members(&self) -> Vec<(String, String, String, String)> {
        self.db()
            .prepare(
                "SELECT model_id,sample_id,file_id,source_version
            FROM fingerprint_model_members ORDER BY model_id,sample_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[test]
fn whole_model_set_replacement_preserves_previous_on_error_and_accepts_empty_set() {
    let f = Fixture::new();
    let previous = f
        .store
        .bind_fingerprint_model(model_bundle::bundle(f.loser, &f.samples))
        .unwrap();
    f.store
        .library_write(|db| db.put_fingerprint_model_bundle(&previous))
        .unwrap();
    let mut alternate_template: marker::adaptive::TemplateModel =
        serde_json::from_str(&previous.model.model_json).unwrap();
    for reference in &mut alternate_template.references {
        reference.match_interval_ms = (100_000, 160_000);
    }
    let alternate = f
        .store
        .bind_fingerprint_model(
            store::StoredFingerprintModelBundle::new(
                f.loser,
                1,
                "policy".into(),
                alternate_template,
                f.samples.clone(),
                2000,
            )
            .unwrap(),
        )
        .unwrap();
    let set = store::StoredFingerprintModelSet::new(
        f.loser,
        1,
        "policy".into(),
        vec![previous.clone(), alternate.clone()],
    )
    .unwrap();
    f.store
        .library_write(|db| db.replace_fingerprint_model_bundles(&set))
        .unwrap();
    let saved = f
        .store
        .list_fingerprint_models(&f.loser.to_string(), 1)
        .unwrap();
    assert_eq!(
        saved.len(),
        2,
        "all compatible themes survive one set publication"
    );
    let invalid = f
        .store
        .bind_fingerprint_model(
            store::StoredFingerprintModelBundle::new(
                f.loser,
                1,
                "another-policy".into(),
                serde_json::from_str(&alternate.model.model_json).unwrap(),
                f.samples.clone(),
                2000,
            )
            .unwrap(),
        )
        .unwrap();
    let bad =
        store::StoredFingerprintModelSet::new(f.loser, 1, "policy".into(), vec![previous, invalid])
            .unwrap();
    assert!(
        f.store
            .library_write(|db| db.replace_fingerprint_model_bundles(&bad))
            .is_err()
    );
    assert_eq!(
        f.store
            .list_fingerprint_models(&f.loser.to_string(), 1)
            .unwrap(),
        saved
    );
    let empty = store::StoredFingerprintModelSet::new(f.loser, 1, "policy".into(), vec![]).unwrap();
    f.store
        .library_write(|db| db.replace_fingerprint_model_bundles(&empty))
        .unwrap();
    assert!(
        f.store
            .list_fingerprint_models(&f.loser.to_string(), 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn current_members_publish_normally_and_survive_reopen() {
    let f = Fixture::new();
    let model = f.model();
    let members = f.members();
    f.store.put_fingerprint_model(&model, &members).unwrap();
    assert_eq!(
        f.store.list_fingerprint_models(&model.media_id, 1).unwrap(),
        vec![model.clone()]
    );
    assert_eq!(f.saved_members()[0].2, members[0].ledger_id);
    let dir = f.dir.path().to_path_buf();
    drop(f.store);
    let reopened = Store::open(dir).unwrap();
    assert_eq!(
        reopened
            .list_fingerprint_models(&model.media_id, 1)
            .unwrap(),
        vec![model]
    );
}

#[test]
fn canonical_model_cannot_invent_reference_episode_coverage() {
    let f = Fixture::new();
    let mut template: marker::adaptive::TemplateModel =
        serde_json::from_str(&f.model().model_json).unwrap();
    template.references[0].episode = 99;
    let forged = store::StoredFingerprintModelBundle::new(
        f.loser,
        1,
        "policy".into(),
        template,
        f.samples.clone(),
        1000,
    )
    .unwrap();
    assert!(
        f.store
            .put_fingerprint_model(&forged.model, &forged.members)
            .is_err(),
        "a canonical hash must not authenticate an episode absent from FileCoverage"
    );
}

#[test]
fn prepared_model_cannot_publish_after_selected_audio_changes() {
    let f = Fixture::new();
    let model = f.model();
    let members = f.members();
    let prepared = f.store.prepare_fingerprint_model(&model, &members).unwrap();
    let mut tracks = f
        .store
        .get_file_meta(&f.file.file_id.to_string())
        .unwrap()
        .unwrap();
    tracks.audio[0].stream_index = Some(8);
    tracks.audio[0]
        .capture_source
        .as_mut()
        .unwrap()
        .audio_stream_index = 8;
    f.store
        .put_file_meta_versioned(
            &f.file.file_id.to_string(),
            &tracks,
            Some(&f.file.version.source_version),
            Some(1_800_000),
        )
        .unwrap();
    assert!(
        f.store
            .library_write(|db| db.put_fingerprint_model_bundle(&prepared))
            .is_err()
    );
    assert!(
        f.store
            .list_fingerprint_models(&f.loser.to_string(), 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn copies_of_one_episode_cannot_form_an_independent_model() {
    let f = Fixture::new();
    let mut template: marker::adaptive::TemplateModel =
        serde_json::from_str(&f.model().model_json).unwrap();
    for reference in &mut template.references {
        reference.episode = 1;
    }
    assert!(
        store::StoredFingerprintModelBundle::new(
            f.loser,
            1,
            "policy".into(),
            template,
            f.samples.clone(),
            1000
        )
        .is_err()
    );
}

#[test]
fn canonical_model_rejects_unproven_source_profile_and_mapping() {
    for changed in ["source", "profile", "mapping", "audio"] {
        let f = Fixture::new();
        let mut samples = f.samples.clone();
        let mut metrics: marker::CaptureMetrics =
            serde_json::from_str(&samples[0].metrics_json).unwrap();
        match changed {
            "source" => metrics.source_facts = None,
            "mapping" => metrics.time_mapping_version = 2,
            "audio" => metrics.audio_stream_index = Some(8),
            "profile" => samples[0].capture_profile_key = "forged-but-canonical".into(),
            _ => unreachable!(),
        }
        samples[0].metrics_json = serde_json::to_string(&metrics).unwrap();
        if changed == "profile" {
            f.store
                .delete_file_meta_by_ledger_id(&samples[0].ledger_id)
                .unwrap();
        }
        model_bundle::save(&f.store, &f.file, &samples[0]);
        let template = serde_json::from_str(&f.model().model_json).unwrap();
        let accepted = store::StoredFingerprintModelBundle::new(
            f.loser,
            1,
            "policy".into(),
            template,
            samples,
            1000,
        )
        .and_then(|bundle| {
            f.store
                .put_fingerprint_model(&bundle.model, &bundle.members)
        });
        assert!(accepted.is_err(), "accepted {changed}");
    }
}

#[test]
fn model_publication_rejects_missing_exact_sample() {
    let f = Fixture::new();
    // The same window now has another sample identity through the real capture seam.
    let mut replacement = f.samples[0].clone();
    replacement.sample_id = "replacement".into();
    model_bundle::save(&f.store, &f.file, &replacement);
    assert!(
        f.store
            .put_fingerprint_model(&f.model(), &f.members())
            .is_err()
    );
    assert!(
        f.store
            .list_fingerprint_models(&f.loser.to_string(), 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn exact_bundle_reopens_and_rejects_same_id_changed_capture_facts() {
    for change in ["words", "profile", "track", "kind", "references"] {
        let f = Fixture::new();
        let model = f.model();
        f.store.put_fingerprint_model(&model, &f.members()).unwrap();
        let loaded = f
            .store
            .load_fingerprint_model_bundle(&model.model_id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.samples, f.samples);
        if change == "references" {
            let mut changed = model.clone();
            let mut template: marker::adaptive::TemplateModel =
                serde_json::from_str(&changed.model_json).unwrap();
            template.references[0].ledger_id = f.other.file_id.to_string();
            changed.model_json = serde_json::to_string(&template).unwrap();
            assert!(
                f.store
                    .put_fingerprint_model(&changed, &f.members())
                    .is_err()
            );
            assert_eq!(
                f.store
                    .load_fingerprint_model_bundle(&model.model_id)
                    .unwrap()
                    .unwrap()
                    .model,
                model
            );
            continue;
        }
        let mut sample = f.samples[0].clone();
        match change {
            "words" => sample.fingerprint[1] = 999,
            "profile" => sample.capture_profile_key = "changed-profile".into(),
            "track" => {
                let mut metrics: marker::CaptureMetrics =
                    serde_json::from_str(&sample.metrics_json).unwrap();
                metrics.audio_stream_index = Some(7);
                sample.metrics_json = serde_json::to_string(&metrics).unwrap();
            }
            "kind" => sample.kind = "outro".into(),
            _ => unreachable!(),
        }
        // Profile/kind change uses a new exact sample ID to avoid violating the sample PK.
        if matches!(change, "profile" | "kind") {
            f.store
                .delete_file_meta_by_ledger_id(&sample.ledger_id)
                .unwrap();
        }
        model_bundle::save(&f.store, &f.file, &sample);
        let dir = f.dir.path().to_path_buf();
        drop(f.store);
        let reopened = Store::open(dir).unwrap();
        assert!(!matches!(
            reopened.load_fingerprint_model_bundle(&model.model_id),
            Ok(Some(_))
        ));
    }
}

#[test]
fn interrupted_merge_rejects_late_model_and_resumes_after_reopen() {
    let f = Fixture::new();
    let operation = f.begin();
    f.db()
        .execute_batch(
            "CREATE TRIGGER fail_retire BEFORE UPDATE OF phase_cursor
        ON media_operations WHEN NEW.phase_cursor='retire'
        BEGIN SELECT RAISE(ABORT,'injected boundary failure'); END;",
        )
        .unwrap();
    assert!(f.store.advance_media_merge(&operation, 1100).is_err());
    assert_eq!(
        f.store.media_operation_status(&operation).unwrap().phase,
        "subscribes"
    );
    let current = f
        .store
        .file_coverage_by_id(f.file.file_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.file.media_id, f.survivor);
    assert_eq!(
        current.file.version.source_version,
        f.file.version.source_version
    );
    assert!(
        f.store
            .put_fingerprint_model(&f.model(), &f.members())
            .is_err()
    );
    assert!(
        f.store
            .list_fingerprint_models(&f.loser.to_string(), 1)
            .unwrap()
            .is_empty()
    );
    assert!(f.saved_members().is_empty());
    f.db().execute_batch("DROP TRIGGER fail_retire").unwrap();
    let dir = f.dir.path().to_path_buf();
    drop(f.store);
    let reopened = Store::open(dir).unwrap();
    assert_eq!(
        reopened
            .advance_media_merge(&operation, 1300)
            .unwrap()
            .state,
        "succeeded"
    );
}

#[test]
fn frozen_and_retired_models_are_zero_write() {
    let f = Fixture::new();
    let operation = f.begin();
    for media in [f.survivor, f.loser] {
        let mut model = f.model();
        model.media_id = media.to_string();
        assert!(f.store.put_fingerprint_model(&model, &[]).is_err());
        assert!(
            f.store
                .list_fingerprint_models(&media.to_string(), 1)
                .unwrap()
                .is_empty()
        );
    }
    f.store.advance_media_merge(&operation, 1100).unwrap();
    assert!(f.store.put_fingerprint_model(&f.model(), &[]).is_err());
    assert!(f.saved_members().is_empty());
}

#[test]
fn invalid_member_keeps_previous_model_and_members() {
    for invalid in [
        "media",
        "source",
        "publication",
        "presence",
        "root",
        "lifecycle",
        "member_model",
        "missing",
        "season",
    ] {
        let f = Fixture::new();
        let model = f.model();
        let members = f.members();
        f.store.put_fingerprint_model(&model, &members).unwrap();
        let before = f.saved_members();
        let mut replacement = model.clone();
        replacement.model_json = "{\"updated\":true}".into();
        let mut replacement_members = members.clone();
        let db = f.db();
        match invalid {
            "media" => {
                let file = store::test_support::publish_fixture_file(
                    &f.store,
                    &store::test_support::FixtureFile {
                        media_id: f.survivor,
                        path: f
                            .file
                            .path
                            .parent()
                            .unwrap()
                            .join("other.mkv")
                            .display()
                            .to_string(),
                        season: Some(1),
                        episode: Some(2),
                        ..Default::default()
                    },
                )
                .unwrap();
                replacement_members[0].ledger_id = file.file_id.to_string();
                replacement_members[0].source_version = file.version.source_version;
            }
            "source" => {
                replacement_members[0].source_version = "changed-source".into();
            }
            "publication" => {
                db.execute(
                    "UPDATE library_files SET publication_state='pending' WHERE file_id=?1",
                    [f.file.file_id.to_string()],
                )
                .unwrap();
            }
            "presence" => {
                db.execute(
                    "UPDATE library_files SET presence_state='missing' WHERE file_id=?1",
                    [f.file.file_id.to_string()],
                )
                .unwrap();
            }
            "root" => {
                db.execute(
                    "UPDATE library_roots SET enabled=0 WHERE root_id=?1",
                    [&f.file.root_id],
                )
                .unwrap();
            }
            "lifecycle" => {
                db.execute(
                    "UPDATE libraries SET lifecycle_state='reindexing' WHERE library_id=?1",
                    [f.file.library_id.to_string()],
                )
                .unwrap();
            }
            "member_model" => {
                replacement_members[0].model_id = "another-model".into();
            }
            "missing" => {
                replacement_members[0].ledger_id = domain::LedgerId::new().to_string();
            }
            "season" => {
                replacement.season = 2;
            }
            _ => unreachable!(),
        }
        assert!(
            f.store
                .put_fingerprint_model(&replacement, &replacement_members)
                .is_err(),
            "accepted {invalid}"
        );
        assert_eq!(
            f.store.list_fingerprint_models(&model.media_id, 1).unwrap(),
            vec![model],
            "{invalid}"
        );
        assert_eq!(f.saved_members(), before, "{invalid}");
    }
}
