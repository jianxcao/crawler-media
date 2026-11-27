use plugin_api::{
    ApiError, BrowserElement, BrowserFillValue, BrowserPurpose, ConfigView, ErrorCode, ErrorDetail,
    Failure, Header, HttpResponse, PluginTrust, SearchQuery,
};
use plugin_host::{BrowserSessionPort, HostPorts, Invocation, Limits, PluginRuntime};
use std::collections::HashSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

const INDEXER_WASM: &[u8] = include_bytes!("fixtures/indexer_fixture.wasm");
const MAINTENANCE_WASM: &[u8] = include_bytes!("fixtures/maintenance_fixture.wasm");

struct FakeBrowserState {
    open_calls: AtomicUsize,
    close_calls: AtomicUsize,
    active_slots: AtomicUsize,
    fill_calls: AtomicUsize,
    fail_close: bool,
    isolation_unavailable: bool,
}

impl FakeBrowserState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open_calls: AtomicUsize::new(0),
            close_calls: AtomicUsize::new(0),
            active_slots: AtomicUsize::new(0),
            fill_calls: AtomicUsize::new(0),
            fail_close: false,
            isolation_unavailable: false,
        })
    }
}

struct FakeBrowserSession {
    state: Arc<FakeBrowserState>,
    closed: bool,
}

impl Drop for FakeBrowserSession {
    fn drop(&mut self) {
        if !self.closed {
            self.state.active_slots.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl BrowserSessionPort for FakeBrowserSession {
    fn navigate(&mut self, site_relative_url: &str, _wait_until: &str) -> Result<String, ApiError> {
        Ok(format!("nav:{site_relative_url}"))
    }
    fn dom(&mut self, _max_bytes: u32) -> Result<String, ApiError> {
        Ok("<html><body>ok</body></html>".into())
    }
    fn query(&mut self, _selector: &str, _limit: u32) -> Result<Vec<BrowserElement>, ApiError> {
        Ok(vec![BrowserElement {
            handle: "raw-el-1".into(),
            visible_text: "Result One".into(),
        }])
    }
    fn text(&mut self, element: &str, _max_bytes: u32) -> Result<String, ApiError> {
        assert_eq!(
            element, "raw-el-1",
            "host must map opaque handle back to backend handle"
        );
        Ok("Result One".into())
    }
    fn attribute(&mut self, element: &str, name: &str) -> Result<Option<String>, ApiError> {
        assert_eq!(element, "raw-el-1");
        if name == "href" {
            Ok(Some("/details.php?id=1".into()))
        } else {
            Ok(None)
        }
    }
    fn click(&mut self, element: &str) -> Result<String, ApiError> {
        assert_eq!(element, "raw-el-1");
        Ok("clicked".into())
    }
    fn fill(&mut self, element: &str, value: BrowserFillValue) -> Result<(), ApiError> {
        assert_eq!(element, "raw-el-1");
        self.state.fill_calls.fetch_add(1, Ordering::SeqCst);
        match value {
            BrowserFillValue::Text(_) | BrowserFillValue::CredentialSlot(_) => Ok(()),
        }
    }
    fn wait_for(
        &mut self,
        _selector: Option<&str>,
        _condition: &str,
        _timeout_ms: u64,
    ) -> Result<String, ApiError> {
        Ok("ready".into())
    }
    fn read_response(&mut self, receipt: &str, _max_bytes: u32) -> Result<HttpResponse, ApiError> {
        Ok(HttpResponse {
            status: 200,
            headers: vec![
                Header {
                    name: "Set-Cookie".into(),
                    value: "secret=bad".into(),
                },
                Header {
                    name: "Content-Type".into(),
                    value: "text/html".into(),
                },
            ],
            body: format!("resp:{receipt}").into_bytes(),
            truncated: false,
            receipt: Some(receipt.into()),
        })
    }
    fn capture_session(&mut self) -> Result<String, ApiError> {
        Ok("captured-session-token".into())
    }
    fn close(&mut self) -> Result<(), ApiError> {
        self.state.close_calls.fetch_add(1, Ordering::SeqCst);
        if self.state.fail_close {
            return Err(ApiError {
                code: ErrorCode::UnknownOutcome,
                message: "close failed".into(),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            });
        }
        if !self.closed {
            self.closed = true;
            self.state.active_slots.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

struct FakeBrowserPorts {
    grants: HashSet<String>,
    browser: Arc<FakeBrowserState>,
}

impl FakeBrowserPorts {
    fn new(grants: &[&str], browser: Arc<FakeBrowserState>) -> Self {
        Self {
            grants: grants.iter().map(|s| s.to_string()).collect(),
            browser,
        }
    }
}

impl HostPorts for FakeBrowserPorts {
    fn authorized(&self, _inv: &Invocation, cap: &str) -> bool {
        self.grants.contains(cap)
    }

    fn config_get(&self, _inv: &Invocation) -> Result<ConfigView, ApiError> {
        Ok(ConfigView {
            revision: 1,
            validated_json: "{}".into(),
        })
    }

    fn browser_open(
        &self,
        _inv: &Invocation,
        _purpose: BrowserPurpose,
    ) -> Result<Box<dyn BrowserSessionPort>, ApiError> {
        self.browser.open_calls.fetch_add(1, Ordering::SeqCst);
        if self.browser.isolation_unavailable {
            return Err(ApiError {
                code: ErrorCode::Unsupported,
                message: "cdp disabled or site opted out".into(),
                detail: Some(ErrorDetail::BrowserIsolationUnavailable),
                retry_after_ms: None,
                operation_id: None,
            });
        }
        self.browser.active_slots.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeBrowserSession {
            state: self.browser.clone(),
            closed: false,
        }))
    }
}

#[test]
fn untrusted_plugins_return_browser_isolation_unavailable_without_calling_host_open() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:search", "browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();
    let maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();

    // Default Invocation has PluginTrust::Untrusted
    let mut inv = Invocation::new("org.thirdparty.idx", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    assert_eq!(inv.trust, PluginTrust::Untrusted);

    let err = host
        .indexer_search(
            &idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "browser-search".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Failure::Guest(ApiError {
            code: ErrorCode::Unsupported,
            detail: Some(ErrorDetail::BrowserIsolationUnavailable),
            ..
        })
    ));

    let m_err = host
        .maintenance_run(
            &maint,
            inv,
            plugin_api::MaintenanceAction::CheckIn,
            "browser-checkin".into(),
        )
        .unwrap_err();
    assert!(matches!(
        m_err,
        Failure::Guest(ApiError {
            code: ErrorCode::Unsupported,
            detail: Some(ErrorDetail::BrowserIsolationUnavailable),
            ..
        })
    ));
    assert_eq!(
        state.open_calls.load(Ordering::SeqCst),
        0,
        "HostPorts::browser_open must NOT be called for untrusted plugins"
    );
}

#[test]
fn browser_triple_gate_allows_trusted_and_enforces_role_purpose() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:search", "browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();
    let maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();

    // BuiltIn + AdminTrusted succeed
    for trust in [PluginTrust::BuiltIn, PluginTrust::AdminTrusted] {
        let mut inv = Invocation::new("builtin.site", Duration::from_secs(5));
        inv.site_id = Some("site-a".into());
        inv.trust = trust;
        let batch = host
            .indexer_search(
                &idx,
                inv,
                "fixture",
                SearchQuery {
                    keyword: "browser-search".into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap();
        assert_eq!(batch.items[0].title, "browser:Result One:/details.php?id=1");
    }
    assert_eq!(state.active_slots.load(Ordering::SeqCst), 0);

    // Maintenance browser check-in + capture_session succeeds for BuiltIn
    let mut m_inv = Invocation::new("builtin.maint", Duration::from_secs(5));
    m_inv.site_id = Some("site-a".into());
    m_inv.trust = PluginTrust::BuiltIn;
    let outcome = host
        .maintenance_run(
            &maint,
            m_inv.clone(),
            plugin_api::MaintenanceAction::CheckIn,
            "browser-checkin".into(),
        )
        .unwrap();
    assert_eq!(
        outcome.session_capture.as_deref(),
        Some("captured-session-token")
    );

    // Maintenance calling Purpose::Search -> WrongContext
    let err = host
        .maintenance_run(
            &maint,
            m_inv,
            plugin_api::MaintenanceAction::CheckIn,
            "browser-wrong-purpose".into(),
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
fn browser_epoch_fencing_double_close_and_missing_site_or_grant_enforced() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:search", "browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    let mut inv = Invocation::new("builtin.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.trust = PluginTrust::BuiltIn;

    // Epoch fencing across navigate & click, forged receipt, capture_session in Search, use after close
    for (kw, expected) in [
        ("browser-stale-after-nav", ErrorCode::StaleHandle),
        ("browser-stale-after-click", ErrorCode::StaleHandle),
        ("browser-forged-receipt", ErrorCode::StaleHandle),
        ("browser-capture-forbidden", ErrorCode::WrongContext),
        ("browser-use-after-close", ErrorCode::StaleHandle),
    ] {
        let err = host
            .indexer_search(
                &idx,
                inv.clone(),
                "fixture",
                SearchQuery {
                    keyword: kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap_err();
        match err {
            Failure::Guest(ApiError { code, .. }) => {
                assert_eq!(code, expected, "unexpected code for {kw}");
            }
            other => panic!("expected Failure::Guest({expected:?}) for {kw}, got {other:?}"),
        }
    }

    // Idempotent close
    let batch = host
        .indexer_search(
            &idx,
            inv.clone(),
            "fixture",
            SearchQuery {
                keyword: "browser-double-close".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(batch.items[0].title, "closed-twice");

    // Missing site_id -> WrongContext
    let mut no_site = inv.clone();
    no_site.site_id = None;
    let err = host
        .indexer_search(
            &idx,
            no_site,
            "fixture",
            SearchQuery {
                keyword: "browser-search".into(),
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

    // Missing grant -> PermissionDenied
    let no_grant_ports = Arc::new(FakeBrowserPorts::new(&[], state));
    let no_grant_host = runtime.host(no_grant_ports);
    let err = no_grant_host
        .indexer_search(
            &idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "browser-search".into(),
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
}

#[test]
fn browser_session_closes_on_trap_and_quarantines_on_close_failure() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:search", "browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    let mut inv = Invocation::new("builtin.trap", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.trust = PluginTrust::BuiltIn;
    let err = host
        .indexer_search(
            &idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "browser-trap".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(err, Failure::Trap(_)));
    assert_eq!(state.close_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.active_slots.load(Ordering::SeqCst), 0);

    // Close failure surfaces error and quarantines session
    let fail_state = Arc::new(FakeBrowserState {
        open_calls: AtomicUsize::new(0),
        close_calls: AtomicUsize::new(0),
        active_slots: AtomicUsize::new(0),
        fill_calls: AtomicUsize::new(0),
        fail_close: true,
        isolation_unavailable: false,
    });
    let fail_ports = Arc::new(FakeBrowserPorts::new(
        &["browser:search"],
        fail_state.clone(),
    ));
    let fail_host = runtime.host(fail_ports);
    let mut inv2 = Invocation::new("builtin.failclose", Duration::from_secs(5));
    inv2.site_id = Some("site-a".into());
    inv2.trust = PluginTrust::BuiltIn;
    let err = fail_host
        .indexer_search(
            &idx,
            inv2,
            "fixture",
            SearchQuery {
                keyword: "browser-double-close".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap_err();
    assert!(matches!(
        err,
        Failure::Guest(ApiError {
            code: ErrorCode::UnknownOutcome,
            ..
        })
    ));
    assert!(fail_state.close_calls.load(Ordering::SeqCst) >= 1);
}

#[test]
fn browser_navigate_and_attribute_enforce_boundary_validation() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(&["browser:search"], state));
    let host = runtime.host(ports);
    let idx = host
        .load_indexer(INDEXER_WASM, plugin_host::INDEXER_WORLD)
        .unwrap();

    let mut inv = Invocation::new("builtin.site", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.trust = PluginTrust::BuiltIn;

    for (kw, expected_code, expected_detail) in [
        (
            "browser-nav-external",
            ErrorCode::PermissionDenied,
            Some(ErrorDetail::BlockedAddress),
        ),
        (
            "browser-nav-proto-rel",
            ErrorCode::PermissionDenied,
            Some(ErrorDetail::BlockedAddress),
        ),
        (
            "browser-nav-js",
            ErrorCode::PermissionDenied,
            Some(ErrorDetail::BlockedAddress),
        ),
        ("browser-nav-space", ErrorCode::InvalidArgument, None),
        ("browser-attr-on", ErrorCode::PermissionDenied, None),
        ("browser-attr-empty", ErrorCode::InvalidArgument, None),
    ] {
        let err = host
            .indexer_search(
                &idx,
                inv.clone(),
                "fixture",
                SearchQuery {
                    keyword: kw.into(),
                    page: 1,
                    categories: vec![],
                },
            )
            .unwrap_err();
        match err {
            Failure::Guest(ApiError { code, detail, .. }) => {
                assert_eq!(code, expected_code, "unexpected code for {kw}");
                assert_eq!(detail, expected_detail, "unexpected detail for {kw}");
            }
            other => panic!("expected Failure::Guest for {kw}, got {other:?}"),
        }
    }

    let batch = host
        .indexer_search(
            &idx,
            inv,
            "fixture",
            SearchQuery {
                keyword: "browser-attr-data".into(),
                page: 1,
                categories: vec![],
            },
        )
        .unwrap();
    assert_eq!(batch.items[0].title, "attr:");
}

#[test]
fn browser_fill_credential_slot_enforces_non_empty_and_site_scope_at_host_boundary() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();

    let mut inv = Invocation::new("builtin.checkin", Duration::from_secs(5));
    inv.site_id = Some("site-a".into());
    inv.trust = PluginTrust::BuiltIn;

    for (action, expected_code) in [
        ("browser-fill-slot:", ErrorCode::InvalidArgument),
        ("browser-fill-slot:   ", ErrorCode::InvalidArgument),
        ("browser-fill-slot:site:   ", ErrorCode::InvalidArgument),
        ("browser-fill-slot:site:site-b", ErrorCode::PermissionDenied),
        ("browser-fill-slot:SITE:site-b", ErrorCode::PermissionDenied),
    ] {
        let err = host
            .maintenance_run(
                &maint,
                inv.clone(),
                plugin_api::MaintenanceAction::CheckIn,
                action.into(),
            )
            .expect_err("invalid or cross-site credential slot must fail");
        match err {
            Failure::Guest(ApiError { code, .. }) => {
                assert_eq!(code, expected_code, "unexpected code for {action}");
            }
            other => panic!("expected Failure::Guest for {action}, got {other:?}"),
        }
    }
    assert_eq!(
        state.fill_calls.load(Ordering::SeqCst),
        0,
        "BrowserSessionPort::fill must not be invoked when CredentialSlot fails host boundary validation"
    );

    // Same-site prefixed slot `site:site-a` and bare slot `site-password` must succeed
    for valid_action in [
        "browser-fill-slot:site:site-a",
        "browser-fill-slot:site-password",
    ] {
        let out = host
            .maintenance_run(
                &maint,
                inv.clone(),
                plugin_api::MaintenanceAction::CheckIn,
                valid_action.into(),
            )
            .expect("valid credential slot must succeed");
        assert_eq!(out.evidence, "slot-ok");
    }
    assert_eq!(state.fill_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn invocation_site_id_with_site_prefix_canonicalizes_across_host_boundaries() {
    let runtime = PluginRuntime::new(Limits::default()).unwrap();
    let state = FakeBrowserState::new();
    let ports = Arc::new(FakeBrowserPorts::new(
        &["browser:maintenance"],
        state.clone(),
    ));
    let host = runtime.host(ports);
    let maint = host
        .load_maintenance(MAINTENANCE_WASM, plugin_host::MAINTENANCE_WORLD)
        .unwrap();

    // 1. `Invocation.site_id = Some("site:site-a")` must accept `site:site-a` slot without self-denial
    let mut inv_prefixed = Invocation::new("builtin.checkin", Duration::from_secs(5));
    inv_prefixed.site_id = Some("site:site-a".into());
    inv_prefixed.trust = PluginTrust::BuiltIn;
    let out = host
        .maintenance_run(
            &maint,
            inv_prefixed,
            plugin_api::MaintenanceAction::CheckIn,
            "browser-fill-slot:site:site-a".into(),
        )
        .expect("prefixed invocation.site_id must canonicalize to site-a");
    assert_eq!(out.evidence, "slot-ok");

    // 2. `Invocation.site_id = Some("site:   ")` must be rejected as WrongContext (unbound)
    let mut inv_blank = Invocation::new("builtin.checkin", Duration::from_secs(5));
    inv_blank.site_id = Some("site:   ".into());
    inv_blank.trust = PluginTrust::BuiltIn;
    let err = host
        .maintenance_run(
            &maint,
            inv_blank,
            plugin_api::MaintenanceAction::CheckIn,
            "browser-fill-slot:site-password".into(),
        )
        .expect_err("blank `site:   ` invocation.site_id must fail WrongContext");
    match err {
        Failure::Guest(ApiError { code, .. }) => assert_eq!(code, ErrorCode::WrongContext),
        other => panic!("expected WrongContext, got {other:?}"),
    }
}
