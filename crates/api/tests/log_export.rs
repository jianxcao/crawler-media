//! Log export preserves the entire download while bounding stream resources.
mod writer_hold {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/writer_hold.rs"
    ));
}
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tokio_stream::StreamExt;
use tower::ServiceExt;

const TOKEN: &str = "log-export-fixture";
const LARGE_SIZE: usize = 8 * 1024 * 1024 + 97;
struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    state: ApiState,
    path: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        std::fs::create_dir_all("/tmp/crawler-storage-fixes").unwrap();
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            TOKEN.into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let path = dir.path().join("data/logs/crawler-media.log.2026-10-30");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Self {
            _dir: dir,
            state,
            path,
        }
    }
    fn write_large(&self) {
        let mut file = std::fs::File::create(&self.path).unwrap();
        let block: Vec<u8> = (0..65536).map(|offset| (offset % 251) as u8).collect();
        let mut remaining = LARGE_SIZE;
        while remaining > 0 {
            let count = remaining.min(block.len());
            file.write_all(&block[..count]).unwrap();
            remaining -= count;
        }
    }
    async fn export(&self) -> axum::response::Response {
        api::router(self.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/logs/export")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }
}
fn assert_download(response: &axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    let disposition = response.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap();
    assert!(disposition.starts_with("attachment; filename=\"crawler-media-logs-"));
    assert!(disposition.ends_with(".txt\""));
}

#[test]
fn log_export_returns_complete_large_file_with_bounded_streaming() {
    let fx = Fixture::new();
    fx.write_large();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let started = Instant::now();
        let response = fx.export().await; assert_download(&response);
        assert_eq!(response.headers()[header::CONTENT_LENGTH].to_str().unwrap(), LARGE_SIZE.to_string());
        let mut stream = response.into_body().into_data_stream();
        let (mut total, mut chunks, mut first_byte) = (0usize, 0usize, None);
        while let Some(bytes) = stream.next().await {
            let bytes = bytes.unwrap();
            first_byte.get_or_insert_with(|| started.elapsed());
            assert!(bytes.len() <= 256 * 1024, "an export chunk must have a fixed memory bound");
            for (index, byte) in bytes.iter().enumerate() {
                assert_eq!(*byte, ((total + index) % 65536 % 251) as u8);
            }
            total += bytes.len(); chunks += 1;
            tokio::task::yield_now().await;
        }
        assert_eq!(total, LARGE_SIZE, "the entire selected log must be downloaded");
        assert!(chunks <= LARGE_SIZE.div_ceil(32 * 1024),
            "large exports must amortize stream scheduling, observed {chunks} chunks for {total} bytes");
        assert!(first_byte.is_some());
        // Timings are diagnostic; correctness and resource bounds are deterministic.
        eprintln!("generated log export bytes={total} chunks={chunks} first_ms={:.3} full_ms={:.3}",
            first_byte.unwrap().as_secs_f64()*1000., started.elapsed().as_secs_f64()*1000.);
    });
}

#[test]
fn log_export_does_not_wait_for_writer_or_block_async_worker() {
    let fx = Fixture::new();
    fx.write_large();
    let held = writer_hold::WriterHold::acquire(fx.state.store(), store::Db::Library);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let export = async {
            let response = fx.export().await;
            assert_download(&response);
            let mut stream = response.into_body().into_data_stream();
            let mut total = 0;
            while let Some(bytes) = stream.next().await {
                total += bytes.unwrap().len();
            }
            assert_eq!(total, LARGE_SIZE);
        };
        let responsive = async {
            for _ in 0..5 {
                let tick = Instant::now();
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(
                    tick.elapsed() < Duration::from_millis(250),
                    "export blocked the sole async worker"
                );
            }
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(export, responsive);
        })
        .await
        .expect("export must finish while the writer remains held");
    });
    drop(held);
}

#[test]
fn log_export_buffer_fallback_preserves_content() {
    let fx = Fixture::new();
    let first = fx
        .state
        .log_buffer
        .push("INFO", "fixture", "first generated message".into());
    let second =
        fx.state
            .log_buffer
            .push("WARN", "fixture", "second generated message 中文".into());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = fx.export().await;
        assert_download(&response);
        let actual = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let line = |entry: api::system_logs::SystemLogEntry| {
            format!(
                "[{}.{:03}] [{}] [{}] {}\n",
                entry.timestamp / 1000,
                entry.timestamp % 1000,
                entry.level,
                entry.target,
                entry.message
            )
        };
        let expected = format!("{}{}", line(first), line(second));
        assert_eq!(actual.as_ref(), expected.as_bytes());
    });
}

#[test]
fn log_export_file_growth_does_not_exceed_declared_snapshot_length() {
    let fx = Fixture::new();
    fx.write_large();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let response = fx.export().await;
        assert_download(&response);
        let declared: usize = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&fx.path)
            .unwrap()
            .write_all(b"later log entry\n")
            .unwrap();
        let bytes = to_bytes(response.into_body(), LARGE_SIZE + 1024)
            .await
            .unwrap();
        assert_eq!(
            bytes.len(),
            declared,
            "snapshot download must match advertised Content-Length while log grows"
        );
        assert_eq!(declared, LARGE_SIZE);
    });
}
