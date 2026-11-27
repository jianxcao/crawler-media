#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;

use axum::http::StatusCode;
use plugin_http_support::{
    ADMIN_PASS, HttpEnv, TOKEN, assert_err, assert_ok, assert_paged, build_21m_pkg, etag, location,
    pkg::{TestSigningKey, build_task_package},
    req_json, req_multipart,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[tokio::test]
async fn reauth_validates_password_and_purpose() {
    let env = HttpEnv::new().await;
    let (st, _, body) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-reauth",
            Some(TOKEN),
            &[],
            Some(json!({ "password": ADMIN_PASS, "purpose": "plugin:upload" })),
        ))
        .await;
    let data = assert_ok(st, &body, StatusCode::OK);
    assert_eq!(data["purpose"], "plugin:upload");
    assert!(!data["reauth_token"].as_str().unwrap().is_empty());

    for (pw, purpose, code, st_exp) in [
        ("", "plugin:upload", "auth.invalid", StatusCode::BAD_REQUEST),
        (
            "wrong",
            "plugin:upload",
            "auth.invalid",
            StatusCode::UNAUTHORIZED,
        ),
        (
            ADMIN_PASS,
            "bogus:purpose",
            "request.invalid",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (st_e, _, b_e) = env
            .send(req_json(
                "POST",
                "/api/v1/plugin-reauth",
                Some(TOKEN),
                &[],
                Some(json!({ "password": pw, "purpose": purpose })),
            ))
            .await;
        assert_err(st_e, &b_e, st_exp, code);
    }
}

#[tokio::test]
async fn trust_keys_prepare_and_create_if_match() {
    let env = HttpEnv::new().await;
    let k1 = TestSigningKey::generate(101);
    let (st_p, _, b_p) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-trust-keys/prepare",
            Some(TOKEN),
            &[],
            Some(json!({ "public_key": k1.public_key_str, "label": "Key One" })),
        ))
        .await;
    let prep = assert_ok(st_p, &b_p, StatusCode::OK);
    let reauth = env.reauth("trust-key:create").await;
    let create_body = json!({
        "label": "Key One", "public_key": prep["public_key"], "fingerprint": prep["fingerprint"],
        "consent_token": prep["consent_token"], "reauth_token": reauth,
    });
    let (st_428, _, b_428) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-trust-keys",
            Some(TOKEN),
            &[("idempotency-key", "tk-missing-im")],
            Some(create_body.clone()),
        ))
        .await;
    assert_err(
        st_428,
        &b_428,
        StatusCode::PRECONDITION_REQUIRED,
        "plugin.precondition_required",
    );

    let (st_409, _, b_409) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-trust-keys",
            Some(TOKEN),
            &[("if-match", "\"1\""), ("idempotency-key", "tk-bad-im")],
            Some(create_body.clone()),
        ))
        .await;
    assert_err(
        st_409,
        &b_409,
        StatusCode::CONFLICT,
        "plugin.revision_conflict",
    );

    let (st_c, h_c, b_c) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-trust-keys",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "tk-ok-1")],
            Some(create_body),
        ))
        .await;
    assert_ok(st_c, &b_c, StatusCode::CREATED);
    assert_eq!(etag(&h_c), "\"1\"");
}

#[tokio::test]
async fn trust_keys_list_cursor_pagination_and_query_validation() {
    let env = HttpEnv::new().await;
    env.pin_trust_key(&TestSigningKey::generate(101), "Key One")
        .await;
    env.pin_trust_key(&TestSigningKey::generate(102), "Key Two")
        .await;

    let mut seen_labels = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let uri = match &cursor {
            Some(c) => format!("/api/v1/plugin-trust-keys?limit=1&cursor={c}"),
            None => "/api/v1/plugin-trust-keys?limit=1".to_string(),
        };
        let (st, _, body) = env
            .send(req_json("GET", &uri, Some(TOKEN), &[], None))
            .await;
        let (items, next) = assert_paged(st, &body);
        assert_eq!(items.len(), 1);
        seen_labels.push(items[0]["label"].as_str().unwrap().to_string());
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert!(seen_labels.contains(&"crawler-media Official Plugins".to_string()));
    assert!(seen_labels.contains(&"Key One".to_string()));
    assert!(seen_labels.contains(&"Key Two".to_string()));
    assert_eq!(seen_labels.len(), 3);

    for bad_q in ["limit=0", "limit=201", "offset=0", "cursor="] {
        let (st_b, _, b_b) = env
            .send(req_json(
                "GET",
                &format!("/api/v1/plugin-trust-keys?{bad_q}"),
                Some(TOKEN),
                &[],
                None,
            ))
            .await;
        assert_err(st_b, &b_b, StatusCode::BAD_REQUEST, "plugin.invalid_input");
    }
}

#[tokio::test]
async fn trust_key_revoke_drains_signed_plugin() {
    let env = HttpEnv::new().await;
    let k1 = TestSigningKey::generate(103);
    let key_view = env.pin_trust_key(&k1, "Revoke Key").await;
    let key_id = key_view["key_id"].as_str().unwrap().to_string();

    let pkg = build_task_package("org.example.tkrev", "1.0.0", 1, false);
    let sig = k1.sign_package(&pkg);
    let detail = env.install_pkg(&pkg, Some(&sig), json!([])).await;
    env.enable_plugin(
        "org.example.tkrev",
        detail["revision"].as_i64().unwrap(),
        "en-tk",
    )
    .await;

    let rev_reauth = env.reauth("trust-key:revoke").await;
    let (st_rev, h_rev, b_rev) = env
        .send(req_json(
            "POST",
            &format!("/api/v1/plugin-trust-keys/{key_id}/revoke"),
            Some(TOKEN),
            &[("if-match", "\"1\""), ("idempotency-key", "tk-rev-1")],
            Some(json!({ "reason": "compromised", "reauth_token": rev_reauth })),
        ))
        .await;
    let op = assert_ok(st_rev, &b_rev, StatusCode::ACCEPTED);
    assert!(location(&h_rev).starts_with("/api/v1/plugin-operations/"));
    assert_eq!(op["state"], "succeeded");

    let (st_pg, _, b_pg) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.tkrev",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(
        assert_ok(st_pg, &b_pg, StatusCode::OK)["lifecycle"],
        "disabled"
    );
}

#[tokio::test]
async fn developer_mode_enable_and_disable_cascade_drains_unsigned() {
    let env = HttpEnv::new().await;
    let settings = env.enable_dev_mode().await;
    let s_rev = settings["revision"].as_i64().unwrap();

    let pkg = build_task_package("org.example.devplug", "1.0.0", 1, false);
    let detail = env.install_pkg(&pkg, None, json!([])).await;
    env.enable_plugin(
        "org.example.devplug",
        detail["revision"].as_i64().unwrap(),
        "en-dev",
    )
    .await;

    let (st_dis, h_dis, b_dis) = env
        .send(req_json(
            "POST",
            "/api/v1/settings/plugins/developer/disable",
            Some(TOKEN),
            &[
                ("if-match", &format!("\"{s_rev}\"")),
                ("idempotency-key", "dev-dis-1"),
            ],
            Some(json!({})),
        ))
        .await;
    let op = assert_ok(st_dis, &b_dis, StatusCode::ACCEPTED);
    assert!(location(&h_dis).starts_with("/api/v1/plugin-operations/"));
    assert_eq!(op["state"], "succeeded");

    let (st_pg, _, b_pg) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.devplug",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_eq!(
        assert_ok(st_pg, &b_pg, StatusCode::OK)["lifecycle"],
        "disabled"
    );
}

#[tokio::test]
async fn multipart_upload_signature_and_sha_checks() {
    let env = HttpEnv::new().await;
    let pkg = build_task_package("org.example.up", "1.0.0", 1, false);

    let r1 = env.reauth("plugin:upload").await;
    let u1 = Uuid::new_v4().to_string();
    let (st_unsig, _, b_unsig) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "up-unsig")],
            "----B1",
            &[
                ("upload_id", None, u1.as_bytes()),
                ("file", Some("p.cmplugin"), &pkg),
                ("reauth_token", None, r1.as_bytes()),
            ],
        ))
        .await;
    assert_err(
        st_unsig,
        &b_unsig,
        StatusCode::FORBIDDEN,
        "plugin.signature_required",
    );

    let big_sig = vec![b'a'; 64 * 1024 + 1];
    let r2 = env.reauth("plugin:upload").await;
    let u2 = Uuid::new_v4().to_string();
    let (st_sb, _, b_sb) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "up-sigbig")],
            "----B2",
            &[
                ("upload_id", None, u2.as_bytes()),
                ("file", Some("p.cmplugin"), &pkg),
                ("signature", Some("p.minisig"), &big_sig),
                ("reauth_token", None, r2.as_bytes()),
            ],
        ))
        .await;
    assert_err(
        st_sb,
        &b_sb,
        StatusCode::PAYLOAD_TOO_LARGE,
        "plugin.signature_too_large",
    );

    let r3 = env.reauth("plugin:upload").await;
    let u3 = Uuid::new_v4().to_string();
    let (st_sha, _, b_sha) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[
                ("if-match", "\"0\""),
                ("idempotency-key", "up-badsha"),
                ("x-plugin-sha256", &"0".repeat(64)),
            ],
            "----B3",
            &[
                ("upload_id", None, u3.as_bytes()),
                ("file", Some("p.cmplugin"), &pkg),
                ("reauth_token", None, r3.as_bytes()),
            ],
        ))
        .await;
    assert_err(
        st_sha,
        &b_sha,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_package",
    );
}

#[tokio::test]
async fn multipart_upload_over_20mib_passes_and_over_64mib_returns_413() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg_21m = build_21m_pkg("org.example.big21");
    let sha_21m = format!("{:x}", Sha256::digest(&pkg_21m));
    let r4 = env.reauth("plugin:upload").await;
    let u4 = Uuid::new_v4().to_string();
    let (st_21m, h_21m, b_21m) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[
                ("if-match", "\"0\""),
                ("idempotency-key", "up-21m"),
                ("x-plugin-sha256", &sha_21m),
            ],
            "----B4",
            &[
                ("upload_id", None, u4.as_bytes()),
                ("file", Some("big21.cmplugin"), &pkg_21m),
                ("reauth_token", None, r4.as_bytes()),
            ],
        ))
        .await;
    let up21 = assert_ok(st_21m, &b_21m, StatusCode::CREATED);
    assert_eq!(up21["upload_id"], u4);
    assert_eq!(location(&h_21m), format!("/api/v1/plugin-uploads/{u4}"));

    let too_big = vec![0x50u8; 64 * 1024 * 1024 + 1024];
    let r5 = env.reauth("plugin:upload").await;
    let u5 = Uuid::new_v4().to_string();
    let (st_65m, _, b_65m) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "up-65m")],
            "----B5",
            &[
                ("upload_id", None, u5.as_bytes()),
                ("file", Some("too-big.cmplugin"), &too_big),
                ("reauth_token", None, r5.as_bytes()),
            ],
        ))
        .await;
    assert_err(
        st_65m,
        &b_65m,
        StatusCode::PAYLOAD_TOO_LARGE,
        "plugin.package_too_large",
    );
}

#[tokio::test]
async fn upload_replay_and_install_prepare_execute_and_list_plugins() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let pkg = build_task_package("org.example.inst", "1.0.0", 1, false);
    let reauth = env.reauth("plugin:upload").await;
    let uid = Uuid::new_v4().to_string();
    let parts = [
        ("upload_id", None, uid.as_bytes()),
        ("file", Some("p.cmplugin"), pkg.as_slice()),
        ("reauth_token", None, reauth.as_bytes()),
    ];

    let (st1, _, b1) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "up-rep-1")],
            "----R1",
            &parts,
        ))
        .await;
    let d1 = assert_ok(st1, &b1, StatusCode::CREATED);

    let (st2, _, b2) = env
        .send(req_multipart(
            "/api/v1/plugin-uploads",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "up-rep-1")],
            "----R2-DIFF",
            &parts,
        ))
        .await;
    assert_eq!(d1, assert_ok(st2, &b2, StatusCode::CREATED));

    let (st_repo, _, b_repo) = env
        .send(req_json(
            "POST",
            "/api/v1/plugin-installations/prepare",
            Some(TOKEN),
            &[("if-match", "\"0\""), ("idempotency-key", "prep-repo")],
            Some(json!({ "repo_id": "official" })),
        ))
        .await;
    assert_err(
        st_repo,
        &b_repo,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );

    env.install_pkg(&pkg, None, json!([])).await;
    let pkg_b = build_task_package("org.example.inst2", "1.0.0", 1, false);
    env.install_pkg(&pkg_b, None, json!([])).await;

    let (st_lp, _, b_lp) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins?state=installed-disabled&limit=1",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (page1, next) = assert_paged(st_lp, &b_lp);
    assert_eq!(page1.len(), 1);
    let cur = next.unwrap();
    let (st_lp2, _, b_lp2) = env
        .send(req_json(
            "GET",
            &format!("/api/v1/plugins?state=installed-disabled&limit=1&cursor={cur}"),
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let (page2, next2) = assert_paged(st_lp2, &b_lp2);
    assert_eq!(page2.len(), 1);
    assert_eq!(next2, None);

    let (st_det, h_det, b_det) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins/org.example.inst",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    let det = assert_ok(st_det, &b_det, StatusCode::OK);
    assert_eq!(etag(&h_det), format!("\"{}\"", det["revision"]));
}

#[tokio::test]
async fn list_plugins_rejects_malformed_cursor() {
    let env = HttpEnv::new().await;
    let (st_bad, _, b_bad) = env
        .send(req_json(
            "GET",
            "/api/v1/plugins?cursor=not-a-valid-cursor",
            Some(TOKEN),
            &[],
            None,
        ))
        .await;
    assert_err(
        st_bad,
        &b_bad,
        StatusCode::BAD_REQUEST,
        "plugin.invalid_input",
    );
}
