use super::*;

#[tokio::test]
async fn failed_user_removal_rejects_updates_and_login_until_retry_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(&tmp);
    let (member, _) = create_owned_subscribe(&state).await;
    let store = state.store();
    store.with_writer(store::Db::Subscribe, |conn| {
        conn.execute_batch("CREATE TRIGGER reject_user_transfer BEFORE UPDATE OF user_id ON subscribes BEGIN SELECT RAISE(ABORT, 'cleanup unavailable'); END;")?;
        Ok(())
    }).unwrap();
    let service = api::user_removal::UserRemovalService::new(Arc::new(state.clone()));
    let accepted = service.accept(member.id).unwrap();
    assert!(service.continue_removal(member.id).is_err());
    let failed = api::user_removal::find_removal(&store, &accepted.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(api::user_removal::status_dto(&failed)["state"], "failed");
    let app = router(state);
    for body in [
        json!({"enabled": true}),
        json!({"login": "changed"}),
        json!({"password": "changed-password"}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/users/{}", member.id))
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer admin-token")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
    }
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "member", "password": "member-password"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(!store.get_user(member.id).unwrap().unwrap().enabled);
    assert!(store.user_by_token("member-token").unwrap().is_none());
    assert_eq!(
        service.accept(member.id).unwrap().operation_id,
        accepted.operation_id
    );
    store
        .with_writer(store::Db::Subscribe, |conn| {
            conn.execute_batch("DROP TRIGGER reject_user_transfer;")?;
            Ok(())
        })
        .unwrap();
    service.continue_removal(member.id).unwrap();
    assert!(store.get_user(member.id).unwrap().is_none());
    let done = api::user_removal::find_removal(&store, &accepted.operation_id)
        .unwrap()
        .unwrap();
    assert_eq!(api::user_removal::status_dto(&done)["state"], "succeeded");
}

#[tokio::test]
async fn user_removal_acceptance_rolls_back_when_token_revocation_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let state = state(&tmp);
    let (member, _) = create_owned_subscribe(&state).await;
    let store = state.store();
    store.with_writer(store::Db::App, |conn| {
        conn.execute_batch("CREATE TRIGGER reject_token_revocation BEFORE DELETE ON user_tokens BEGIN SELECT RAISE(ABORT, 'token revocation unavailable'); END;")?;
        Ok(())
    }).unwrap();
    let service = api::user_removal::UserRemovalService::new(Arc::new(state));
    assert!(service.accept(member.id).is_err());
    assert!(
        api::user_removal::pending_removal_of(&store, member.id)
            .unwrap()
            .is_none()
    );
    assert!(store.get_user(member.id).unwrap().unwrap().enabled);
    assert!(store.user_by_token("member-token").unwrap().is_some());
    store
        .with_writer(store::Db::App, |conn| {
            conn.execute_batch("DROP TRIGGER reject_token_revocation;")?;
            Ok(())
        })
        .unwrap();
    service.accept(member.id).unwrap();
    assert!(!store.get_user(member.id).unwrap().unwrap().enabled);
    assert!(store.user_by_token("member-token").unwrap().is_none());
}
