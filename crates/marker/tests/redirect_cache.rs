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
