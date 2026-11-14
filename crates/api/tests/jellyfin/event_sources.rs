use super::*;

#[path = "event_sources/bound_source_guards.rs"]
mod bound_source_guards;
#[path = "event_sources/playback_info_race.rs"]
mod playback_info_race;
#[path = "event_sources/shared_episode_binding.rs"]
mod shared_episode_binding;
#[path = "event_sources/subtitle_source.rs"]
mod subtitle_source;

const DEVICE: &str = "source-contract-device";

struct Fixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    store: Store,
    media: MediaId,
    item: String,
    user: domain::UserId,
    token: String,
    source: domain::PhysicalSourceId,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (app, media, item) = app(tmp.path());
        let store = Store::open(tmp.path().join("data")).unwrap();
        let user = store.user_id_by_token("admin-token").unwrap().unwrap();
        let typed = domain::TypedItemId::from_wire_string(&item).unwrap();
        let file = store.playable_entries(user, &typed).unwrap().remove(0).file;
        let source = domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        };
        Self {
            _tmp: tmp,
            app,
            store,
            media,
            item,
            user,
            token: "admin-token".into(),
            source,
        }
    }

    async fn info(&self, prefix: &str) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}/Items/{}/PlaybackInfo", self.item))
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("X-Emby-Device-Id", DEVICE)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        json(response).await
    }

    fn replace_source(&self) {
        let path = self._tmp.path().join("matrix.mkv");
        let file = self
            .store
            .file_coverage_by_path(&path.display().to_string())
            .unwrap()
            .unwrap()
            .file;
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: self
                .store
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        std::fs::write(&path, b"replacement source").unwrap();
        let observation = api::fingerprint_job::observe_source(&path).unwrap();
        self.store
            .replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000)
            .unwrap();
    }

    fn nonpreferred_source(&self) -> domain::PhysicalSourceId {
        let path = self._tmp.path().join("alternate.mkv");
        std::fs::write(&path, b"alternate").unwrap();
        store::test_support::publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: self.media,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        let item = domain::TypedItemId::from_wire_string(&self.item).unwrap();
        let sources = self.store.playable_entries(self.user, &item).unwrap();
        assert_eq!(sources.len(), 2);
        let file = &sources[1].file;
        domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        }
    }

    fn publish_new_preferred_source(&self) -> domain::PhysicalSourceId {
        let path = self._tmp.path().join("new-preferred.mkv");
        std::fs::write(&path, b"new source").unwrap();
        let file = store::test_support::publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: self.media,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        let scope = store::library_write::WriteScope {
            library_id: file.library_id,
            config_revision: self
                .store
                .library_config_revision(&file.library_id.to_string())
                .unwrap(),
        };
        std::fs::write(&path, b"new source revised").unwrap();
        let observation = api::fingerprint_job::observe_source(&path).unwrap();
        self.store
            .replace_file_source(&scope, file.file_id, &file.version, &observation, 5_000)
            .unwrap();
        let item = domain::TypedItemId::from_wire_string(&self.item).unwrap();
        let entries = self.store.playable_entries(self.user, &item).unwrap();
        assert_eq!(entries[0].file.file_id, file.file_id);
        assert!(
            entries
                .iter()
                .any(|entry| entry.file.file_id == self.source.file_id
                    && entry.file.version.revision == self.source.revision)
        );
        domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: entries[0].file.version.revision,
        }
    }

    fn other_media_source(&self) -> String {
        let media = catalog::new_media(MediaKind::Movie, "Other source");
        self.store.insert_media(&media).unwrap();
        let path = self._tmp.path().join("other.mkv");
        std::fs::write(&path, b"other").unwrap();
        let file = store::test_support::publish_fixture_file(
            &self.store,
            &FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        domain::PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        }
        .to_wire_string()
    }

    async fn use_member(&mut self) {
        let created = self
            .app
            .clone()
            .oneshot(
                Request::post("/api/v1/users")
                    .header("authorization", "Bearer admin-token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"login":"source-member", "password":"member-pass"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let login = self
            .app
            .clone()
            .oneshot(
                Request::post("/Users/AuthenticateByName")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"Username":"source-member", "Pw":"member-pass"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        self.token = json(login).await["AccessToken"]
            .as_str()
            .unwrap()
            .to_owned();
        self.user = self.store.user_id_by_token(&self.token).unwrap().unwrap();
    }

    async fn event(
        &self,
        prefix: &str,
        path: &str,
        source: Option<&str>,
        sid: Option<&str>,
        ticks: i64,
    ) {
        let mut body = json!({"ItemId": self.item, "PositionTicks": ticks});
        if let Some(source) = source {
            body["MediaSourceId"] = json!(source);
        }
        if let Some(sid) = sid {
            body["PlaySessionId"] = json!(sid);
        }
        self.send_event(prefix, path, body).await;
    }

    async fn send_event(&self, prefix: &str, path: &str, body: Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::post(format!("{prefix}{path}"))
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("X-Emby-Device-Id", DEVICE)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{path}");
    }

    fn state(
        &self,
    ) -> (
        Option<store::SessionRow>,
        Option<store::UnitState>,
        Vec<store::PlayLogRow>,
    ) {
        (
            self.store.get_session(self.user, DEVICE).unwrap(),
            self.store
                .unit_state(self.user, self.media, -1, -1)
                .unwrap(),
            self.store
                .list_logs(100, None, None, None, 0, Some(self.user))
                .unwrap(),
        )
    }
}

#[tokio::test]
async fn invalid_explicit_source_events_never_modify_playback() {
    for prefix in ["", "/emby"] {
        for with_sid in [false, true] {
            let f = Fixture::new();
            f.replace_source();
            let info = f.info(prefix).await;
            let valid = info["MediaSources"][0]["Id"].as_str().unwrap();
            let sid = with_sid.then(|| info["PlaySessionId"].as_str().unwrap());
            let stale = f.source.to_wire_string();
            let forged = domain::PhysicalSourceId {
                file_id: LedgerId::new(),
                revision: 1,
            }
            .to_wire_string();
            let before = f.state();
            for source in [stale.as_str(), forged.as_str(), "invalid", " "] {
                f.event(prefix, "/Sessions/Playing", Some(source), sid, 50_000_000)
                    .await;
                assert_eq!(f.state(), before, "invalid source must not start a session");
            }
            f.event(prefix, "/Sessions/Playing", Some(valid), sid, 10_000_000)
                .await;
            let active = f.state();
            assert_eq!(active.0.as_ref().unwrap().position_ms, 1_000);
            for source in [stale.as_str(), forged.as_str(), "invalid", " "] {
                for path in [
                    "/Sessions/Playing",
                    "/Sessions/Playing/Progress",
                    "/Sessions/Playing/Stopped",
                ] {
                    f.event(prefix, path, Some(source), sid, 90_000_000).await;
                    assert_eq!(
                        f.state(),
                        active,
                        "{path} with invalid source changed playback"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn other_media_source_cannot_start_or_modify_playback() {
    for prefix in ["", "/emby"] {
        for with_sid in [false, true] {
            let f = Fixture::new();
            let other = f.other_media_source();
            let info = f.info(prefix).await;
            let valid = info["MediaSources"][0]["Id"].as_str().unwrap();
            let sid = with_sid.then(|| info["PlaySessionId"].as_str().unwrap());
            let before = f.state();
            f.event(prefix, "/Sessions/Playing", Some(&other), sid, 50_000_000)
                .await;
            assert_eq!(f.state(), before);
            f.event(prefix, "/Sessions/Playing", Some(valid), sid, 10_000_000)
                .await;
            let active = f.state();
            for path in [
                "/Sessions/Playing",
                "/Sessions/Playing/Progress",
                "/Sessions/Playing/Stopped",
            ] {
                f.event(prefix, path, Some(&other), sid, 90_000_000).await;
                assert_eq!(
                    f.state(),
                    active,
                    "{path} with other Media source changed playback"
                );
            }
        }
    }
}

#[tokio::test]
async fn explicit_nonpreferred_source_is_bound_without_a_session_id() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let requested = f.nonpreferred_source();
        let item = domain::TypedItemId::from_wire_string(&f.item).unwrap();
        let wire = requested.to_wire_string();
        f.event(prefix, "/Sessions/Playing", Some(&wire), None, 10_000_000)
            .await;
        f.event(
            prefix,
            "/Sessions/Playing/Progress",
            Some(&wire),
            None,
            30_000_000,
        )
        .await;
        let active = f.state();
        assert_eq!(
            active.0.unwrap().ledger_id,
            Some(requested.file_id.to_string())
        );
        assert_eq!(active.1.unwrap().position_ms, 3_000);
        let resolved = f
            .store
            .resolve_playback(f.user, &item, Some(&requested), None)
            .unwrap();
        assert_eq!(resolved.target.file.file_id, requested.file_id);
        assert_eq!(resolved.start_ms, 3_000);
        f.event(
            prefix,
            "/Sessions/Playing/Stopped",
            Some(&wire),
            None,
            50_000_000,
        )
        .await;
        let stopped = f.state();
        assert!(stopped.0.is_none());
        assert_eq!(stopped.1.unwrap().position_ms, 5_000);
        assert_eq!(stopped.2.len(), 1);
        assert_eq!(stopped.2[0].end_position_ms, 5_000);
    }
}

#[tokio::test]
async fn valid_other_source_cannot_change_a_bound_session() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let other = f.nonpreferred_source().to_wire_string();
        let info = f.info(prefix).await;
        let current = info["MediaSources"][0]["Id"].as_str().unwrap();
        let sid = info["PlaySessionId"].as_str().unwrap();
        assert_ne!(current, other);
        f.event(
            prefix,
            "/Sessions/Playing",
            Some(current),
            Some(sid),
            10_000_000,
        )
        .await;
        let active = f.state();
        assert_eq!(active.0.as_ref().unwrap().position_ms, 1_000);
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            f.event(prefix, path, Some(&other), Some(sid), 90_000_000)
                .await;
            assert_eq!(
                f.state(),
                active,
                "{path} with another valid source changed playback"
            );
        }
    }
}

#[tokio::test]
async fn invalid_source_does_not_activate_or_issue_a_session_binding() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        let pending = f
            .store
            .play_session_binding(f.user, DEVICE, sid)
            .unwrap()
            .unwrap();
        assert_eq!(pending.state, store::PlaySessionIdState::Pending);
        for event_sid in [None, Some(sid)] {
            for path in [
                "/Sessions/Playing",
                "/Sessions/Playing/Progress",
                "/Sessions/Playing/Stopped",
            ] {
                f.event(prefix, path, Some("invalid"), event_sid, 50_000_000)
                    .await;
                assert_eq!(
                    f.store.play_session_binding(f.user, DEVICE, sid).unwrap(),
                    Some(pending.clone())
                );
                assert_eq!(f.state(), (None, None, Vec::new()));
            }
        }
        let next = f.info(prefix).await;
        let next_sid = next["PlaySessionId"].as_str().unwrap();
        let next_binding = f
            .store
            .play_session_binding(f.user, DEVICE, next_sid)
            .unwrap()
            .unwrap();
        assert_eq!(next_binding.generation, pending.generation + 1);
    }
}

#[tokio::test]
async fn hidden_library_source_cannot_change_members_playback() {
    for prefix in ["", "/emby"] {
        let mut f = Fixture::new();
        let root = f._tmp.path().join("restricted");
        std::fs::create_dir_all(&root).unwrap();
        f.store
            .create_library(
                MediaKind::Movie,
                "Restricted",
                &[root.to_str().unwrap()],
                "selected",
                true,
                &[],
            )
            .unwrap();
        let path = root.join("hidden.mkv");
        std::fs::write(&path, b"hidden").unwrap();
        let hidden = store::test_support::publish_fixture_file(
            &f.store,
            &FixtureFile {
                media_id: f.media,
                path: path.display().to_string(),
                quality_source: QualitySource::Probe,
                confidence: Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
        let source = domain::PhysicalSourceId {
            file_id: hidden.file_id,
            revision: hidden.version.revision,
        }
        .to_wire_string();
        f.use_member().await;
        let before = f.state();
        f.event(prefix, "/Sessions/Playing", Some(&source), None, 50_000_000)
            .await;
        assert_eq!(f.state(), before, "hidden source must not start playback");
        let info = f.info(prefix).await;
        let visible = info["MediaSources"][0]["Id"].as_str().unwrap();
        assert_ne!(visible, source);
        f.event(prefix, "/Sessions/Playing", Some(visible), None, 10_000_000)
            .await;
        let active = f.state();
        assert_eq!(active.0.as_ref().unwrap().position_ms, 1_000);
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            f.event(prefix, path, Some(&source), None, 90_000_000).await;
            assert_eq!(
                f.state(),
                active,
                "{path} with hidden source changed playback"
            );
        }
    }
}

#[tokio::test]
async fn omitted_source_and_session_still_record_playback() {
    for prefix in ["", "/emby"] {
        for source in [None, Some("")] {
            let f = Fixture::new();
            for (path, ticks) in [
                ("/Sessions/Playing", 10_000_000),
                ("/Sessions/Playing/Progress", 30_000_000),
                ("/Sessions/Playing/Stopped", 50_000_000),
            ] {
                f.event(prefix, path, source, None, ticks).await;
            }
            let stopped = f.state();
            assert!(stopped.0.is_none());
            assert_eq!(stopped.1.unwrap().position_ms, 5_000);
            assert_eq!(stopped.2.len(), 1);
            assert_eq!(stopped.2[0].end_position_ms, 5_000);
        }
    }
}

#[tokio::test]
async fn null_source_and_omitted_session_still_record_playback() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        for (path, ticks) in [
            ("/Sessions/Playing", 10_000_000),
            ("/Sessions/Playing/Progress", 30_000_000),
            ("/Sessions/Playing/Stopped", 50_000_000),
        ] {
            f.send_event(
                prefix,
                path,
                json!({
                    "ItemId": f.item, "MediaSourceId": null, "PositionTicks": ticks
                }),
            )
            .await;
        }
        let stopped = f.state();
        assert!(stopped.0.is_none());
        assert_eq!(stopped.1.unwrap().position_ms, 5_000);
        assert_eq!(stopped.2.len(), 1);
        assert_eq!(stopped.2[0].end_position_ms, 5_000);
    }
}

#[tokio::test]
async fn session_events_follow_bound_source_after_preference_changes() {
    for prefix in ["", "/emby"] {
        for source_kind in ["omitted", "null", "empty"] {
            for already_active in [false, true] {
                let f = Fixture::new();
                let info = f.info(prefix).await;
                let sid = info["PlaySessionId"].as_str().unwrap();
                assert_eq!(info["MediaSources"][0]["Id"], f.source.to_wire_string());
                if already_active {
                    f.event(prefix, "/Sessions/Playing", None, Some(sid), 10_000_000)
                        .await;
                }
                let other = f.publish_new_preferred_source().to_wire_string();
                let before = f.state();
                f.event(
                    prefix,
                    "/Sessions/Playing",
                    Some(&other),
                    Some(sid),
                    90_000_000,
                )
                .await;
                assert_eq!(f.state(), before, "conflicting source must be ignored");
                for (path, ticks) in [
                    ("/Sessions/Playing", 10_000_000),
                    ("/Sessions/Playing/Progress", 30_000_000),
                    ("/Sessions/Playing/Stopped", 50_000_000),
                ] {
                    let mut body =
                        json!({"ItemId": f.item, "PlaySessionId": sid, "PositionTicks": ticks});
                    match source_kind {
                        "null" => body["MediaSourceId"] = Value::Null,
                        "empty" => body["MediaSourceId"] = json!(""),
                        _ => {}
                    }
                    f.send_event(prefix, path, body).await;
                    if path != "/Sessions/Playing/Stopped" {
                        let (active, unit, _) = f.state();
                        assert_eq!(
                            active.unwrap().ledger_id,
                            Some(f.source.file_id.to_string())
                        );
                        assert_eq!(unit.unwrap().position_ms, ticks / 10_000);
                    }
                }
                let (active, unit, logs) = f.state();
                assert!(active.is_none());
                assert_eq!(unit.unwrap().position_ms, 5_000);
                assert_eq!(logs.len(), 1);
                assert_eq!(logs[0].end_position_ms, 5_000);
                assert_eq!(
                    f.store
                        .play_session_binding(f.user, DEVICE, sid)
                        .unwrap()
                        .unwrap()
                        .state,
                    store::PlaySessionIdState::Closed
                );
            }
        }
    }
}

#[tokio::test]
async fn unspecified_event_source_does_not_revive_stale_session_source() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        let info = f.info(prefix).await;
        let sid = info["PlaySessionId"].as_str().unwrap();
        f.replace_source();
        for path in [
            "/Sessions/Playing",
            "/Sessions/Playing/Progress",
            "/Sessions/Playing/Stopped",
        ] {
            for source in ["omitted", "null", "empty"] {
                let mut body =
                    json!({"ItemId": f.item, "PlaySessionId": sid, "PositionTicks": 10_000_000});
                match source {
                    "null" => body["MediaSourceId"] = Value::Null,
                    "empty" => body["MediaSourceId"] = json!(""),
                    _ => {}
                }
                f.send_event(prefix, path, body).await;
                assert_eq!(f.state(), (None, None, Vec::new()));
            }
        }
        assert_eq!(
            f.store
                .play_session_binding(f.user, DEVICE, sid)
                .unwrap()
                .unwrap()
                .state,
            store::PlaySessionIdState::Pending
        );
    }
}
