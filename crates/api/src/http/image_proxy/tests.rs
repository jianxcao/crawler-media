use super::cache::ImageCache;
use std::sync::{Arc, Barrier};

fn image(bytes: usize) -> Result<(String, Vec<u8>), String> {
    Ok(("image/png".into(), vec![42; bytes]))
}

fn disk_bytes(root: &std::path::Path) -> u64 {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[test]
fn cache_reuses_complete_images_without_fetching_again() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert_eq!(cache.get("one", || image(20)).unwrap().1, vec![42; 20]);
    assert_eq!(
        cache.get("one", || Err("must not fetch".into())).unwrap().1,
        vec![42; 20]
    );
}

#[test]
fn oversized_response_is_rejected_without_cache_write() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert!(cache.get("large", || image(41)).is_err());
    assert_eq!(disk_bytes(tmp.path()), 0);
}

#[test]
fn eviction_makes_room_before_publication_and_failed_fetch_keeps_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::new(tmp.path().into(), 50, 40);
    cache.get("one", || image(30)).unwrap();
    cache.get("two", || image(30)).unwrap();
    assert!(disk_bytes(tmp.path()) <= 50);
    assert!(
        cache
            .get("failed", || Err("upstream failed".into()))
            .is_err()
    );
    assert!(disk_bytes(tmp.path()) <= 50);
    assert_eq!(
        cache
            .get("two", || Err("cache lost".into()))
            .unwrap()
            .1
            .len(),
        30
    );
}

#[test]
fn deletion_failure_rejects_new_image_without_exceeding_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ImageCache::with_remover(
        tmp.path().into(),
        50,
        40,
        Arc::new(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "fixture refuses deletion",
            ))
        }),
    );
    cache.get("one", || image(30)).unwrap();
    let error = cache.get("two", || image(30)).unwrap_err();
    assert!(error.contains("fixture refuses deletion"), "{error}");
    assert!(disk_bytes(tmp.path()) <= 50);
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn concurrent_writers_publish_complete_files_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 100, 40));
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for index in 0..8 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                cache
                    .get(&format!("image-{index}"), || {
                        barrier.wait();
                        image(30)
                    })
                    .unwrap();
            });
        }
    });
    assert!(disk_bytes(tmp.path()) <= 100);
    for entry in std::fs::read_dir(tmp.path()).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(entry.path().extension().unwrap(), "img");
        assert_eq!(
            std::fs::read(entry.path()).unwrap(),
            [b"image/png\n".as_slice(), &[42; 30]].concat()
        );
    }
}

#[test]
fn concurrent_same_url_writes_never_expose_partial_images() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(ImageCache::new(tmp.path().into(), 100, 40));
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                let result = cache
                    .get("same", || {
                        barrier.wait();
                        image(30)
                    })
                    .unwrap();
                assert_eq!(result.1, vec![42; 30]);
            });
        }
    });
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    assert_eq!(
        cache
            .get("same", || Err("must hit".into()))
            .unwrap()
            .1
            .len(),
        30
    );
}

#[test]
fn preexisting_over_budget_cache_cannot_succeed_when_eviction_fails() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("legacy.img"), vec![42; 80]).unwrap();
    let cache = ImageCache::with_remover(
        tmp.path().into(),
        50,
        40,
        Arc::new(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "eviction blocked",
            ))
        }),
    );
    assert!(
        cache
            .get("new", || image(10))
            .unwrap_err()
            .contains("eviction blocked")
    );
    assert_eq!(disk_bytes(tmp.path()), 80, "no additional bytes written");
}

#[test]
fn oversized_legacy_entry_is_evicted_before_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(format!("{}.img", super::cache_key("old")));
    std::fs::write(&path, [b"image/png\n".as_slice(), &[42; 41]].concat()).unwrap();
    let cache = ImageCache::new(tmp.path().into(), 100, 40);
    assert_eq!(cache.get("old", || image(20)).unwrap().1.len(), 20);
    assert_eq!(disk_bytes(tmp.path()), 30);
}

#[test]
fn upstream_body_reader_enforces_limit_without_content_length() {
    let body = ureq::Body::builder().reader(std::io::Cursor::new(vec![42; 41]));
    assert!(super::read_image_body(body, 40).is_err());
    let body = ureq::Body::builder().reader(std::io::Cursor::new(vec![42; 40]));
    assert_eq!(super::read_image_body(body, 40).unwrap().len(), 40);
}

#[tokio::test]
async fn saturated_proxy_returns_unavailable_without_fetching() {
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let _permit = gate.clone().acquire_owned().await.unwrap();
    let response = super::proxy_with_gate(
        super::ProxyQuery {
            url: "https://image.tmdb.org/test.png".into(),
        },
        gate,
    )
    .await;
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}
