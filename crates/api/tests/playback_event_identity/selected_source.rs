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
