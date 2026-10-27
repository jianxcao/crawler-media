//! Store 四个库的**连接级**策略与**持久**文件头属性。
//!
//! `journal_mode=WAL` 与 `auto_vacuum` 写在数据库文件头里，所以外部连接也能验证它们
//! 确实被设置过。`synchronous` / `cache_size` / `temp_store` / `journal_size_limit` /
//! `mmap_size` / `busy_timeout` 都是**连接级**设置：外部重开连接只会看到 bundled 构建
//! 的默认值，因此它们通过 [`store::Store::connection_pragmas`] 在**本进程自己的连接**
//! 上断言（`open.rs` 的单元测试另有一层，直接用策略函数本身）。

#[test]
fn all_store_databases_use_wal_journal() {
    let tmp = tempfile::tempdir().unwrap();
    drop(store::Store::open(tmp.path()).unwrap());

    for name in ["app.db", "cache.db", "library.db", "subscribe.db"] {
        let conn = rusqlite::Connection::open(tmp.path().join(name)).unwrap();
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "{name} 应为 WAL");
    }
}

/// 重开一个已有的 Store 必须保持幂等，且不改变已设的 WAL。
#[test]
fn reopening_store_keeps_wal() {
    let tmp = tempfile::tempdir().unwrap();
    drop(store::Store::open(tmp.path()).unwrap());
    drop(store::Store::open(tmp.path()).unwrap());

    let conn = rusqlite::Connection::open(tmp.path().join("app.db")).unwrap();
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

/// P6：per-kind 连接策略。缓存库可重建，容忍掉电丢最近提交（NORMAL）；权威库
/// （app / library / subscribe）保持 FULL，绝不为了速度牺牲已提交事务。
#[test]
fn connection_policy_per_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store::Store::open(tmp.path()).unwrap();
    let snapshots = store.connection_pragmas();
    assert_eq!(snapshots.len(), 4, "四个库都要有快照: {snapshots:?}");

    for (name, pragmas) in &snapshots {
        assert_eq!(pragmas.temp_store, 2, "{name}: temp_store 必须是 MEMORY");
        assert_eq!(pragmas.cache_size, -16_384, "{name}: 页缓存 16 MiB");
        assert_eq!(
            pragmas.journal_size_limit, 67_108_864,
            "{name}: WAL 上限 64 MiB"
        );
        assert_eq!(
            pragmas.mmap_size, 268_435_456,
            "{name}: mmap 上限 256 MiB（每连接地址空间，不是常驻内存）"
        );
        assert_eq!(pragmas.busy_timeout_ms, 5_000, "{name}: busy_timeout");
        assert_eq!(pragmas.foreign_keys, 1, "{name}: foreign_keys");
        assert_eq!(pragmas.query_only, 0, "{name}: 写连接不能是 query_only");
        let expected = if *name == "cache" { 1 } else { 2 };
        assert_eq!(
            pragmas.synchronous, expected,
            "{name}: synchronous 必须按库区分（cache=1 NORMAL，其余=2 FULL）"
        );
    }
    let cache = snapshots
        .iter()
        .find(|(name, _)| *name == "cache")
        .map(|(_, pragmas)| pragmas);
    assert_eq!(
        cache.map(|pragmas| pragmas.synchronous),
        Some(1),
        "cache.db 必须用 NORMAL"
    );

    // 持久属性：建库路径把 auto_vacuum 写成 INCREMENTAL(2)。
    drop(store);
    for name in ["app.db", "cache.db", "library.db", "subscribe.db"] {
        let conn = rusqlite::Connection::open(tmp.path().join(name)).unwrap();
        let mode: i64 = conn
            .pragma_query_value(None, "auto_vacuum", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, 2, "{name} 建库时必须设为 incremental auto_vacuum");
    }

    // 重开不得静默翻转文件头属性，也不能丢连接策略（每次 open 都重新应用）。
    let reopened = store::Store::open(tmp.path()).unwrap();
    for (name, pragmas) in reopened.connection_pragmas() {
        assert_eq!(pragmas.auto_vacuum, 2, "{name}: 重开后 auto_vacuum 变了");
        assert_eq!(pragmas.cache_size, -16_384, "{name}: 重开后缓存设置丢了");
        let expected = if name == "cache" { 1 } else { 2 };
        assert_eq!(pragmas.synchronous, expected, "{name}: 重开后 synchronous 变了");
    }
}
