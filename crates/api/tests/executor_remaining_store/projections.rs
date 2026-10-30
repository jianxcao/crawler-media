use super::*;

fn check_projection(f: &Fixture, method: &str, uri: String, body: Value, expected: StatusCode) {
    let app = router(f.state.clone());
    let holder = hold_store(&f.state, store::Db::Library, Duration::from_millis(500));
    let (response, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(app.oneshot(request(method, &uri, &f.token, body)), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            started.elapsed()
        })
    });
    holder.join().unwrap();
    assert_eq!(response.unwrap().status(), expected, "{method} {uri}");
    assert!(
        elapsed < Duration::from_millis(200),
        "{method} {uri} parked timer: {elapsed:?}"
    );
}

#[test]
fn favorites_reads_keep_executor_responsive() {
    for suffix in ["", "/gallery"] {
        let f = Fixture::new();
        check_projection(
            &f,
            "GET",
            format!("/api/v1/playback/favorites{suffix}"),
            Value::Null,
            StatusCode::OK,
        );
    }
}

#[test]
fn library_preview_and_missing_reads_keep_executor_responsive() {
    for suffix in [
        "missing",
        "path-reconciliation-preview",
        "root-consolidation-preview",
        "reidentify",
    ] {
        let f = Fixture::new();
        let library = f
            .state
            .store()
            .default_library(MediaKind::Movie)
            .unwrap()
            .unwrap()
            .id;
        let uri = if suffix == "reidentify" {
            format!(
                "/api/v1/libraries/{library}/items/{}/reidentification-preview",
                f.media
            )
        } else {
            format!("/api/v1/libraries/{library}/{suffix}")
        };
        check_projection(&f, "GET", uri, Value::Null, StatusCode::OK);
    }
}

#[test]
fn metadata_override_and_library_order_keep_executor_responsive() {
    let f = Fixture::new();
    check_projection(
        &f,
        "PUT",
        format!("/api/v1/media/{}/metadata-overrides", f.media),
        json!({"scope":"work","locale":"en-US","fields":{},"expected_revision":0}),
        StatusCode::OK,
    );
    let ids: Vec<_> = f
        .state
        .store()
        .list_libraries()
        .unwrap()
        .into_iter()
        .map(|l| l.id)
        .collect();
    check_projection(
        &f,
        "PUT",
        "/api/v1/libraries/order".into(),
        json!({"ids":ids}),
        StatusCode::OK,
    );
}

#[test]
fn removal_status_read_keeps_executor_responsive() {
    let f = Fixture::new();
    check_projection(
        &f,
        "GET",
        format!("/api/v1/library-removals/{}", MediaId::new()),
        Value::Null,
        StatusCode::NOT_FOUND,
    );
}

struct OfflineCatalog {
    slow: bool,
}
impl api::catalog::Catalog for OfflineCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> {
        if self.slow {
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(None)
    }
    fn genres(&self, _: MediaKind) -> Result<Vec<(i64, String)>, String> {
        if self.slow {
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(vec![])
    }
}

#[test]
fn discovery_routing_and_log_export_keep_executor_responsive() {
    for (method, uri, body) in [
        ("GET", "/api/v1/discover/movie", Value::Null),
        (
            "POST",
            "/api/v1/subscriptions/download-routing-preview",
            json!({"kind":"movie"}),
        ),
        ("GET", "/api/v1/system/logs/export", Value::Null),
    ] {
        let mut f = Fixture::new();
        f.state = f
            .state
            .with_catalog(Arc::new(OfflineCatalog { slow: false }));
        check_projection(&f, method, uri.into(), body, StatusCode::OK);
    }
}

#[test]
fn catalog_preview_and_routing_genres_do_not_run_on_executor() {
    for (method, uri, body) in [
        (
            "POST",
            "/api/v1/subscriptions/title-preview",
            json!({"title_ref":"tmdb:movie:123"}),
        ),
        ("GET", "/api/v1/libraries/routing-options", Value::Null),
    ] {
        let mut f = Fixture::new();
        f.state = f
            .state
            .with_catalog(Arc::new(OfflineCatalog { slow: true }));
        check_projection(&f, method, uri.into(), body, StatusCode::OK);
    }
}

#[test]
fn retired_root_writer_is_unavailable() {
    let f = Fixture::new();
    let library = f
        .state
        .store()
        .default_library(MediaKind::Movie)
        .unwrap()
        .unwrap()
        .id;
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request(
            "POST",
            &format!("/api/v1/libraries/{library}/root-consolidations"),
            &f.token,
            json!({"roots":[]}),
        )))
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn metadata_refresh_and_extras_keep_executor_responsive() {
    for refresh in [true, false] {
        let f = Fixture::new();
        let (uri, body) = {
            let handle = f.state.store();
            let store = handle.as_ref();
            let library = store.default_library(MediaKind::Movie).unwrap().unwrap();
            if refresh {
                (
                    format!("/api/v1/libraries/{}/metadata/refresh", library.id),
                    json!({}),
                )
            } else {
                let file = store
                    .file_coverage_for_media(f.media)
                    .unwrap()
                    .remove(0)
                    .file
                    .file_id;
                (
                    "/api/v1/reidentify/extras".into(),
                    json!({"file_ids":[file]}),
                )
            }
        };
        check_projection(&f, "POST", uri, body, StatusCode::OK);
    }
}

#[test]
fn anonymous_person_image_lookup_does_not_park_executor() {
    let f = Fixture::new();
    let provider = api::media_server_provider::ApiServerProvider::new(f.state.clone());
    let holder = hold_store(&f.state, store::Db::Library, Duration::from_millis(500));
    use media_server::provider::MediaServerProvider;
    let (result, elapsed) = runtime().block_on(async {
        let started = Instant::now();
        tokio::join!(
            provider.resolve_person_image_url(None, "absent-person"),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    holder.join().unwrap();
    assert_eq!(result.unwrap(), None);
    assert!(
        elapsed < Duration::from_millis(200),
        "person image lookup parked timer: {elapsed:?}"
    );
}
