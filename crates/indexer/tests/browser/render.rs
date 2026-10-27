//! Rendering one page through a browser: injected HTML, cookies, failure cleanup,
//! deadlines, proxies and external CDP.

use crate::support::*;

#[test]
fn recording_fetcher_never_launches_chromium_for_injected_html() {
    let overlay = tempfile::tempdir().unwrap();
    write_render_overlay(overlay.path());
    let html = include_str!("../fixtures/nexusphp.html");
    let site = site_with_profile("demo", Some("ws://example"));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let fetcher = RecordingFetcher::new(
        HashMap::from([(format!("search:{}", site.id), Ok(html.to_string()))]),
        requests.clone(),
    );
    let indexer = Indexer::new(
        ProfileSet::load(Some(overlay.path())).unwrap(),
        Arc::new(fetcher),
    );
    let outcome = indexer.search(&[site], "matrix");
    assert_eq!(outcome.torrents.len(), 1);
    let captured = requests.lock().unwrap();
    assert_eq!(captured[0].cdp_url.as_deref(), Some("ws://example"));
}

#[test]
fn browser_with_external_cdp_propagates_injected_connection_failure() {
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |endpoint| {
        assert_eq!(endpoint, Some("http://fixture.invalid:9222"));
        Err(IndexerError::Fetch("fixture CDP connection failed".into()))
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://fixture.invalid/document".into(),
        method: indexer::FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://fixture.invalid:9222".into()),
    };
    // Open failures happen before navigation, so they surface as BrowserSetup.
    let err = browser.fetch_html(&req).unwrap_err();
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
    assert!(err.to_string().contains("fixture CDP connection failed"));
}

#[test]
fn render_returns_html_and_read_back_cookie() {
    struct TestCookiePage;
    impl PageSession for TestCookiePage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("<html>ok</html>".into())
        }
        fn cookie_header(&self, _: &str) -> Result<String, IndexerError> {
            Ok("session=123".into())
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(TestCookiePage) as Arc<dyn PageSession>)
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://example.org/search".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: Some("session=old".into()),
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    };
    let res = browser.render(&req).unwrap();
    assert_eq!(res.html, "<html>ok</html>");
    assert_eq!(res.cookie_header.as_deref(), Some("session=123"));
}

#[test]
fn target_is_closed_when_goto_fails() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_clone = closed.clone();
    struct FailGotoPage(Arc<std::sync::atomic::AtomicBool>);
    impl PageSession for FailGotoPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            Err(IndexerError::Fetch("navigation rejected".into()))
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("".into())
        }
    }
    impl Drop for FailGotoPage {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(FailGotoPage(closed_clone.clone())) as Arc<dyn PageSession>)
    });
    let req = FetchRequest {
        key: "test".into(),
        url: "https://example.org/search".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    };
    assert!(browser.render(&req).is_err());
    assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn render_respects_the_total_deadline_and_closes_the_target() {
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_c = closed.clone();

    struct TimeoutPage {
        closed: Arc<std::sync::atomic::AtomicBool>,
        deadline: Mutex<Option<Instant>>,
    }
    impl PageSession for TimeoutPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(std::time::Duration::from_millis(650));
            if let Some(d) = *self.deadline.lock().unwrap()
                && Instant::now() > d
            {
                return Err(IndexerError::Fetch("deadline exceeded".into()));
            }
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            std::thread::sleep(std::time::Duration::from_millis(650));
            if let Some(d) = *self.deadline.lock().unwrap()
                && Instant::now() > d
            {
                return Err(IndexerError::Fetch("deadline exceeded".into()));
            }
            Ok("ok".into())
        }
        fn set_deadline(&self, deadline: Instant) {
            *self.deadline.lock().unwrap() = Some(deadline);
        }
    }
    impl Drop for TimeoutPage {
        fn drop(&mut self) {
            self.closed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(TimeoutPage {
            closed: closed_c.clone(),
            deadline: Mutex::new(None),
        }) as Arc<dyn PageSession>)
    })
    .with_render_deadline(std::time::Duration::from_millis(1200))
    .with_queue_cap(std::time::Duration::from_millis(1000))
    .with_min_work(std::time::Duration::from_millis(100));

    let start = std::time::Instant::now();
    let res = browser.render(&FetchRequest {
        key: "t".into(),
        url: "https://timeout.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    assert!(res.is_err());
    assert!(start.elapsed() < std::time::Duration::from_millis(2500));
    assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn render_rejects_proxy_before_opening_a_target() {
    let opened = Arc::new(Mutex::new(0));
    let o = opened.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        *o.lock().unwrap() += 1;
        Err(IndexerError::BrowserSetup("unused".into()))
    });
    let err = browser
        .render(&plain_req(Some("http://proxy:8080")))
        .unwrap_err();
    assert!(matches!(err, IndexerError::BrowserSetup(_)), "{err:?}");
    assert_eq!(*opened.lock().unwrap(), 0);
}

#[test]
fn cookie_injection_failure_is_setup_but_content_failure_is_not() {
    struct P(bool);
    impl PageSession for P {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            if self.0 {
                Err(IndexerError::Fetch("setCookies".into()))
            } else {
                Ok(())
            }
        }
        fn content(&self) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("after goto".into()))
        }
    }
    let b = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(P(true)) as Arc<dyn PageSession>)
    });
    assert!(matches!(
        b.render(&plain_req(None)),
        Err(IndexerError::BrowserSetup(_))
    ));
    let b = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(P(false)) as Arc<dyn PageSession>)
    });
    assert!(matches!(
        b.render(&plain_req(None)),
        Err(IndexerError::Fetch(_))
    ));
}
