use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
pub const BASE: i64 = 1_800_000_000_125;
pub struct Clock(pub AtomicI64);
impl api::playback_events::PlaybackClock for Clock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}
pub struct Env {
    pub fixture: PresenceFixture,
    pub app: axum::Router,
    pub clock: Arc<Clock>,
    pub user: domain::UserId,
    pub playing: domain::LibraryFile,
    pub media: domain::MediaId,
    pub scope: store::library_write::WriteScope,
    pub root_path: std::path::PathBuf,
    pub templates: Vec<domain::LibraryFile>,
}
impl Env {
    pub fn new() -> Self {
        let fixture = PresenceFixture::new();
        let (media, mut playing) = fixture.insert_media_and_ledger("playing.mkv");
        let clock = Arc::new(Clock(AtomicI64::new(BASE)));
        let state = fixture.state.clone().with_playback_clock(clock.clone());
        let store = state.store();
        playing.duration_ms = Some(3_600_000);
        store
            .publish_files(
                &store::library_write::WriteScope {
                    library_id: playing.library_id,
                    config_revision: 0,
                },
                &[publication(&playing, &[domain::UnitKey::WHOLE])],
                BASE,
            )
            .unwrap();
        let user = store
            .user_by_token(&fixture.admin_token)
            .unwrap()
            .unwrap()
            .id;
        let root_path = fixture.dir.path().join("workload");
        let lib = store
            .create_library(
                domain::MediaKind::Tv,
                "L1",
                &[root_path.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|r| r.library_id == lib.id)
            .unwrap()
            .id;
        let scope = store::library_write::WriteScope {
            library_id: lib.id.parse().unwrap(),
            config_revision: 0,
        };
        let mut templates = Vec::new();
        for n in 0..100 {
            let mut work = store.get_media(media).unwrap().unwrap();
            work.id = domain::MediaId::new();
            work.kind = domain::MediaKind::Tv;
            work.title = format!("Workload {n}");
            store.insert_media(&work).unwrap();
            let mut file = playing.clone();
            file.library_id = scope.library_id;
            file.root_id = root.clone();
            file.media_id = work.id;
            file.kind = work.kind;
            templates.push(file);
        }
        let app = api::router(state);
        Self {
            fixture,
            app,
            clock,
            user,
            playing,
            media,
            scope,
            root_path,
            templates,
        }
    }
    pub fn readonly(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.fixture.dir.path().join("data/library.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    pub async fn event(
        &self,
        protocol: bool,
        session: &str,
        event: &str,
        sequence: i64,
        position: i64,
    ) -> (u16, f64, media_server::query_metrics::QueryMetricsSnapshot) {
        let (path, body) = if protocol {
            let path = match event {
                "start" => "/Sessions/Playing",
                "stop" => "/Sessions/Playing/Stopped",
                _ => "/Sessions/Playing/Progress",
            };
            (
                path,
                json!({"ItemId":domain::TypedItemId::Work(self.media).to_wire_string(),"PlaySessionId":session,"EventSequence":sequence,"PositionTicks":position*10_000}),
            )
        } else {
            (
                "/api/v1/playback/progress",
                json!({"media_item_id":self.media,"play_session_id":session,"event_sequence":sequence,"device_id":"l1-player","event":event,"position_ms":position,"duration_ms":3_600_000}),
            )
        };
        let timing = media_server::provider::QueryTiming::start();
        let metrics = media_server::query_metrics::QueryMetrics::new(timing, true);
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header(
                "Authorization",
                format!("Bearer {}", self.fixture.admin_token),
            )
            .header("Content-Type", "application/json")
            .header("X-Emby-Device-Id", "l1-player")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(timing);
        request.extensions_mut().insert(metrics.clone());
        let start = Instant::now();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        let code = response.status().as_u16();
        if code == 503 {
            eprintln!(
                "L1 503 retry_after={:?}",
                response.headers().get("Retry-After")
            );
        }
        assert_eq!(
            code,
            if protocol { 204 } else { 200 },
            "heartbeat persistence HTTP error: {}",
            String::from_utf8_lossy(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
            )
        );
        (code, elapsed, metrics.snapshot())
    }
    pub fn assert_progress(&self, session: &str, sequence: i64, position: i64, now: i64) {
        let db = self.readonly();
        let row:(i64,i64,String,String,i64,String,i64)=db.query_row("SELECT position_ms,last_report_at_ms,play_session_id,file_id,file_revision,source_version,last_event_sequence FROM playback_sessions WHERE user_id=?1 AND device_id='l1-player'",[self.user.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
        assert_eq!(
            row,
            (
                position,
                now,
                session.into(),
                self.playing.file_id.to_string(),
                self.playing.version.revision,
                self.playing.version.source_version.clone(),
                sequence
            )
        );
        let unit:(i64,String,i64,i64)=db.query_row("SELECT position_ms,resume_file_id,favorite,play_count FROM playback_units WHERE user_id=?1 AND media_id=?2",rusqlite::params![self.user.to_string(),self.media.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(unit, (position, self.playing.file_id.to_string(), 1, 0));
    }
}
pub fn publication<'a>(
    file: &'a domain::LibraryFile,
    units: &'a [domain::UnitKey],
) -> store::library_write::FilePublication<'a> {
    store::library_write::FilePublication {
        file,
        units,
        expected_version: None,
        nfo_observation: None,
        probe_required: false,
    }
}
