use super::*;

#[test]
fn filter_routes_keep_executor_responsive() {
    for route in 0..6 {
        let f = Fixture::new();
        let filter = domain::Filter {
            id: domain::FilterId::new(),
            name: "Original".into(),
            keep_old_versions: false,
            atoms: vec![domain::FilterAtom {
                priority: 10,
                exclude: false,
                rule: domain::AtomRule::Resolution("1080p".into()),
            }],
        };
        f.state.store().insert_filter(&filter).unwrap();
        let body = json!({"name":"Edited", "atoms":[{"kind":"hdr","value":"HDR10","priority":10,"exclude":false}]});
        let uri = format!("/api/v1/rule-sets/{}", filter.id);
        let (method, uri, body, status) = match route {
            0 => (
                "GET",
                "/api/v1/rule-sets".to_string(),
                Value::Null,
                StatusCode::OK,
            ),
            1 => ("GET", uri, Value::Null, StatusCode::OK),
            2 => (
                "POST",
                "/api/v1/rule-sets".into(),
                body,
                StatusCode::CREATED,
            ),
            3 => ("PATCH", uri, body, StatusCode::OK),
            4 => ("DELETE", uri, Value::Null, StatusCode::OK),
            _ => (
                "PUT",
                "/api/v1/rule-sets/default".into(),
                json!({"id":filter.id}),
                StatusCode::OK,
            ),
        };
        let app = router(f.state.clone());
        let holder = hold_store(&f.state, store::Db::App, Duration::from_millis(500));
        let (response, elapsed) = runtime().block_on(async {
            let started = Instant::now();
            tokio::join!(app.oneshot(request(method, &uri, &f.token, body)), async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                started.elapsed()
            })
        });
        holder.join().unwrap();
        assert_eq!(response.unwrap().status(), status, "{method} {uri}");
        assert!(
            elapsed < Duration::from_millis(200),
            "{method} {uri} parked timer: {elapsed:?}"
        );
    }
}
