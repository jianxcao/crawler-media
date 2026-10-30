use super::*;

#[test]
fn collection_write_times_out_without_late_mutation() {
    let f = Fixture::new();
    let app = router(f.state.clone());
    let holder = hold_store(&f.state, store::Db::Library, Duration::from_millis(1500));
    let response = runtime()
        .block_on(app.oneshot(request(
            "PATCH",
            &f.uri(""),
            &f.token,
            json!({"name":"Late"}),
        )))
        .unwrap();
    holder.join().unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    let store = f.state.store();
    let guard = store.as_ref();
    assert_eq!(guard.list_collections(f.user).unwrap()[0].name, "Original");
}

#[test]
fn collection_write_rechecks_readonly_after_waiting_for_store() {
    let f = Fixture::new();
    let app = router(f.state.clone());
    let store = f.state.store();
    let state = f.state.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let _ = state.with_instance_writable(false);
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let response = runtime()
        .block_on(app.oneshot(request(
            "PATCH",
            &f.uri(""),
            &f.token,
            json!({"name":"Late"}),
        )))
        .unwrap();
    holder.join().unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        f.state.store().list_collections(f.user).unwrap()[0].name,
        "Original"
    );
}

#[test]
fn collection_write_rechecks_maintenance_after_waiting_for_store() {
    let f = Fixture::new();
    let app = router(f.state.clone());
    let store = f.state.store();
    let gate = f.state.write_gate();
    let (tx, rx) = std::sync::mpsc::channel();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let guard = crate::writer_hold::WriterHold::acquire(store.clone(), store::Db::Library);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let _closed = gate.close();
        drop(guard);
        closed_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let response = runtime()
        .block_on(app.oneshot(request(
            "PATCH",
            &f.uri(""),
            &f.token,
            json!({"name":"Late"}),
        )))
        .unwrap();
    closed_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        f.state.store().list_collections(f.user).unwrap()[0].name,
        "Original"
    );
}

#[test]
fn collection_ownership_and_current_response_remain_intact() {
    let f = Fixture::new();
    let other = domain::User {
        id: domain::UserId::new(),
        login: "other".into(),
        enabled: true,
        role: UserRole::Member,
    };
    {
        let store = f.state.store();
        let guard = store.as_ref();
        guard
            .insert_user_with_password(&other, "other-pass")
            .unwrap();
        guard.set_user_token(other.id, "other-token").unwrap();
    }
    runtime().block_on(async {
        let app = router(f.state.clone());
        for (method, uri, body) in [
            ("GET", f.uri("/items"), Value::Null),
            ("GET", f.uri("/gallery"), Value::Null),
            ("PATCH", f.uri(""), json!({"name":"Forbidden"})),
            ("DELETE", f.uri(""), Value::Null),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, &uri, "other-token", body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        let response = app
            .oneshot(request("GET", &f.uri("/items"), &f.token, Value::Null))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_response(response).await;
        assert_eq!(body["data"][0]["media_item_id"], f.media.to_string());
        assert!(
            body["data"][0]["poster_url"]
                .as_str()
                .unwrap()
                .starts_with("/posters/")
        );
    });
    assert_eq!(
        f.state.store().list_collections(f.user).unwrap()[0].name,
        "Original"
    );
}
