use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use marker::redirect::RedirectCache;

/// Minimal HTTP server: every request gets `status` (+ Location for 302).
fn server(status: &'static str, location: Option<&'static str>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let location = location
                .map(|value| format!("Location: {value}\r\n"))
                .unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{location}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (format!("http://{addr}/api/302?id=1"), hits)
}

#[test]
fn redirect_target_is_resolved_once_and_reused_within_ttl() {
    let (url, hits) = server("302 Found", Some("http://cdn.invalid/file.mkv?sig=abc"));
    let cache = RedirectCache::new(Duration::from_secs(60));
    assert_eq!(cache.resolve(&url), "http://cdn.invalid/file.mkv?sig=abc");
    assert_eq!(cache.resolve(&url), "http://cdn.invalid/file.mkv?sig=abc");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn expired_entries_are_resolved_again() {
    let (url, hits) = server("302 Found", Some("http://cdn.invalid/a"));
    let cache = RedirectCache::new(Duration::ZERO);
    cache.resolve(&url);
    cache.resolve(&url);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn non_redirect_and_unreachable_sources_fall_back_to_the_original_url() {
    let (url, _) = server("200 OK", None);
    let cache = RedirectCache::new(Duration::from_secs(60));
    assert_eq!(cache.resolve(&url), url);
    let dead = "http://127.0.0.1:9/x.mkv";
    assert_eq!(cache.resolve(dead), dead);
}

#[test]
fn invalidated_entry_is_resolved_again() {
    let (url, hits) = server("302 Found", Some("http://cdn.invalid/b"));
    let cache = RedirectCache::new(Duration::from_secs(60));
    cache.resolve(&url);
    cache.invalidate(&url);
    cache.resolve(&url);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn direct_sources_reuse_the_observed_target_until_invalidation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/video.mkv", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let response = if index == 0 {
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 0\r\n\r\n"
            } else {
                "HTTP/1.1 302 Found\r\nLocation: http://cdn.invalid/new-target\r\nContent-Length: 0\r\n\r\n"
            };
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    let cache = RedirectCache::new(Duration::from_secs(60));
    assert_eq!(cache.resolve(&url), url);
    assert_eq!(
        cache.resolve(&url),
        url,
        "direct targets also need a reusable observation"
    );
    cache.invalidate(&url);
    assert_eq!(cache.resolve(&url), "http://cdn.invalid/new-target");
}

#[test]
fn rotating_targets_honor_no_store_and_short_max_age_and_invalidation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first_line = String::new();
            let _ = reader.read_line(&mut first_line);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
                line.clear();
            }
            let response = if first_line.contains("/no-store") {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://cdn.invalid/no-store-{n}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else {
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://cdn.invalid/short-{n}\r\nCache-Control: max-age=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });

    let cache = RedirectCache::new(Duration::from_secs(300));
    let no_store_url = format!("http://{addr}/no-store");
    let first = cache.resolve(&no_store_url);
    let second = cache.resolve(&no_store_url);
    assert_ne!(first, second, "no-store redirects must resolve every time");

    let short_url = format!("http://{addr}/short");
    let short_a = cache.resolve(&short_url);
    assert_eq!(cache.resolve(&short_url), short_a, "fresh within max-age=1");
    std::thread::sleep(Duration::from_millis(1100));
    let short_b = cache.resolve(&short_url);
    assert_ne!(short_a, short_b, "must re-resolve once max-age=1 expires");
}
