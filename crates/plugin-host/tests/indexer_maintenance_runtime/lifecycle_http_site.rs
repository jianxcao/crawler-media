use crate::support::{FakeSitePorts, INDEXER_WASM, MAINTENANCE_WASM};
use plugin_api::{ApiError, ErrorCode, Failure, MaintenanceAction, MaintenanceState, SearchQuery};
use plugin_host::{Invocation, Limits, PluginRuntime};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn indexer_and_maintenance_lifecycle_validate_enforces_context() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&[
        "http",
        "http:site-auth",
        "read:sites",
        "kv",
    ]));
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .expect("indexer fixture loads");
    let compiled_maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .expect("maintenance fixture loads");

    // Lifecycle validate rejects http, kv.put, browser.open with WrongContext
    for schema in [
        "wrong-context-http",
        "wrong-context-kv-put",
        "wrong-context-browser",
    ] {
        assert!(matches!(
            host.validate_indexer(
                &compiled_idx,
                Invocation::new("org.example.site", Duration::from_secs(2)),
                schema,
                1,
            ),
            Err(Failure::Guest(ApiError {
                code: ErrorCode::WrongContext,
                ..
            }))
        ));
    }
    assert!(
        host.validate_maintenance(
            &compiled_maint,
            Invocation::new("org.example.maint", Duration::from_secs(2)),
            "{}",
            1,
        )
        .is_ok()
    );
}

#[test]
fn http_fetch_sanitizes_headers_enforces_budget_and_fetch_many_bounds() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let mut ports = FakeSitePorts::new(&["http", "http:site-auth", "read:sites", "kv"]);
    ports.inject_set_cookie = true;
    let host = runtime.host(Arc::new(ports));
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .expect("indexer fixture loads");

    let mut inv = Invocation::new("org.example.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-a-1".into());

    // http.fetch strips Set-Cookie / Authorization before guest receives response
    let batch = host
        .indexer_search(
            &compiled_idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "http-fetch".into(),
                page: 1,
                categories: vec![],
            },
        )
        .expect("http-fetch strips sensitive response headers");
    assert_eq!(batch.items[0].title, "http:ok-search");

    // Forbidden request headers rejected (including Set-Cookie)
    for kw in ["forbidden-header", "forbidden-set-cookie-header"] {
        let err = host
            .indexer_search(
                &compiled_idx,
                inv.clone(),
                "fixture",
                SearchQuery {
                    keyword: kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::PermissionDenied | ErrorCode::InvalidArgument,
                    ..
                })
            ),
            "unexpected error for {kw}: {err:?}"
        );
    }

    // fetch-many preserves order; empty or >8 rejected; http_calls budget enforced
    let batch = host
        .indexer_search(
            &compiled_idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "fetch-many".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(batch.items[0].title, "many:ok-1|ok-2|ok-3");

    for bad_kw in ["fetch-many-empty", "fetch-many-too-many"] {
        let err = host
            .indexer_search(
                &compiled_idx,
                inv.clone(),
                "fixture",
                SearchQuery {
                    keyword: bad_kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            Failure::Guest(ApiError {
                code: ErrorCode::InvalidArgument,
                ..
            })
        ));
    }

    let err = host
        .indexer_search(
            &compiled_idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "http-quota".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Failure::Guest(ApiError {
            code: ErrorCode::QuotaExceeded,
            ..
        })
    ));
}

#[test]
fn site_views_are_scoped_blob_put_rejected_and_maintenance_http_runs() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&[
        "http",
        "http:site-auth",
        "read:sites",
        "kv",
    ]));
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .expect("indexer fixture loads");
    let compiled_maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .expect("maintenance fixture loads");

    let mut inv = Invocation::new("org.example.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-a-1".into());

    // site.* views scoped to invocation site_id
    let site_batch = host
        .indexer_search(
            &compiled_idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "site-views".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(site_batch.items[0].title, "site:Site Alpha:9999:1");

    let err = host
        .indexer_search(
            &compiled_idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "site-get-other".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Failure::Guest(ApiError {
            code: ErrorCode::PermissionDenied,
            ..
        })
    ));

    // blob.put restricted to Task | Endpoint -> WrongContext in Indexer
    let err = host
        .indexer_search(
            &compiled_idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "blob-put".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Failure::Guest(ApiError {
            code: ErrorCode::WrongContext,
            ..
        })
    ));

    // maintenance run works with site-auth http
    let mut m_inv = Invocation::new("org.example.maint", Duration::from_secs(5));
    m_inv.site_id = Some("site-a".into());
    m_inv.credential_epoch = Some("epoch-a-1".into());
    let out = host
        .maintenance_run(
            &compiled_maint,
            m_inv,
            MaintenanceAction::CheckIn,
            "http-checkin".into(),
        )
        .unwrap();
    assert_eq!(out.state, MaintenanceState::Completed);
    assert!(out.evidence.contains("Site Alpha"));
}

#[test]
fn http_fetch_enforces_case_insensitive_site_auth_slot_prefix() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .expect("indexer fixture loads");

    let mut inv = Invocation::new("org.example.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-a-1".into());

    for kw in [
        "site-auth-http-other-upper",
        "site-auth-http-other-mixed",
        "site-auth-http-wrong-case-site",
    ] {
        let err = host
            .indexer_search(
                &compiled_idx,
                inv.clone(),
                "fixture",
                SearchQuery {
                    keyword: kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                Failure::Guest(ApiError {
                    code: ErrorCode::PermissionDenied,
                    ..
                })
            ),
            "expected PermissionDenied for {kw}, got {err:?}"
        );
    }

    let ok_batch = host
        .indexer_search(
            &compiled_idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "site-auth-http-same-upper".into(),
                page: 1,
                categories: vec![],
            },
        )
        .expect("SITE:site-a should match site-a while preserving site id case");
    assert_eq!(ok_batch.items[0].title, "auth-slot:ok-/api/search");
}

#[test]
fn host_limits_clamp_invocation_max_http_calls_even_with_default_invocation() {
    let runtime = PluginRuntime::new(Limits {
        max_http_calls: 1,
        ..Limits::default()
    })
    .unwrap();
    let ports = Arc::new(FakeSitePorts::new(&["http", "http:site-auth"]));
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    let mut inv = Invocation::new("org.example.site", Duration::from_secs(5));
    assert_eq!(inv.max_http_calls, 8);
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-a-1".into());

    let single = host
        .indexer_search(
            &compiled_idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "http-fetch".into(),
                page: 1,
                categories: vec![],
            },
        )
        .expect("single http.fetch within Limits.max_http_calls=1 succeeds");
    assert_eq!(single.items[0].title, "http:ok-search");

    let exceeded = host
        .indexer_search(
            &compiled_idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "fetch-many".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            exceeded,
            Failure::Guest(ApiError {
                code: ErrorCode::QuotaExceeded,
                ..
            })
        ),
        "expected QuotaExceeded when guest exceeds clamped Limits.max_http_calls=1, got {exceeded:?}"
    );
}
