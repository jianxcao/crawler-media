//! Orthogonal release representatives for the remaining local §9.2 axes.
//! AXIS_SIZE=100000 cargo test --release -p store --test query_model_axes_perf -- --ignored --nocapture --test-threads=1
use domain::{LibraryId, MediaKind, ProtocolParent, ProtocolSort, ProtocolSortKey};
use std::time::Instant;
use store::{
    Store,
    library_query::{
        BrowseFilter, BrowseOrder, BrowsePage, FacetRequest, FacetTier, LibraryQuery,
        items::QueryWorkStats,
    },
};
#[path = "query_model_axes_perf/actors.rs"]
mod actors;
#[path = "query_model_axes_perf/hierarchy.rs"]
mod hierarchy;
#[path = "query_model_axes_perf/metadata.rs"]
mod metadata;
#[path = "query_model_axes_perf/ordering.rs"]
mod ordering;
#[path = "query_model_axes_perf/overlap_user.rs"]
mod overlap_user;
#[path = "query_work/support.rs"]
mod support;

fn fixed_library(store: &Store, kind: MediaKind, label: &str) -> LibraryId {
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, label.as_bytes()).to_string();
    let root_id = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("{label}-root").as_bytes(),
    )
    .to_string();
    let path = format!("/query-work/{}", kind.as_str());
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute("INSERT INTO libraries(library_id,kind,name,is_default,access_mode,admin_visible) VALUES(?1,?2,'Axis',0,'everyone',1)",rusqlite::params![id,kind.as_str()]).unwrap();
    db.execute(
        "INSERT INTO library_roots(root_id,library_id,path_key,display_path) VALUES(?1,?2,?3,?3)",
        rusqlite::params![root_id, id, path],
    )
    .unwrap();
    id.parse().unwrap()
}

fn media_id(kind: MediaKind, index: usize) -> domain::MediaId {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("query-media-{}-{index}", kind.as_str()).as_bytes(),
    )
    .to_string()
    .parse()
    .unwrap()
}

fn repair(store: &Store, library: LibraryId, label: &str) {
    let op = format!("axis-{label}-{}", uuid::Uuid::new_v4());
    store
        .begin_library_integrity(library, true, &op, 5_000)
        .unwrap();
    loop {
        let state = store.advance_library_operation(&op, 5_001, 100).unwrap();
        if state.state != "applying" {
            assert_eq!(state.state, "succeeded", "{label}");
            break;
        }
    }
}

fn measure(label: &str, expected: u64, mut query: impl FnMut() -> (u64, QueryWorkStats)) {
    let started = Instant::now();
    let (first_total, _) = query();
    let first_ms = started.elapsed().as_secs_f64() * 1_000.;
    assert_eq!(first_total, expected, "{label} first");
    for _ in 0..5 {
        assert_eq!(query().0, expected, "{label} warmup");
    }
    let mut values = Vec::with_capacity(50);
    let mut work = None;
    for _ in 0..50 {
        let started = Instant::now();
        let (total, stats) = query();
        values.push(started.elapsed().as_secs_f64() * 1_000.);
        assert_eq!(total, expected, "{label}");
        if let Some(previous) = &work {
            let previous: &QueryWorkStats = previous;
            assert_eq!(stats.vm_steps, previous.vm_steps, "{label} VM drift");
            assert_eq!(
                stats.statements, previous.statements,
                "{label} statement drift"
            );
        }
        work = Some(stats);
    }
    values.sort_by(f64::total_cmp);
    println!(
        "{label}\ttotal={expected}\tfirst_ms={first_ms:.3}\tp50_ms={:.3}\tp95_ms={:.3}\tmax_ms={:.3}\t{:?}",
        values[24],
        values[47],
        values[49],
        work.unwrap()
    );
}

fn db_sizes(store: &Store, label: &str) {
    let path = store.data_dir().join("library.db");
    println!(
        "{label} db_bytes={} wal_bytes={:?}",
        std::fs::metadata(&path).unwrap().len(),
        std::fs::metadata(format!("{}-wal", path.display()))
            .map(|m| m.len())
            .ok()
    );
}

#[test]
#[ignore = "manual sequential release matrix"]
fn orthogonal_matrix() {
    let high_end_size = std::env::var("AXIS_SIZE")
        .unwrap_or_else(|_| "100000".into())
        .parse()
        .unwrap();
    metadata::run(high_end_size);
    actors::run(1_000);
    overlap_user::overlap(1_000);
    overlap_user::users(1_000);
    hierarchy::run(1_000);
    ordering::run();
}
