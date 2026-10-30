use super::*;

#[test]
fn metadata_and_browser_settings_keep_executor_responsive() {
    for (method, uri, body, expected) in [
        (
            "GET",
            "/api/v1/settings/metadata",
            Value::Null,
            StatusCode::OK,
        ),
        (
            "PUT",
            "/api/v1/settings/metadata",
            json!({}),
            StatusCode::OK,
        ),
        (
            "POST",
            "/api/v1/settings/metadata/test",
            Value::Null,
            StatusCode::BAD_REQUEST,
        ),
        (
            "GET",
            "/api/v1/settings/browser",
            Value::Null,
            StatusCode::OK,
        ),
        (
            "PUT",
            "/api/v1/settings/browser",
            json!({"cdp":{"enabled":false}}),
            StatusCode::OK,
        ),
        (
            "POST",
            "/api/v1/settings/browser/sync-cdp",
            Value::Null,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let f = Fixture::new();
        f.state.store().put_setting("tmdb.api_key", "").unwrap();
        let app = router(f.state.clone());
        let holder = hold_store(&f.state, store::Db::App, Duration::from_millis(500));
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, uri, &f.token, body)), async {
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
}

#[test]
fn notify_proxy_and_scrape_settings_keep_executor_responsive() {
    for (method, uri, body, expected) in [
        ("GET", "/api/v1/notify/config", Value::Null, StatusCode::OK),
        ("PUT", "/api/v1/notify/config", json!({}), StatusCode::OK),
        (
            "POST",
            "/api/v1/notify/test",
            json!({}),
            StatusCode::BAD_REQUEST,
        ),
        ("GET", "/api/v1/settings/proxy", Value::Null, StatusCode::OK),
        ("PUT", "/api/v1/settings/proxy", json!({}), StatusCode::OK),
        (
            "GET",
            "/api/v1/settings/scrape",
            Value::Null,
            StatusCode::OK,
        ),
        (
            "PUT",
            "/api/v1/settings/scrape",
            json!({"setting":{}}),
            StatusCode::OK,
        ),
        (
            "POST",
            "/api/v1/settings/scrape/preview-naming",
            Value::Null,
            StatusCode::OK,
        ),
    ] {
        let f = Fixture::new();
        let app = router(f.state.clone());
        let holder = hold_store(&f.state, store::Db::App, Duration::from_millis(500));
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, uri, &f.token, body)), async {
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
}

#[test]
fn proxy_diagnosis_reads_off_executor_and_obeys_capacity_without_network() {
    let f = Fixture::new();
    let budget = Arc::new(api::blocking_budget::BlockingBudget::new(1));
    let reservation = budget.try_reserve().unwrap();
    let state = f.state.clone().with_http_work(budget);
    let holder = hold_store(&state, store::Db::App, Duration::from_millis(500));
    let (response, elapsed) = runtime().block_on(async {
        let app = router(state);
        let started = Instant::now();
        tokio::join!(
            app.oneshot(request(
                "POST",
                "/api/v1/settings/proxy/diagnose",
                &f.token,
                Value::Null,
            )),
            async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            }
        )
    });
    holder.join().unwrap();
    drop(reservation);
    let response = response.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = runtime().block_on(json_response(response));
    let items = body["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert!(items.iter().all(|item| item["ok"] == false));
    assert!(items.iter().all(|item| {
        item["error"]
            .as_str()
            .unwrap()
            .contains("capacity exhausted")
    }));
    assert!(elapsed < Duration::from_millis(200));
}

#[test]
fn metadata_key_hint_handles_unicode_without_panicking() {
    let f = Fixture::new();
    f.state
        .store()
        .put_setting("tmdb.api_key", "密钥测试")
        .unwrap();
    let response = runtime()
        .block_on(router(f.state.clone()).oneshot(request(
            "GET",
            "/api/v1/settings/metadata",
            &f.token,
            Value::Null,
        )))
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = runtime().block_on(json_response(response));
    assert_eq!(body["data"]["tmdb"]["api_key_hint"], "已配置");
}
