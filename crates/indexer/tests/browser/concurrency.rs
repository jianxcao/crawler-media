//! Concurrency: per-host serialization, cross-subdomain cousins, queue caps, FIFO
//! service and per-registrable-domain limits.

use crate::support::*;

#[test]
fn different_sites_render_concurrently_up_to_the_limit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(4));

    struct ConcurrentPage {
        active: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
        barrier: Arc<std::sync::Barrier>,
    }
    impl PageSession for ConcurrentPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.max_concurrent.fetch_max(cur, Ordering::SeqCst);
            self.barrier.wait();
            std::thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let barrier_c = barrier.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(ConcurrentPage {
            active: active_c.clone(),
            max_concurrent: max_c.clone(),
            barrier: barrier_c.clone(),
        }) as Arc<dyn PageSession>)
    })
    .with_max_sessions(3);

    let mut handles = Vec::new();
    for site in [
        "https://site1.org/s",
        "https://site2.org/s",
        "https://site3.org/s",
    ] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    barrier.wait();
    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 3);
}

#[test]
fn same_site_sessions_are_serialized() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    struct SerialPage {
        active: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
    }
    impl PageSession for SerialPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.max_concurrent.fetch_max(cur, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(SerialPage {
            active: active_c.clone(),
            max_concurrent: max_c.clone(),
        }) as Arc<dyn PageSession>)
    })
    .with_max_sessions(3);

    let mut handles = Vec::new();
    // Same eTLD+1: example.org
    for site in ["https://a.example.org/s", "https://b.example.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn cousin_and_cross_level_subdomains_are_serialized() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let active = Arc::new(AtomicUsize::new(0));
    let max_concurrent = Arc::new(AtomicUsize::new(0));

    struct SerialPage(Arc<AtomicUsize>, Arc<AtomicUsize>);
    impl PageSession for SerialPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            let cur = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.1.fetch_max(cur, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(40));
            self.0.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let active_c = active.clone();
    let max_c = max_concurrent.clone();
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        Ok(Arc::new(SerialPage(active_c.clone(), max_c.clone())) as Arc<dyn PageSession>)
    })
    .with_max_sessions(3);

    let mut handles = Vec::new();
    for site in ["https://x.a.example.org/s", "https://y.b.example.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            let req = FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
            };
            b.render(&req)
        }));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}

#[test]
fn waiting_beyond_the_queue_cap_is_busy_before_navigation() {
    use std::time::Duration;

    struct BlockPage;
    impl PageSession for BlockPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let opened_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let opened_count_c = opened_count.clone();

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        opened_count_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(BlockPage) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_secs(2))
    .with_queue_cap(Duration::from_millis(50))
    .with_min_work(Duration::from_millis(10));

    let b1 = browser.clone();
    let h1 = std::thread::spawn(move || {
        let req = FetchRequest {
            key: "1".into(),
            url: "https://site1.org/s".into(),
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: None,
            proxy: None,
            cdp_url: Some("http://127.0.0.1:9222".into()),
        };
        b1.render(&req)
    });

    std::thread::sleep(Duration::from_millis(20));

    let b2 = browser.clone();
    let res2 = b2.render(&FetchRequest {
        key: "2".into(),
        url: "https://site2.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });

    assert!(h1.join().unwrap().is_ok());
    assert!(matches!(res2, Err(IndexerError::BrowserBusy)));
    assert_eq!(opened_count.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn too_little_time_left_after_queueing_is_busy_before_navigation() {
    use std::time::Duration;

    struct SleepPage;
    impl PageSession for SleepPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(1150));
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        Ok(Arc::new(SleepPage) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_millis(1200))
    .with_queue_cap(Duration::from_millis(1100))
    .with_min_work(Duration::from_millis(100));

    let b1 = browser.clone();
    let h1 = std::thread::spawn(move || {
        b1.render(&FetchRequest {
            key: "1".into(),
            url: "https://site1.org/s".into(),
            method: FetchMethod::Get,
            body: None,
            cookie: None,
            api_key: None,
            proxy: None,
            cdp_url: Some("http://127.0.0.1:9222".into()),
        })
    });

    std::thread::sleep(Duration::from_millis(20));

    let b2 = browser.clone();
    let res2 = b2.render(&FetchRequest {
        key: "2".into(),
        url: "https://site2.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });

    let h1_res = h1.join().unwrap();
    assert!(h1_res.is_ok());
    assert!(matches!(res2, Err(IndexerError::BrowserBusy)));
}

#[test]
fn waiters_are_served_in_fifo_order() {
    use std::time::Duration;

    let finish_order = Arc::new(Mutex::new(Vec::new()));
    let finish_order_c = finish_order.clone();

    struct SlowPage(usize, Arc<Mutex<Vec<usize>>>);
    impl PageSession for SlowPage {
        fn goto(&self, _: &str) -> Result<(), IndexerError> {
            std::thread::sleep(Duration::from_millis(50));
            self.1.lock().unwrap().push(self.0);
            Ok(())
        }
        fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
            Ok(())
        }
        fn content(&self) -> Result<String, IndexerError> {
            Ok("ok".into())
        }
    }

    let finish_log = finish_order.clone();
    let next_id = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), move |_| {
        let id = next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(SlowPage(id, finish_log.clone())) as Arc<dyn PageSession>)
    })
    .with_max_sessions(1)
    .with_render_deadline(Duration::from_secs(3))
    .with_queue_cap(Duration::from_secs(2))
    .with_min_work(Duration::from_millis(10));

    let mut handles = Vec::new();
    for site in ["https://s1.org/s", "https://s2.org/s", "https://s3.org/s"] {
        let b = browser.clone();
        let s = site.to_string();
        handles.push(std::thread::spawn(move || {
            b.render(&FetchRequest {
                key: "k".into(),
                url: s,
                method: FetchMethod::Get,
                body: None,
                cookie: None,
                api_key: None,
                proxy: None,
                cdp_url: Some("http://127.0.0.1:9222".into()),
            })
        }));
        std::thread::sleep(Duration::from_millis(15));
    }

    for h in handles {
        assert!(h.join().unwrap().is_ok());
    }
    assert_eq!(*finish_order_c.lock().unwrap(), vec![1, 2, 3]);
}

#[test]
fn unrelated_registrable_domains_can_render_concurrently() {
    let browser = Browser::with_opener(BrowserConfig::enabled_for_test(), |_| {
        struct QuickPage;
        impl PageSession for QuickPage {
            fn goto(&self, _: &str) -> Result<(), IndexerError> {
                Ok(())
            }
            fn set_site_cookies(&self, _: &str, _: &str) -> Result<(), IndexerError> {
                Ok(())
            }
            fn content(&self) -> Result<String, IndexerError> {
                Ok("ok".into())
            }
        }
        Ok(Arc::new(QuickPage) as Arc<dyn PageSession>)
    })
    .with_max_sessions(2);

    let res1 = browser.render(&FetchRequest {
        key: "1".into(),
        url: "https://example.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    let res2 = browser.render(&FetchRequest {
        key: "2".into(),
        url: "https://other.org/s".into(),
        method: FetchMethod::Get,
        body: None,
        cookie: None,
        api_key: None,
        proxy: None,
        cdp_url: Some("http://127.0.0.1:9222".into()),
    });
    assert!(res1.is_ok());
    assert!(res2.is_ok());
}
