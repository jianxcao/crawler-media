use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn http_poster_encodes_unicode_and_spaces_in_upstream_path() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        let mut request = [0; 2048];
        let length = stream.read(&mut request).unwrap();
        let request = std::str::from_utf8(&request[..length]).unwrap();
        assert!(
            request.starts_with("GET /%E7%81%AB%E7%BA%BF%20poster.jpg HTTP/1.1"),
            "{request}"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nimage")
            .unwrap();
    });
    let result = HttpPoster.get(&format!("http://{address}/火线 poster.jpg"));
    assert_eq!(result.unwrap(), b"image");
    server.join().unwrap();
}

struct PosterCatalog;
impl crate::catalog::Catalog for PosterCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: domain::MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://example.test/poster.jpg".into()))
    }
}

struct RenameFetch {
    video: std::path::PathBuf,
}
impl PosterFetch for RenameFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        let _ = std::fs::remove_file(&self.video);
        Err("poster download failed after rename".into())
    }
}

struct EmptyFetcher;
impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unused".into()))
    }
}

#[test]
fn failed_poster_download_does_not_frame_a_vanished_video() {
    let tmp = tempfile::tempdir().unwrap();
    let video = tmp.path().join("Movie.mkv");
    std::fs::write(&video, b"video").unwrap();
    let called = AtomicBool::new(false);
    let state = ApiState::new(
        crate::Store::open(tmp.path().join("data")).unwrap(),
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
        tmp.path().join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(PosterCatalog))
    .with_poster_fetch(Arc::new(RenameFetch {
        video: video.clone(),
    }));
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Movie".into(),
        year: Some(2023),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("1".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let written = attach_poster_with(&state, &media, &video, true, |_| {
        called.store(true, Ordering::SeqCst);
        panic!("ffmpeg must not run after the poster source disappears");
    });
    assert_eq!(written, ArtworkOutcome::Skipped);
    assert!(!called.load(Ordering::SeqCst));
    assert!(!video.exists());
    assert!(!tmp.path().join("poster.jpg").exists());
}

struct NoCatalog;
impl crate::catalog::Catalog for NoCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
}

fn test_state(tmp: &std::path::Path) -> ApiState {
    ApiState::new(
        crate::Store::open(tmp.join("data")).unwrap(),
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(tmp.join("stage"))),
        tmp.join("library"),
    )
    .unwrap()
    .with_catalog(Arc::new(NoCatalog))
}

fn test_media() -> domain::Media {
    domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Test Movie".into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn published_movie(
    state: &ApiState,
    tmp: &std::path::Path,
) -> (store::Library, domain::Media, std::path::PathBuf) {
    let root = tmp.join("homevideos");
    let directory = root.join("Movie");
    std::fs::create_dir_all(&directory).unwrap();
    let library = state
        .store()
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let video = directory.join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let media = test_media();
    // Thumbnail policy needs a recorded Library, so publish the fixture file.
    {
        let store = state.store();
        let guard = &store;
        guard.insert_media(&media).unwrap();
        store::test_support::publish_fixture_file(
            &guard,
            &store::test_support::FixtureFile {
                file_id: Some(domain::LedgerId::new()),
                media_id: media.id,
                path: video.display().to_string(),
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                quality_source: domain::QualitySource::Probe,
                confidence: domain::Confidence::High,
                ..Default::default()
            },
        )
        .unwrap();
    }
    (library, media, video)
}

#[test]
fn frame_non_empty_but_write_fails_returns_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let (_, media, video) = published_movie(&state, tmp.path());
    // 把目标 poster.jpg 建成目录使 std::fs::write 必然失败
    std::fs::create_dir(video.with_file_name("poster.jpg")).unwrap();
    let outcome = attach_poster_with(&state, &media, &video, true, |_| {
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert_eq!(outcome, ArtworkOutcome::Failed);
}

#[test]
fn frame_after_source_vanishes_returns_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let (_, media, video) = published_movie(&state, tmp.path());
    let called = AtomicBool::new(false);
    let outcome = attach_poster_with(&state, &media, &video, true, |video| {
        called.store(true, Ordering::SeqCst);
        std::fs::remove_file(video).unwrap();
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert!(called.load(Ordering::SeqCst), "frame callback must run");
    assert_eq!(outcome, ArtworkOutcome::Skipped);
    assert!(!video.exists());
    assert!(!video.with_file_name("poster.jpg").exists());
}

#[test]
fn frame_after_cancelled_publication_returns_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let (library, media, video) = published_movie(&state, tmp.path());
    let target = video.with_file_name("poster.jpg");
    let called = AtomicBool::new(false);
    let called_ref = &called;
    let state_ref = &state;
    let library_id = library.id.clone();
    let outcome = attach_poster_with(&state, &media, &video, true, move |_| {
        called_ref.store(true, Ordering::SeqCst);
        state_ref.library_work().cancel(&library_id);
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert!(called.load(Ordering::SeqCst), "frame 闭包必须被执行");
    assert_eq!(outcome, ArtworkOutcome::Failed);
    assert!(!target.exists(), "已取消的工作不得写入海报目标文件");
}

#[test]
fn frame_without_recorded_library_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let video = tmp.path().join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let outcome = attach_poster_with(&state, &test_media(), &video, true, |_| {
        panic!("an unrecorded file has no Library thumbnail policy");
    });
    assert_eq!(outcome, ArtworkOutcome::Skipped);
    assert!(!video.with_file_name("poster.jpg").exists());
}

#[test]
fn frame_uses_recorded_library_policy_when_the_path_matches_another_library() {
    let tmp = tempfile::tempdir().unwrap();
    let state = test_state(tmp.path());
    let (library, media, video) = published_movie(&state, tmp.path());
    let other_root = tmp.path().join("other");
    std::fs::create_dir_all(&other_root).unwrap();
    let other_root = std::fs::canonicalize(other_root).unwrap();
    let moved = other_root.join("Movie.mp4");
    std::fs::rename(&video, &moved).unwrap();
    {
        let store = state.store();
        let guard = &store;
        let other = guard
            .create_library(
                domain::MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        guard
            .set_library_switch_settings(&other.id, None, Some(false), None, None, None)
            .unwrap();
        // Simulate an inconsistent recorded path without republishing ownership.
        assert!(
            store::test_support::force_file_path(
                &guard,
                &video.to_string_lossy(),
                &moved.to_string_lossy()
            )
            .unwrap()
        );
        assert_eq!(
            guard
                .file_coverage_by_path(&moved.to_string_lossy())
                .unwrap()
                .unwrap()
                .file
                .library_id
                .to_string(),
            library.id
        );
    }
    let outcome = attach_poster_with(&state, &media, &moved, true, |_| {
        Ok(b"fake-frame-bytes".to_vec())
    });
    assert_eq!(outcome, ArtworkOutcome::Written);
    assert_eq!(
        std::fs::read(moved.with_file_name("poster.jpg")).unwrap(),
        b"fake-frame-bytes"
    );
}

#[test]
fn library_lease_publish_after_cancelled_control_returns_err() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let state = test_state(tmp.path());
    let _ = state
        .store()
        .create_library(
            domain::MediaKind::Movie,
            "Movies",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let video = root.join("Movie.mp4");
    std::fs::write(&video, b"video-data").unwrap();
    let media = test_media();
    let lease = state
        .acquire_path_work(&video, domain::MediaKind::Movie)
        .unwrap()
        .unwrap();
    lease.control().cancel();
    let outcome = lease.publish(|| {
        Ok(write_artwork(
            &media,
            &root.join("poster.jpg"),
            "海报",
            b"fake-frame-bytes",
        ))
    });
    assert!(
        outcome.is_err(),
        "底层租约取消后 lease.publish 必须返回 Err"
    );
}
