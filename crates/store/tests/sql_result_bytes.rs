//! Logical SQL result volume is observable independently of decoded Rust values.
use store::{Db, Store, stats::StatsScope};

fn sample(store: &Store, text: &str, blob: &[u8]) -> (u64, u64) {
    let (result, stats) = store.with_stats(StatsScope::Library, || {
        store.writers().with_writer(Db::Library, |db| {
            db.query_row("SELECT ?1, ?2, 42, 1.25, NULL", (text, blob), |row| {
                assert_eq!(row.get::<_, String>(0)?, text);
                assert_eq!(row.get::<_, Vec<u8>>(1)?, blob);
                Ok(())
            })?;
            Ok(())
        })
    });
    result.unwrap();
    let json = serde_json::to_value(stats).unwrap();
    let bytes = json["sql_result_value_bytes"].as_u64().expect(
        "SQL diagnostics must expose logical result bytes: UTF-8 TEXT/BLOB lengths, numeric 8, NULL 0",
    );
    (bytes, stats.sql_rows)
}

#[test]
fn public_stats_report_exact_sql_result_value_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let small = sample(&store, "媒", &[0, 1, 2]);
    let large_text = "媒体🎞".repeat(1000);
    let large_blob = vec![0x81; 4096];
    let large = sample(&store, &large_text, &large_blob);
    assert_eq!(small, (3 + 3 + 16, 1));
    assert_eq!(
        large,
        ((large_text.len() + large_blob.len() + 16) as u64, 1)
    );
    let ((_, inner), outer) = store.with_stats(StatsScope::Library, || {
        store.with_stats(StatsScope::Library, || sample(&store, "媒", &[1, 2, 3]))
    });
    assert_eq!(
        serde_json::to_value(inner).unwrap()["sql_result_value_bytes"],
        22
    );
    assert_eq!(
        serde_json::to_value(outer).unwrap()["sql_result_value_bytes"],
        22
    );
    assert_eq!(sample(&store, "媒", &[0, 1, 2]), small);
}

#[test]
fn explicit_scopes_include_subscribe_and_cache_without_changing_both() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    for (scope, expected) in [
        (StatsScope::App, 8),
        (StatsScope::Library, 8),
        (StatsScope::Subscribe, 8),
        (StatsScope::Cache, 8),
        (StatsScope::Both, 16),
        (StatsScope::All, 32),
    ] {
        let (_, stats) = store.with_stats(scope, || {
            for db in [Db::App, Db::Subscribe, Db::Library, Db::Cache] {
                store
                    .writers()
                    .with_writer(db, |conn| {
                        conn.query_row("SELECT 1", [], |_| Ok(()))?;
                        Ok(())
                    })
                    .unwrap();
            }
        });
        assert_eq!(stats.sql_result_value_bytes, expected, "{scope:?}");
        assert_eq!(stats.sql_rows, expected / 8);
    }
}
