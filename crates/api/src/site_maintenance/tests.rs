use super::*;
use std::cell::Cell;

fn site() -> Site {
    Site {
        id: domain::SiteId::new(),
        name: "t".into(),
        url: "https://example.org/".into(),
        profile_id: "nexusphp".into(),
        cookie: Some("a=b".into()),
        api_key: None,
        rss_url: None,
        proxy: None,
        rate_limit_per_minute: None,
        cdp_url: None,
        downloader_id: None,
        enabled: true,
        maintenance_via: domain::MaintenanceVia::Browser,
        browser_search: false,
        timezone: "UTC".into(),
    }
}

fn run(
    resolves: Result<bool, PluginError>,
    browser: Result<(), PluginError>,
) -> (Result<(), PluginError>, u32) {
    let http_calls = Cell::new(0u32);
    let browser_was_pre_nav = matches!(browser, Err(PluginError::BrowserPreNavigation(_)));
    let s = site();
    let r = run_with_browser_fallback(
        &s,
        "check_in",
        resolves,
        || browser,
        |fallback| {
            assert_eq!(fallback, browser_was_pre_nav);
            http_calls.set(http_calls.get() + 1);
            Ok(())
        },
    );
    (r, http_calls.get())
}

#[test]
fn post_navigation_error_does_not_fall_back_no_double_check_in() {
    let (r, calls) = run(Ok(true), Err(PluginError::Fetch("after goto".into())));
    assert!(r.is_err());
    assert_eq!(
        calls, 0,
        "post-navigation failure must not trigger HTTP check-in"
    );
    let (r, calls) = run(Ok(true), Err(PluginError::Veto("policy".into())));
    assert!(r.is_err());
    assert_eq!(calls, 0);
}

#[test]
fn pre_navigation_error_falls_back_once() {
    let (r, calls) = run(
        Ok(true),
        Err(PluginError::BrowserPreNavigation("busy".into())),
    );
    assert!(r.is_ok());
    assert_eq!(calls, 1);
}

#[test]
fn browser_success_does_not_call_http() {
    let (r, calls) = run(Ok(true), Ok(()));
    assert!(r.is_ok());
    assert_eq!(calls, 0);
}

#[test]
fn disabled_endpoint_uses_http_and_config_error_surfaces() {
    let (r, calls) = run(Ok(false), Ok(()));
    assert!(r.is_ok());
    assert_eq!(calls, 1);
    let (r, calls) = run(Err(PluginError::Fetch("bad config".into())), Ok(()));
    assert!(r.is_err());
    assert_eq!(
        calls, 0,
        "config errors must not be swallowed into HTTP fallback"
    );
}
