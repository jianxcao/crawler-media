use std::collections::BTreeMap;

use domain::{Media, MediaId, MediaKind};
use rusqlite::Connection;
use store::playback::MediaMergeRequest;
use store::{Store, StoredFingerprintModel, StoredFingerprintModelMember};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    survivor: MediaId,
    loser: MediaId,
    file: domain::LibraryFile,
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
        Self {
            dir,
            store,
            survivor,
            loser,
            file,
        }
    }

    fn db(&self) -> Connection {
        Connection::open(self.dir.path().join("library.db")).unwrap()
    }

    fn model(&self) -> StoredFingerprintModel {
        StoredFingerprintModel {
            model_id: "season-model".into(),
            media_id: self.loser.to_string(),
            season: 1,
            kind: "intro".into(),
            model_version: 1,
            membership_key: "membership".into(),
            policy_key: "default".into(),
            model_json: "{}".into(),
            created_at_ms: 1000,
        }
    }

    fn members(&self) -> Vec<StoredFingerprintModelMember> {
        vec![StoredFingerprintModelMember {
            model_id: "season-model".into(),
            sample_id: "sample-one".into(),
            ledger_id: self.file.file_id.to_string(),
            source_version: self.file.version.source_version.clone(),
        }]
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
