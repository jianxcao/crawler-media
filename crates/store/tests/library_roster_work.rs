//! Exact roster aggregates stay linear in own rows; covering reads avoid table lookups.
use domain::MediaKind;
use rusqlite::{Connection, params};
use std::time::Instant;
use store::{Store, stats::StatsScope};

fn seed(store: &Store, lib: &str, tag: i64, from: i64, to: i64) {
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id == lib)
        .unwrap()
        .id;
    let mut conn = Connection::open(store.data_dir().join("library.db")).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute("WITH RECURSIVE n(x) AS (SELECT ?1 UNION ALL SELECT x+1 FROM n WHERE x<?2) INSERT OR IGNORE INTO media(media_id,kind,title,identity_state) SELECT printf('%08x-0000-0000-0000-000000000000',x),'movie',printf('Movie %d',x),'provisional' FROM n",params![from,to]).unwrap();
    tx.execute("WITH RECURSIVE n(x) AS (SELECT ?1 UNION ALL SELECT x+1 FROM n WHERE x<?2) INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,size_bytes,quality_source,confidence) SELECT printf('%08x-0000-0000-%04x-000000000000',x,?5),?3,?4,printf('%08x-0000-0000-0000-000000000000',x),'movie',printf('/p1/%d/%d.%s',?5,x,CASE WHEN x%2=0 THEN 'strm' ELSE 'mkv' END),printf('/p1/%d/%d',?5,x),'present','published',1,'v1',100,'probe','high' FROM n",params![from,to,lib,root,tag]).unwrap();
    tx.execute("WITH RECURSIVE n(x) AS (SELECT ?1 UNION ALL SELECT x+1 FROM n WHERE x<?2) INSERT INTO library_items(library_id,media_id,kind,title,title_sort,identity_state,metadata_revision,item_revision,added_at_sort,size_bytes,stable_shuffle_key,file_count,available_file_count,preferred_file_id) SELECT ?3,printf('%08x-0000-0000-0000-000000000000',x),'movie',printf('Movie %d',x),printf('movie %08d',x),'provisional',1,1,1000,CASE WHEN x%3=0 THEN NULL ELSE 100 END,'seed',1,1,printf('%08x-0000-0000-%04x-000000000000',x,?4) FROM n",params![from,to,lib,tag]).unwrap();
    tx.commit().unwrap();
}
fn measure(
    store: &Store,
    lib: &str,
    count: u64,
) -> store::library_query::items::stats::QueryWorkStats {
    let start = Instant::now();
    let (stats, work) = store.with_stats(StatsScope::Library, || store.library_roster_stats(lib));
    let stats = stats.unwrap();
    assert_eq!(stats.item_count, count);
    assert_eq!(stats.file_count, count);
    assert_eq!(stats.strm_file_count, count / 2);
    assert_eq!(stats.total_size_bytes, (count - count / 3) * 100);
    eprintln!(
        "P1 count={count} read_ms={:.3} work={work:?}",
        start.elapsed().as_secs_f64() * 1000.
    );
    work
}
#[test]
fn exact_roster_aggregates_use_bounded_covering_work_per_own_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let lib = store
        .create_library(MediaKind::Movie, "P1", &["/p1/1"], "everyone", true, &[])
        .unwrap();
    seed(&store, &lib.id, 1, 1, 1000);
    let small = measure(&store, &lib.id, 1000);
    seed(&store, &lib.id, 1, 1001, 100_000);
    let large = measure(&store, &lib.id, 100_000);
    assert!(
        large.vm_steps <= 1_400_100,
        "covering aggregates should avoid per-row table access: {large:?}"
    );
    let other = store
        .create_library(MediaKind::Movie, "Other", &["/p1/2"], "everyone", true, &[])
        .unwrap();
    seed(&store, &other.id, 2, 1, 100_000);
    let unrelated = measure(&store, &lib.id, 100_000);
    assert!(unrelated.vm_steps <= large.vm_steps + 100);
    assert_eq!(unrelated.sql_rows, large.sql_rows);
    assert_eq!(
        unrelated.sql_result_value_bytes,
        large.sql_result_value_bytes
    );
    assert!(
        large.vm_steps > small.vm_steps * 50,
        "exact counts still scan own rows"
    );
    let conn = Connection::open(tmp.path().join("library.db")).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let bytes = std::fs::metadata(tmp.path().join("library.db"))
        .unwrap()
        .len();
    let index_bytes:i64=conn.query_row("SELECT sum(pgsize) FROM dbstat WHERE name IN ('items_roster_stats','files_roster_stats')",[],|r|r.get(0)).unwrap();
    eprintln!(
        "P1 fixture200000 items/files library_db_bytes={bytes} added_covering_index_bytes={index_bytes}"
    );
}
