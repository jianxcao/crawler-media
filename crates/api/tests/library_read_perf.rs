//! Fixed local release 2R1W. Tracing off HTTP and diagnostic SQL are separate runs.
//! cargo test --release -p api --test library_read_perf -- --ignored --nocapture
use api::router;
use media_server::query_metrics::QueryMetricsSnapshot;
use std::sync::Arc;
use std::time::Instant;
#[path = "library_read/support.rs"]
mod support;

#[derive(Default)]
struct Measurements {
    readers: [Vec<(f64, QueryMetricsSnapshot)>; 2],
    writer: Vec<(f64, f64, f64)>,
    wal_max: u64,
    checkpoint: (i64, i64, i64),
}
fn wal_bytes(path: &std::path::Path) -> u64 {
    std::fs::metadata(format!("{}-wal", path.display()))
        .map(|m| m.len())
        .unwrap_or(0)
}
fn rss_kib() -> String {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn distribution(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    serde_json::json!({"p50":values[(n-1)/2], "p95":values[(n*95).div_ceil(100)-1], "max":values[n-1]})
}
impl Measurements {
    fn report(&self, label: &str, diagnostic: bool) {
        for (index, samples) in self.readers.iter().enumerate() {
            let mut row = serde_json::json!({"label":label,"diagnostic":diagnostic,"reader":index+1,
                "http_ms":distribution(samples.iter().map(|s|s.0).collect()),
                "pool_rejections":samples.iter().map(|s|s.1.pool_rejections).sum::<u64>(),
                "timeouts":samples.iter().map(|s|s.1.timeouts).sum::<u64>()});
            for (name, field) in metric_fields() {
                row[name] = distribution(samples.iter().map(|s| field(&s.1)).collect());
            }
            println!("summary {row}");
        }
        println!(
            "summary {}",
            serde_json::json!({"label":label,"diagnostic":diagnostic,
            "writer_ms":distribution(self.writer.iter().map(|v|v.0).collect()),
            "writer_store_wait_ms":distribution(self.writer.iter().map(|v|v.1).collect()),
            "writer_store_held_ms":distribution(self.writer.iter().map(|v|v.2).collect()),
            "wal_file_max_bytes":self.wal_max,"post_round_passive_checkpoint":self.checkpoint,"rss_kib_end":rss_kib()})
        );
    }
}
type MetricField = (&'static str, fn(&QueryMetricsSnapshot) -> f64);
fn metric_fields() -> Vec<MetricField> {
    vec![
        ("auth_ms", |s| s.auth_ms),
        ("auth_lock_wait_ms", |s| s.auth_lock_wait_ms),
        ("credential_binding_ms", |s| s.credential_binding_ms),
        ("queue_wait_ms", |s| s.queue_wait_ms),
        ("prepare_lock_wait_ms", |s| s.prepare_lock_wait_ms),
        ("prepare_lock_held_ms", |s| s.prepare_lock_held_ms),
        ("sql_ms", |s| s.sql_ms),
        ("validation_lock_wait_ms", |s| s.validation_lock_wait_ms),
        ("validation_lock_held_ms", |s| s.validation_lock_held_ms),
        ("cold_ms", |s| s.cold_ms),
    ]
}

async fn round(
    fixture: &mut support::Fixture,
    uris: &[String; 2],
    diagnostic: bool,
    batch: usize,
) -> ([(f64, u16, QueryMetricsSnapshot); 2], (f64, f64, f64)) {
    let app = router(fixture.state.clone());
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let reader = |index: usize| {
        let barrier = barrier.clone();
        let app = app.clone();
        let uri = uris[index].clone();
        async move {
            barrier.wait().await;
            support::read(app, uri, diagnostic).await
        }
    };
    let state = fixture.state.clone();
    let mut files = fixture.files.clone();
    let writer_barrier = barrier.clone();
    let writer = async move {
        writer_barrier.wait().await;
        tokio::task::spawn_blocking(move || support::write_batch(&state, &mut files, batch))
            .await
            .unwrap()
    };
    let (first, second, writer) = tokio::join!(reader(0), reader(1), writer);
    ([first, second], writer)
}

async fn measure(size: usize, shape: &str, diagnostic: bool) {
    let seed = Instant::now();
    let mut fixture = support::Fixture::new(size);
    let lib = fixture.library;
    let (uris, expected) = if shape == "page" {
        (
            [
                format!("/api/v1/libraries/{lib}/items?limit=100"),
                format!(
                    "/Items?ParentId={lib}&Recursive=true&IncludeItemTypes=Movie&SortBy=SortName&Limit=100"
                ),
            ],
            size,
        )
    } else {
        (
            [
                format!("/api/v1/libraries/{lib}/facets?g=Drama"),
                format!(
                    "/Items?ParentId={lib}&Recursive=true&IncludeItemTypes=Movie&Genres=Drama&SortBy=SortName&Limit=100"
                ),
            ],
            size / 2,
        )
    };
    let label = format!("{shape}_{size}");
    println!(
        "fixture {}",
        serde_json::json!({"label":label,"diagnostic":diagnostic,"seed_repair_ms":seed.elapsed().as_secs_f64()*1000.,
        "media":size,"files":size,"users":1,"playback":0,"libraries":3,"writer_batch_files":100,"writer_distinct_media":100,
        "write_mutation":"size_bytes=round+1 via one publish_files transaction, source version unchanged",
        "uris":uris,"db_bytes":std::fs::metadata(fixture.database()).unwrap().len(),"wal_file_bytes":wal_bytes(&fixture.database()),"rss_kib_postsetup":rss_kib()})
    );
    let mut measured = Measurements::default();
    for batch in 0..56 {
        let (readers, writer) = round(&mut fixture, &uris, diagnostic, batch).await;
        for (index, (http, status, metrics)) in readers.into_iter().enumerate() {
            assert_eq!(status, 200, "{label} reader {index} batch {batch}");
            assert_eq!(metrics.total, Some(expected as u64), "{label} {metrics:?}");
            assert_eq!(metrics.vm_steps.is_some(), diagnostic);
            println!(
                "sample {}",
                serde_json::json!({"label":label,"diagnostic":diagnostic,"batch":batch,"reader":index+1,"http_ms":http,"metrics":metrics})
            );
            if batch == 0 {
                println!(
                    "first {label} diagnostic={diagnostic} reader={} http_ms={http:.3}",
                    index + 1
                );
            }
            if batch >= 6 {
                measured.readers[index].push((http, metrics));
            }
        }
        println!(
            "writer_sample {}",
            serde_json::json!({"label":label,"diagnostic":diagnostic,"batch":batch,"elapsed_ms":writer.0,"store_wait_ms":writer.1,"store_held_ms":writer.2})
        );
        if batch == 0 {
            println!(
                "first {label} diagnostic={diagnostic} writer_ms={:.3}",
                writer.0
            );
        }
        if batch >= 6 {
            measured.writer.push(writer);
        }
        measured.wal_max = measured.wal_max.max(wal_bytes(&fixture.database()));
        measured.checkpoint = support::checkpoint(&fixture.database(), "PASSIVE");
    }
    measured.report(&label, diagnostic);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "manual local release 2R1W measurement"]
async fn two_readers_one_writer() {
    println!(
        "environment os={} arch={} sqlite={} rss=ps_rss_KiB process_wide warm_sqlite_cache",
        std::env::consts::OS,
        std::env::consts::ARCH,
        rusqlite::version()
    );
    for diagnostic in [false, true] {
        for size in std::env::var("PERF_SIZES")
            .unwrap_or_else(|_| "5000,100000".into())
            .split(',')
            .map(|v| v.parse().unwrap())
        {
            for shape in ["page", "genre"] {
                measure(size, shape, diagnostic).await;
            }
        }
    }
}
