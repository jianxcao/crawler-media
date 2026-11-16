use super::*;

use domain::{LedgerId, MediaId, UserId};
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};

struct Sources {
    fix: PresenceFixture,
    media: MediaId,
    default: LedgerId,
    selected: LedgerId,
    user: UserId,
}

impl Sources {
    fn new() -> Self {
        // Build before entering Tokio so this HTTP regression starts no background workers.
        let fix = PresenceFixture::new();
        let (media, _) = fix.insert_media_and_ledger("first.mkv");
        let store = fix.state.store();
        publish_fixture_file(
            &store,
            &FixtureFile {
                media_id: media,
                path: fix.root.join("second.mkv").display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let user = store.user_by_token(&fix.admin_token).unwrap().unwrap().id;
        let sources = store
            .playable_entries(user, &domain::TypedItemId::Work(media))
            .unwrap();
        assert_eq!(sources.len(), 2);
        Self {
            fix,
            media,
            default: sources[0].file.file_id,
            selected: sources[1].file.file_id,
            user,
        }
    }

    async fn issue(&self, file: Option<LedgerId>, device: &str) -> Value {
        let mut body = json!({ "media_item_id": self.media, "device_id": device });
        if let Some(file) = file {
            body["file_id"] = json!(file);
        }
        let (status, body) = self
            .fix
            .request_with_body(
                "POST",
                "/api/v1/playback/sessions",
                Some(&self.fix.admin_token),
                body,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["data"].clone()
    }

    async fn event(&self, session: &Value, device: &str, body: Value) -> (StatusCode, Value) {
        let mut request = json!({
            "device_id": device,
            "play_session_id": session["play_session_id"],
            "media_item_id": self.media,
            "event_sequence": support::session::next_sequence(),
            "duration_ms": 600_000,
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        self.fix
            .request_with_body(
                "POST",
                "/api/v1/playback/progress",
                Some(&self.fix.admin_token),
                request,
            )
            .await
    }

    async fn seed_resume(&self) {
        let session = self.issue(Some(self.selected), "seed").await;
        for event in ["start", "stop"] {
            let (status, body) = self
                .event(
                    &session,
                    "seed",
                    json!({ "event": event, "file_id": self.selected, "position_ms": 10_000 }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
    }
}

fn run(work: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(work);
}

#[test]
fn automatic_resume_heartbeats_keep_the_issued_source() {
    let sources = Sources::new();
    run(async {
        sources.seed_resume().await;
        let session = sources.issue(None, "resume").await;
        assert_eq!(session["file_id"], sources.selected.to_string());
        assert_eq!(session["start_ms"], 10_000);
        let store = sources.fix.state.store();
        for (event, position) in [("start", 20_000), ("progress", 30_000), ("stop", 40_000)] {
            let (status, body) = sources
                .event(
                    &session,
                    "resume",
                    json!({ "event": event, "position_ms": position }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["data"]["position_ms"], position, "{event}: {body}");
            let unit = store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap()
                .unwrap();
            assert_eq!(
                unit.position_ms, position,
                "{event} must save the issued source's progress"
            );
            if event != "stop" {
                let live = store.get_session(sources.user, "resume").unwrap().unwrap();
                assert_eq!(live.ledger_id, Some(sources.selected.to_string()));
            }
        }
        assert!(store.get_session(sources.user, "resume").unwrap().is_none());
    });
}

#[test]
fn explicit_heartbeat_source_cannot_replace_the_issued_source() {
    let sources = Sources::new();
    run(async {
        sources.seed_resume().await;
        let session = sources.issue(None, "resume").await;
        assert_eq!(session["file_id"], sources.selected.to_string());
        let store = sources.fix.state.store();
        let before = store
            .unit_state(sources.user, sources.media, -1, -1)
            .unwrap();
        let (status, body) = sources
            .event(
                &session,
                "resume",
                json!({ "event": "progress", "file_id": sources.default, "position_ms": 200_000 }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap(),
            before
        );
        assert!(store.get_session(sources.user, "resume").unwrap().is_none());
    });
}

#[test]
fn heartbeat_without_file_cannot_change_the_issued_media() {
    let sources = Sources::new();
    let (other_media, _) = sources.fix.insert_media_and_ledger("other.mkv");
    run(async {
        sources.seed_resume().await;
        let session = sources.issue(None, "resume").await;
        let store = sources.fix.state.store();
        let before = store
            .unit_state(sources.user, sources.media, -1, -1)
            .unwrap();
        let (status, body) = sources
            .event(
                &session,
                "resume",
                json!({ "event": "progress", "media_item_id": other_media, "position_ms": 200_000 }),
            )
            .await;
        assert!(
            matches!(status, StatusCode::OK | StatusCode::NOT_FOUND),
            "{body}"
        );
        assert_eq!(
            store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap(),
            before
        );
        assert!(
            store
                .unit_state(sources.user, other_media, -1, -1)
                .unwrap()
                .is_none()
        );
        assert!(store.get_session(sources.user, "resume").unwrap().is_none());
    });
}

#[test]
fn omitted_duration_never_carries_another_sources_completion_threshold() {
    for duration in [Some(1_200_000), None] {
        let sources = Sources::new();
        let store = sources.fix.state.store();
        if let Some(duration) = duration {
            let file = store
                .file_coverage_by_id(sources.default)
                .unwrap()
                .unwrap()
                .file;
            store
                .publish_probe_result(
                    file.file_id,
                    &store::library_write::probe::ProbePublication {
                        expected: &file.version,
                        tracks_version: file.tracks_version + 1,
                        resolution: None,
                        codec: None,
                        hdr: store::library_write::probe::Observed::NotObserved,
                        duration_ms: Some(duration),
                        details: None,
                    },
                    1_000,
                )
                .unwrap();
        }
        run(async {
            sources.seed_resume().await;
            let session = sources.issue(Some(sources.default), "changed-source").await;
            for event in ["start", "progress", "stop"] {
                let (status, body) = sources
                    .event(
                        &session,
                        "changed-source",
                        json!({
                            "event": event, "position_ms": 570_000, "duration_ms": null,
                        }),
                    )
                    .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                let unit = store
                    .unit_state(sources.user, sources.media, -1, -1)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    unit.duration_ms, duration,
                    "{event} must use the selected physical source's duration"
                );
                assert!(
                    !unit.played,
                    "another source's shorter duration must not mark this one played"
                );
            }
        });
    }
}

#[test]
fn archived_session_accepts_new_http_progress_without_replaying_old_sequences() {
    let sources = Sources::new();
    run(async {
        let session = sources.issue(Some(sources.selected), "sleep").await;
        let (status, body) = sources
            .event(
                &session,
                "sleep",
                json!({
                    "event": "start", "position_ms": 10_000, "event_sequence": 3,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let store = sources.fix.state.store();
        assert_eq!(
            store
                .sweep_stale_sessions(store::unix_now() + 600, 300)
                .unwrap(),
            1
        );
        let (status, body) = sources
            .event(
                &session,
                "sleep",
                json!({
                    "event": "progress", "position_ms": 20_000, "event_sequence": 4,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap()
                .unwrap()
                .position_ms,
            20_000
        );
        for event in ["progress", "stop"] {
            let (status, body) = sources
                .event(
                    &session,
                    "sleep",
                    json!({
                        "event": event, "position_ms": 1_000, "event_sequence": 3,
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        assert_eq!(
            store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap()
                .unwrap()
                .position_ms,
            20_000
        );
        let (status, body) = sources
            .event(
                &session,
                "sleep",
                json!({
                    "event": "stop", "position_ms": 30_000, "event_sequence": 5,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            store
                .unit_state(sources.user, sources.media, -1, -1)
                .unwrap()
                .unwrap()
                .position_ms,
            30_000
        );
        assert!(store.get_session(sources.user, "sleep").unwrap().is_none());
    });
}
