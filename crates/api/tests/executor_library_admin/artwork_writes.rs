use super::*;

fn json_request(method: &str, uri: &str, token: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[test]
fn artwork_mutations_require_a_filesystem_worker_before_writing() {
    for upload in [false, true] {
        let f = Fixture::new();
        let source = f._dir.path().join("candidate.jpg");
        std::fs::write(&source, b"candidate").unwrap();
        let target = f.file.path.parent().unwrap().join("poster.jpg");
        let reservations = (
            f.state.fs_work.try_reserve().unwrap(),
            f.state.fs_work.try_reserve().unwrap(),
        );
        let (action, body) = if upload {
            (
                "upload",
                serde_json::json!({"data_url":"data:image/jpeg;base64,Y2FuZGlkYXRl"}),
            )
        } else {
            (
                "select",
                serde_json::json!({"file_path":source.display().to_string()}),
            )
        };
        let uri = format!(
            "/api/v1/libraries/{}/items/{}/artwork/{action}",
            f.file.library_id, f.file.media_id
        );
        let response = runtime()
            .block_on(router(f.state.clone()).oneshot(json_request("POST", &uri, &f.token, body)))
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{action}"
        );
        assert!(!target.exists());
        assert!(
            f.state
                .store()
                .lock()
                .get_setting(&format!("artwork.{}", f.file.media_id))
                .unwrap()
                .is_none()
        );
        drop(reservations);
    }
}

struct ObservedFiles(Arc<parking_lot::Mutex<Store>>);
impl api::file_stat_read::FileStatRead for ObservedFiles {
    fn read_nfo(&self, _: &std::path::Path) -> Option<library::NfoMeta> {
        None
    }
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
    fn is_file(&self, path: &std::path::Path) -> bool {
        assert!(
            self.0.try_lock_for(Duration::from_millis(100)).is_some(),
            "artwork observation held Store across filesystem work"
        );
        std::thread::sleep(Duration::from_millis(10));
        path.is_file()
    }
}

#[test]
fn uploaded_artwork_observation_allows_other_store_reads_and_records_its_file() {
    let mut f = Fixture::new();
    f.state = f
        .state
        .clone()
        .with_file_stat_read(Arc::new(ObservedFiles(f.state.store())));
    let uri = format!(
        "/api/v1/libraries/{}/items/{}/artwork/upload",
        f.file.library_id, f.file.media_id
    );
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(json_request(
            "POST",
            &uri,
            &f.token,
            serde_json::json!({"data_url":"data:image/jpeg;base64,Y2FuZGlkYXRl"}),
        )))
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let handle = f.state.store();
    let store = handle.lock();
    let file = store.file_coverage_by_id(f.file.file_id).unwrap().unwrap();
    assert!(file.file.poster_path.is_some());
    assert_eq!(
        std::fs::read(file.file.poster_path.unwrap()).unwrap(),
        b"candidate"
    );
}
