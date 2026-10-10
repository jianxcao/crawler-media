use indexer::{Browser, BrowserConfig, FetchRequest, FetchMethod};

#[test]
fn enabled_managed_browser_can_fetch_without_external_cdp() {
    let tmp = tempfile::tempdir().unwrap();
    let browser = Browser::new(BrowserConfig::enable_in(tmp.path()).unwrap());
    let request = FetchRequest {
        key: "local-document".into(),
        url: "data:text/html,<html>local</html>".into(),
        method: FetchMethod::Get,
        body: None, cookie: None, api_key: None, proxy: None,
        render: true, cdp_url: None,
    };
    let result = browser.fetch_html(&request);
    assert!(result.is_ok(), "enabled managed browser never reaches a session: {result:?}");
}
