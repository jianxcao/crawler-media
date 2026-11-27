use crate::support::{FakeSitePorts, INDEXER_WASM};
use plugin_api::{ApiError, ErrorCode, Failure, HttpTarget, SearchQuery};
use plugin_host::{Invocation, Limits, PluginRuntime};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn http_site_auth_requires_grant_and_credential_epoch() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports = Arc::new(FakeSitePorts::new(&["http", "kv"])); // NO http:site-auth
    let host = runtime.host(ports);
    let compiled_idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    // Missing http:site-auth grant -> PermissionDenied
    let mut inv = Invocation::new("org.example.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.credential_epoch = Some("epoch-a-1".into());
    let err = host
        .indexer_search(
            &compiled_idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "site-auth-http".into(),
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

    // With http:site-auth grant, missing credential_epoch -> WrongContext
    let ports_auth = Arc::new(FakeSitePorts::new(&["http", "http:site-auth", "kv"]));
    let host_auth = runtime.host(ports_auth);
    let mut inv_no_epoch = Invocation::new("org.example.site", Duration::from_secs(5));
    inv_no_epoch.site_id = Some("site-a".into());
    let err = host_auth
        .indexer_search(
            &compiled_idx,
            inv_no_epoch,
            "fixture",
            SearchQuery {
                keyword: "site-auth-http".into(),
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
}

#[test]
fn download_refs_and_kv_are_scoped_by_site_epoch_and_generation() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let ports_auth = Arc::new(FakeSitePorts::new(&["http", "http:site-auth", "kv"]));
    let host_auth = runtime.host(ports_auth);
    let compiled_idx = host_auth
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    // Sealed download_ref from site-a/epoch-1 fails closed on site-b, rotated epoch, or bumped generation
    let mut inv_a1 = Invocation::new("org.example.site", Duration::from_secs(5));
    inv_a1.site_id = Some("site-a".into());
    inv_a1.credential_epoch = Some("epoch-a-1".into());
    inv_a1.generation = 1;
    let batch = host_auth
        .indexer_search(
            &compiled_idx,
            inv_a1.clone(),
            "fixture",
            SearchQuery {
                keyword: "matrix".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    let sealed_ref = batch.items[0].download_ref.clone();
    assert_ne!(sealed_ref, "raw-dl-1", "download_ref must be host-sealed");

    // Same scope works
    let resolved = host_auth
        .indexer_resolve_download(&compiled_idx, inv_a1.clone(), "fixture", sealed_ref.clone())
        .unwrap();
    assert!(matches!(resolved.target, HttpTarget::SiteRelative(_)));

    // Cross-site fails before guest
    let mut inv_b = inv_a1.clone();
    inv_b.site_id = Some("site-b".into());
    inv_b.credential_epoch = Some("epoch-b-1".into());
    assert!(
        host_auth
            .indexer_resolve_download(&compiled_idx, inv_b, "fixture", sealed_ref.clone())
            .is_err()
    );

    // Rotated credential_epoch fails
    let mut inv_rotated = inv_a1.clone();
    inv_rotated.credential_epoch = Some("epoch-a-2".into());
    assert!(
        host_auth
            .indexer_resolve_download(
                &compiled_idx,
                inv_rotated.clone(),
                "fixture",
                sealed_ref.clone()
            )
            .is_err()
    );

    // kv is scoped by (site_id, credential_epoch): site-a/epoch-1 write is invisible to site-b or rotated epoch
    let kv_w = host_auth
        .indexer_search(
            &compiled_idx,
            inv_a1.clone(),
            "fixture",
            SearchQuery {
                keyword: "kv-write".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(kv_w.items[0].title, "kv:scoped-val");
    let kv_r_same = host_auth
        .indexer_search(
            &compiled_idx,
            inv_a1.clone(),
            "fixture",
            SearchQuery {
                keyword: "kv-read".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(kv_r_same.items[0].title, "kv-read:scoped-val");
    let kv_r_rotated = host_auth
        .indexer_search(
            &compiled_idx,
            inv_rotated,
            "fixture",
            SearchQuery {
                keyword: "kv-read".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(kv_r_rotated.items[0].title, "kv-read:none");

    // Stale generation fails
    let mut inv_gen2 = inv_a1;
    inv_gen2.generation = 2;
    assert!(
        host_auth
            .indexer_resolve_download(&compiled_idx, inv_gen2, "fixture", sealed_ref)
            .is_err()
    );
}
