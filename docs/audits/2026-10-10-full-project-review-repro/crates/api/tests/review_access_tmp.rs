use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("unused".into()))
    }
}

fn app(root: &Path) -> (axum::Router, MediaId, String) {
    let store = Store::open(root.join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    let file = root.join("matrix.mkv");
    std::fs::write(&file, b"0123456789abcdef").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: Some("hevc".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(100),
    };
    store.insert_ledger(&row).unwrap();
    let compact = row.id.to_string().replace('-', "");
    let router = router(
        ApiState::new(
            store,
            "admin-token".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(root.join("stage"))),
            root.join("library"),
        )
        .unwrap(),
    );
    (router, media.id, compact)
}

async fn json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}


fn admin() -> domain::UserId { domain::UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap() }
async fn send(app: &axum::Router, method: &str, uri: &str, auth: &str, body: Value, device: Option<&str>) -> axum::response::Response {
    let mut b=Request::builder().method(method).uri(uri).header("authorization", auth).header("content-type", "application/json");
    if let Some(id)=device { b=b.header("X-Emby-Device-Id",id).header("X-Emby-Device-Name","Bedroom Apple TV").header("X-Emby-Client","Infuse"); }
    app.clone().oneshot(b.body(Body::from(body.to_string())).unwrap()).await.unwrap()
}
#[tokio::test]
async fn review_access_tmp_complete_jellyfin_stays_unplayed() {
    let tmp=tempfile::tempdir().unwrap(); let (app,media,id)=app(tmp.path());
    let store=Store::open(tmp.path().join("data")).unwrap();
    store.upsert_unit(admin(),media,-1,-1,0,None,None,Some(120000),None,None,false,1).unwrap();
    for (route,ticks) in [("/Sessions/Playing",0),("/Sessions/Playing/Progress",1200000000),("/Sessions/Playing/Stopped",1200000000)] {
        assert_eq!(send(&app,"POST",route,"Bearer admin-token",json!({"ItemId":id,"PositionTicks":ticks}),Some("actual-device")).await.status(),StatusCode::NO_CONTENT);
    }
    let state=store.unit_state(admin(),media,-1,-1).unwrap().unwrap();
    assert!(!state.played); assert_eq!(state.position_ms,120000);
    let result=json(send(&app,"GET","/Users/Me/Items/Resume","Bearer admin-token",json!({}),None).await).await;
    assert_eq!(result["TotalRecordCount"],1);
}
#[tokio::test]
async fn review_access_tmp_standard_authorization_collides() {
    let tmp=tempfile::tempdir().unwrap(); let (app,_,id)=app(tmp.path());
    for device in ["bedroom", "living-room"] {
        let auth=format!("MediaBrowser Token=\"admin-token\", DeviceId=\"{device}\", Client=\"Infuse\"");
        assert_eq!(send(&app,"POST","/Sessions/Playing",&auth,json!({"ItemId":id,"PositionTicks":0}),None).await.status(),StatusCode::NO_CONTENT);
    }
    let store=Store::open(tmp.path().join("data")).unwrap();
    let sessions=store.active_sessions(i64::MAX/2,i64::MAX/2).unwrap();
    assert_eq!(sessions.len(),1); assert_eq!(sessions[0].device_id,"jellyfin-client");
    assert!(store.get_session(admin(),"bedroom").unwrap().is_none());
}
#[tokio::test]
async fn review_access_tmp_idle_revoke_is_ineffective() {
    let tmp=tempfile::tempdir().unwrap(); let (app,_,id)=app(tmp.path());
    for route in ["/Sessions/Playing", "/Sessions/Playing/Stopped"] {
        assert_eq!(send(&app,"POST",route,"Bearer admin-token",json!({"ItemId":id,"PositionTicks":10000000}),Some("actual-device")).await.status(),StatusCode::NO_CONTENT);
    }
    let body=json(send(&app,"GET","/api/v1/playback/devices","Bearer admin-token",json!({}),None).await).await;
    assert_eq!(body["data"][0]["device_id"],"jf-Bedroom Apple TV");
    let revoke=format!("/api/v1/playback/devices/jf-Bedroom%20Apple%20TV?user_id={}",admin());
    assert_eq!(send(&app,"DELETE",&revoke,"Bearer admin-token",json!({}),None).await.status(),StatusCode::OK);
    let stream=format!("/Videos/{id}/stream");
    assert_eq!(send(&app,"GET",&stream,"Bearer admin-token",json!({}),Some("actual-device")).await.status(),StatusCode::OK);
}
#[tokio::test]
async fn review_access_tmp_web_writes_fail_but_success() {
    let tmp=tempfile::tempdir().unwrap(); let (app,media,_)=app(tmp.path());
    let db=rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER review_access_tmp_block BEFORE INSERT ON playback_units BEGIN SELECT RAISE(ABORT,'review write blocked'); END;").unwrap();
    let response=send(&app,"POST","/api/v1/playback/progress","Bearer admin-token",json!({"media_item_id":media.to_string(),"event":"start","device_id":"browser","position_ms":30000}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    assert_eq!(json(response).await["data"]["position_ms"],30000);
    let store=Store::open(tmp.path().join("data")).unwrap();
    assert!(store.unit_state(admin(),media,-1,-1).unwrap().is_none());
}
#[tokio::test]
async fn review_access_tmp_history_zero_limit_panics() {
    let tmp=tempfile::tempdir().unwrap(); let (app,_,id)=app(tmp.path());
    for route in ["/Sessions/Playing", "/Sessions/Playing/Stopped"] { send(&app,"POST",route,"Bearer admin-token",json!({"ItemId":id,"PositionTicks":10000000}),Some("actual-device")).await; }
    let handle=tokio::spawn(async move {send(&app,"GET","/api/v1/playback/history?limit=0","Bearer admin-token",json!({}),None).await});
    assert!(handle.await.unwrap_err().is_panic());
}

#[tokio::test]
async fn review_access_tmp_browser_revoke_does_not_revoke_stream() {
    let tmp=tempfile::tempdir().unwrap(); let (app,media,id)=app(tmp.path());
    let body=json!({"media_item_id":media.to_string(),"event":"start","device_id":"browser","position_ms":30000});
    assert_eq!(send(&app,"POST","/api/v1/playback/progress","Bearer admin-token",body.clone(),None).await.status(),StatusCode::OK);
    let revoke=format!("/api/v1/playback/devices/browser?user_id={}",admin());
    assert_eq!(send(&app,"DELETE",&revoke,"Bearer admin-token",json!({}),None).await.status(),StatusCode::OK);
    assert_eq!(send(&app,"POST","/api/v1/playback/progress","Bearer admin-token",body,None).await.status(),StatusCode::FORBIDDEN);
    assert_eq!(send(&app,"GET",&format!("/Videos/{id}/stream?api_key=admin-token"),"Bearer admin-token",json!({}),None).await.status(),StatusCode::OK);
}
#[tokio::test]
async fn review_access_tmp_metrics_frontend_shape_dropped() {
    let tmp=tempfile::tempdir().unwrap(); let (app,_,id)=app(tmp.path());
    let response=send(&app,"POST","/api/v1/playback/metrics","Bearer admin-token",json!({"library_file_id":id,"tier":0,"engine":"native","watched_ms":60000}),None).await;
    assert_eq!(response.status(),StatusCode::OK);
    let db=rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM playback_metrics",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
#[tokio::test]
async fn review_access_tmp_clear_history_erases_favorites() {
    let tmp=tempfile::tempdir().unwrap(); let (app,media,_)=app(tmp.path());
    let store=Store::open(tmp.path().join("data")).unwrap();
    store.upsert_unit(admin(),media,-1,-1,1000,None,Some(true),None,None,None,false,1).unwrap();
    assert_eq!(send(&app,"DELETE","/api/v1/playback/history?scope=all","Bearer admin-token",json!({}),None).await.status(),StatusCode::OK);
    let body=json(send(&app,"GET","/api/v1/playback/favorites","Bearer admin-token",json!({}),None).await).await;
    assert_eq!(body["data"]["total"],0);
}

#[tokio::test]
async fn review_access_tmp_jellyfin_shadows_legacy_movie_marks() {
    let tmp=tempfile::tempdir().unwrap(); let (app,media,id)=app(tmp.path());
    let store=Store::open(tmp.path().join("data")).unwrap();
    store.upsert_unit(admin(),media,0,0,45000,Some(true),Some(true),Some(120000),Some("embedded:1"),Some("off"),true,1).unwrap();
    assert_eq!(send(&app,"POST","/Sessions/Playing","Bearer admin-token",json!({"ItemId":id,"PositionTicks":450000000}),Some("actual-device")).await.status(),StatusCode::NO_CONTENT);
    let state=store.unit_state(admin(),media,-1,-1).unwrap().unwrap();
    assert!(!state.played); assert!(!state.favorite); assert_eq!(state.duration_ms,None); assert_eq!(state.audio_track,None);
    let body=json(send(&app,"GET",&format!("/api/v1/playback/resume?media_item_id={media}"),"Bearer admin-token",json!({}),None).await).await;
    assert_eq!(body["data"]["played"],false); assert_eq!(body["data"]["is_favorite"],false);
}
