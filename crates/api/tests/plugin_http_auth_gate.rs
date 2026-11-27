#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::http::{StatusCode, header};
use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, pkg::build_task_package, req_json,
};
use serde_json::json;

#[tokio::test]
async fn unauthenticated_requests_return_401_across_plugin_routes() {
    let env = HttpEnv::new().await;
    let routes = [
        ("GET", "/api/v1/plugins", None),
        ("GET", "/api/v1/plugin-trust-keys", None),
        ("GET", "/api/v1/settings/plugins", None),
        (
            "POST",
            "/api/v1/plugin-reauth",
            Some(json!({ "password": ADMIN_PASS, "purpose": "plugin:upload" })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/config/validate",
            Some(json!({ "schema_revision": "1", "values": {} })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/enable",
            Some(json!({})),
        ),
        (
            "PUT",
            "/api/v1/plugins/org.example.p/config",
            Some(json!({ "schema_revision": "1", "values": {} })),
        ),
    ];
    for (method, path, body) in routes {
        let (st, _, resp) = env.send(req_json(method, path, None, &[], body)).await;
        assert_err(st, &resp, StatusCode::UNAUTHORIZED, "auth.required");
    }
}

#[tokio::test]
async fn member_requests_return_403_across_plugin_routes() {
    let env = HttpEnv::new().await;
    let op_id = uuid::Uuid::new_v4();
    let member = Some(env.member_token.as_str());
    let routes = [
        ("GET", "/api/v1/plugins", None),
        ("GET", "/api/v1/plugins/org.example.p", None),
        ("GET", "/api/v1/plugin-trust-keys", None),
        ("GET", "/api/v1/settings/plugins", None),
        ("GET", &format!("/api/v1/plugin-operations/{op_id}"), None),
        (
            "POST",
            "/api/v1/plugin-reauth",
            Some(json!({ "password": ADMIN_PASS, "purpose": "plugin:upload" })),
        ),
        (
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            Some(json!({ "public_key": "k", "label": "l" })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/config/validate",
            Some(json!({ "schema_revision": "1", "values": {} })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/permissions/prepare",
            Some(json!({ "requested_grants": [] })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/state/reset/prepare",
            Some(json!({ "security_scopes": [] })),
        ),
        (
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(json!({ "upload_id": op_id })),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.p/enable",
            Some(json!({})),
        ),
    ];
    for (method, path, body) in routes {
        let (st, _, resp) = env.send(req_json(method, path, member, &[], body)).await;
        assert_err(st, &resp, StatusCode::FORBIDDEN, "auth.forbidden");
    }
}

#[tokio::test]
async fn write_gate_read_only_allows_gets_and_five_readonly_posts() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.gate", "1.0.0", 1, false);
    env.install_pkg(&pkg, None, json!([])).await;
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.gate/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let _ro = env.state.clone().with_instance_writable(false);
    let readonly_posts = [
        (
            "/api/v1/plugin-reauth",
            json!({ "password": ADMIN_PASS, "purpose": "plugin:upload" }),
        ),
        (
            "/api/v1/plugin-trust-keys/prepare",
            json!({ "public_key": plugin_http_support::pkg::TestSigningKey::generate(7).public_key_str, "label": "k" }),
        ),
        (
            "/api/v1/plugins/org.example.gate/config/validate",
            json!({ "schema_revision": schema_rev, "values": { "mode": "ok" } }),
        ),
        (
            "/api/v1/plugins/org.example.gate/permissions/prepare",
            json!({ "requested_grants": [] }),
        ),
        (
            "/api/v1/plugins/org.example.gate/state/reset/prepare",
            json!({ "security_scopes": [] }),
        ),
    ];
    for (path, body) in readonly_posts {
        let (st, _, resp) = env
            .send(req_json("POST", path, Some(TOKEN), &[], Some(body)))
            .await;
        assert_ok(st, &resp, StatusCode::OK);
    }
    for path in [
        "/api/v1/plugins",
        "/api/v1/plugins/org.example.gate",
        "/api/v1/plugins/org.example.gate/config",
        "/api/v1/settings/plugins",
        "/api/v1/plugin-trust-keys",
    ] {
        let (st, _, resp) = env
            .send(req_json("GET", path, Some(TOKEN), &[], None))
            .await;
        assert_eq!(st, StatusCode::OK, "GET {path} failed: {resp}");
    }
    let _rw = env.state.clone().with_instance_writable(true);
}

#[tokio::test]
async fn write_gate_read_only_blocks_mutating_routes_including_prepares() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.gatew", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let if_match = format!("\"{}\"", detail["revision"].as_i64().unwrap());
    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.gatew/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let _ro = env.state.clone().with_instance_writable(false);
    let blocked_writes = [
        (
            "POST",
            "/api/v1/plugin-installations/prepare",
            "\"0\"",
            json!({ "upload_id": uuid::Uuid::new_v4() }),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.gatew/upgrades/prepare",
            &if_match,
            json!({
                "version": "1.0.1", "upload_id": uuid::Uuid::new_v4(),
                "target_config": { "schema_revision": schema_rev, "values": { "mode": "ok" } }, "state_policy": "preserve"
            }),
        ),
        (
            "POST",
            "/api/v1/plugins/org.example.gatew/enable",
            &if_match,
            json!({}),
        ),
        (
            "PUT",
            "/api/v1/plugins/org.example.gatew/config",
            &if_match,
            json!({ "schema_revision": schema_rev, "values": { "mode": "ok" } }),
        ),
    ];
    for (method, path, em, body) in blocked_writes {
        let (st, _, resp) = env
            .send(req_json(
                method,
                path,
                Some(TOKEN),
                &[("if-match", em), ("idempotency-key", "ro-check")],
                Some(body),
            ))
            .await;
        assert_err(st, &resp, StatusCode::CONFLICT, "instance.read_only");
    }
    let _rw = env.state.clone().with_instance_writable(true);
}

#[tokio::test]
async fn write_gate_maintenance_busy_returns_503_retry_after_on_writes() {
    let env = HttpEnv::new().await;
    let gate = env.state.write_gate();
    let _closed = gate.close();

    // Readonly POST still succeeds during maintenance.
    let (st_r, _, b_r) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-reauth",
            Some(TOKEN),
            &[],
            Some(json!({ "password": ADMIN_PASS, "purpose": "plugin:upload" })),
        ))
        .await;
    assert_ok(st_r, &b_r, StatusCode::OK);

    // Write-gated prepare/mutation routes return 503 runtime.busy + Retry-After: 1.
    for (path, body) in [
        (
            "/api/v1/plugin-installations/prepare",
            json!({ "upload_id": uuid::Uuid::new_v4() }),
        ),
        (
            "/api/v1/plugins/org.example.none/upgrades/prepare",
            json!({
                "version": "1.0.1",
                "target_config": { "schema_revision": "1", "values": {} },
                "state_policy": "preserve"
            }),
        ),
        ("/api/v1/plugins/org.example.none/enable", json!({})),
    ] {
        let (st, hdrs, resp) = env
            .send(req_json(
                "POST",
                path,
                Some(TOKEN),
                &[("if-match", "\"0\""), ("idempotency-key", "busy-check")],
                Some(body),
            ))
            .await;
        assert_err(st, &resp, StatusCode::SERVICE_UNAVAILABLE, "runtime.busy");
        assert_eq!(hdrs.get(header::RETRY_AFTER).unwrap(), "1");
    }
}

#[tokio::test]
async fn secrets_reauth_tokens_and_passwords_are_redacted_everywhere() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.redact", "1.0.0", 1, true);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    let rev = detail["revision"].as_i64().unwrap();

    let (st_sc, _, b_sc) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.redact/config-schema",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let schema_rev = assert_ok(st_sc, &b_sc, StatusCode::OK)["schema_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let secret_val = "super-secret-api-token-98765";
    let (st_put, _, b_put) = env
        .send(req_json(
            "PUT",
            "/api/v1/plugins/org.example.redact/config",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{rev}\"")),
                ("idempotency-key", "cfg-secret-put"),
            ],
            Some(json!({
                "schema_revision": schema_rev,
                "values": { "mode": "prod" },
                "secrets": { "api_secret": { "action": "replace", "value": secret_val } }
            })),
        ))
        .await;
    let put_data = assert_ok(st_put, &b_put, StatusCode::OK);
    assert!(!b_put.to_string().contains(secret_val));
    assert_eq!(put_data["secrets"]["api_secret"]["configured"], true);

    let (st_get, _, b_get) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.redact/config",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_ok(st_get, &b_get, StatusCode::OK);
    assert!(!b_get.to_string().contains(secret_val));

    let (st_aud, _, b_aud) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.redact/audit",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let aud_str = b_aud.to_string();
    assert_eq!(st_aud, StatusCode::OK);
    assert!(!aud_str.contains(secret_val));
    assert!(!aud_str.contains(ADMIN_PASS));

    let wrong_pw = "wrong-secret-password-xyz";
    let (st_re, _, b_re) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-reauth",
            Some(TOKEN),
            &[],
            Some(json!({ "password": wrong_pw, "purpose": "plugin:upload" })),
        ))
        .await;
    assert_err(st_re, &b_re, StatusCode::UNAUTHORIZED, "auth.invalid");
    assert!(!b_re.to_string().contains(wrong_pw));
}

#[tokio::test]
async fn multipart_upload_quote_aware_and_binary_boundary_checks() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let cfg_schema = plugin_http_support::pkg::sample_schema(false, None);
    let manifest = plugin_http_support::pkg::task_manifest_with_options(
        "org.example.mpart",
        "1.0.0",
        1,
        false,
        &["kv"],
    );
    let task_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{}}"#;
    let pkg = plugin_http_support::pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/task.schema.json", task_schema.as_bytes()),
        (
            "components/worker.wasm",
            plugin_http_support::pkg::task_wasm(),
        ),
        ("assets/marker.bin", b"\r\n--B1EXTRAdef\r\n"),
    ]);

    let build_raw_req = |ct: &str, disp: &str, bnd: &str, idem: &str, uid: &str, reauth: &str| {
        let mut body = Vec::new();
        for (hdr, data) in [
            (
                "Content-Disposition: form-data; name=\"upload_id\"\r\n\r\n".to_string(),
                uid.as_bytes(),
            ),
            (
                "Content-Disposition: form-data; name=\"reauth_token\"\r\n\r\n".to_string(),
                reauth.as_bytes(),
            ),
            (format!("{disp}\r\n\r\n"), pkg.as_slice()),
        ] {
            body.extend_from_slice(format!("--{bnd}\r\n{hdr}").as_bytes());
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{bnd}--\r\n").as_bytes());
        axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/plugin-uploads")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", ct)
            .header("if-match", "\"0\"")
            .header("idempotency-key", idem)
            .body(axum::body::Body::from(body))
            .unwrap()
    };

    // 1. Quoted filename before name containing `; name=evil` + binary `\r\n--B1EXTRA` payload
    let r1 = env.reauth("plugin:upload").await;
    let u1 = uuid::Uuid::new_v4().to_string();
    let (st1, _, b1) = env
        .send(build_raw_req(
            "multipart/form-data; boundary=B1",
            "Content-Disposition: form-data; filename=\"a; name=evil.cmplugin\"; name=\"file\"",
            "B1",
            "mp-quoted-disp",
            &u1,
            &r1,
        ))
        .await;
    assert_ok(st1, &b1, StatusCode::CREATED);

    // 2. Quoted boundary containing `;`
    let r2 = env.reauth("plugin:upload").await;
    let u2 = uuid::Uuid::new_v4().to_string();
    let (st2, _, b2) = env
        .send(build_raw_req(
            "multipart/form-data; boundary=\"abc;def\"",
            "Content-Disposition: form-data; name=\"file\"; filename=\"p.cmplugin\"",
            "abc;def",
            "mp-quoted-bnd",
            &u2,
            &r2,
        ))
        .await;
    assert_ok(st2, &b2, StatusCode::CREATED);

    // 3. Unbalanced quote in Content-Type boundary must be rejected with 400 request.invalid
    let r3 = env.reauth("plugin:upload").await;
    let u3 = uuid::Uuid::new_v4().to_string();
    let (st3, _, b3) = env
        .send(build_raw_req(
            "multipart/form-data; boundary=\"UnbalB",
            "Content-Disposition: form-data; name=\"file\"; filename=\"p.cmplugin\"",
            "UnbalB",
            "mp-unbal-bnd",
            &u3,
            &r3,
        ))
        .await;
    assert_err(st3, &b3, StatusCode::BAD_REQUEST, "request.invalid");
}
