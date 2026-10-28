use super::super::*;
use super::fixture::*;

/// P3-3：chunk 已提交 ledger 后失败，失败报告必须带上该 chunk 的已提交前缀。
#[tokio::test]
async fn chunk_failure_after_ledger_commit_reports_its_prefix() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };
    crate::http::library_scan::runner::test_metadata_hook::set(
        &op.id,
        Box::new(|| {
            Some(crate::http::err(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "library.scan_metadata",
                "injected failure",
            ))
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Failed,
        "{:?}",
        done.report
    );
    assert_eq!(
        done.report["scanned"], 1,
        "失败 chunk 已提交的 scanned 前缀必须计入: {}",
        done.report
    );
    assert_eq!(
        done.report["inserted"], 1,
        "失败 chunk 已提交的 inserted 前缀必须计入: {}",
        done.report
    );
    assert!(
        done.report["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{}",
        done.report
    );
    assert_eq!(
        fix.state.store().lock().list_file_coverage().unwrap().len(),
        1,
        "ledger 已提交，报告必须承认这段前缀"
    );
}

/// P2-4(a)：operation 读不到/不存在时必须失败关闭，而不是绕过捕获 scope 复核。
#[test]
fn missing_operation_fails_closed_without_ledgering() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let lease = fix.state.acquire_library_work(&fix.library_id).unwrap();

    let result = crate::http::library_scan::scan_library_leased_resumable(
        &fix.state,
        &fix.library_id,
        &lease,
        &[],
        true,
        Some("missing-operation-id"),
    );
    match result {
        Err(crate::http::library_scan::runner::ScanRunError::Failed(failure)) => {
            assert_eq!(failure.response.status(), axum::http::StatusCode::CONFLICT);
            assert_eq!(failure.error, "library.scan_operation_missing");
        }
        Err(crate::http::library_scan::runner::ScanRunError::ScopeChanged { reason }) => {
            panic!("缺失 operation 不得当成 scope 变化: {reason}")
        }
        Ok(_) => panic!("无法对账的 operation 不得放行"),
    }
    assert!(
        fix.state
            .store()
            .lock()
            .list_file_coverage()
            .unwrap()
            .is_empty(),
        "失败关闭时不得登记任何文件"
    );
}

/// P2-2 & P2-3: 注入失效/重挂失败以及 durable 源版本读取失败，
/// 验证失败路径不得在保留旧派生事实的同时确认新版本，故障解除后重扫必须重试。
#[tokio::test]
async fn invalidation_failure_does_not_advance_durable_source_version() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"initial").unwrap();
    let sig = fix.signature();

    // 轮 1: 扫描入库
    let op1 = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done1 = fix.wait_terminal(&op1.id).await;
    assert_eq!(done1.status, ScanOperationStatus::Completed);

    let row = fix
        .state
        .store()
        .lock()
        .file_coverage_by_path(&file.display().to_string())
        .unwrap()
        .unwrap();
    // 写入派生事实（file_details）
    let tracks = library::Tracks {
        video: Some(library::VideoTrack {
            duration_secs: Some(100.0),
            ..library::VideoTrack::default()
        }),
        audio: Vec::new(),
        subtitles: Vec::new(),
    };
    fix.state
        .store()
        .lock()
        .put_file_meta(&row.file.file_id.to_string(), &tracks)
        .unwrap();
    assert!(
        fix.state
            .store()
            .lock()
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .is_some()
    );

    // 修改文件内容和 mtime
    std::fs::write(&file, b"modified-content").unwrap();

    // 注入 failure: 在 library.db 上建触发器阻止删除 file_details
    let db_path = fix._dir.path().join("data").join("library.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TRIGGER fail_meta_del BEFORE DELETE ON file_details
             BEGIN
                 SELECT RAISE(ABORT, 'injected delete file_details failure');
             END;",
            [],
        )
        .unwrap();
    }

    // 轮 2: 重扫（发生失效失败）
    let op2 = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 2000)
            .unwrap()
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done2 = fix.wait_terminal(&op2.id).await;
    assert_eq!(done2.status, ScanOperationStatus::Completed);

    // 验证 P2-3: 失效失败的路径，旧派生事实仍保留，但 durable 源版本绝不得推进为新文件版本！
    let versions = fix
        .state
        .store()
        .lock()
        .ledger_source_versions(&[file.display().to_string()])
        .unwrap();
    let (_v_ms, v_size, _) = versions.get(&file.display().to_string()).cloned().unwrap();
    assert_eq!(
        v_size,
        b"initial".len() as i64,
        "失效失败不得推进 durable 源版本！"
    );
    assert!(
        fix.state
            .store()
            .lock()
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .is_some()
    );

    // 解除故障注入
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute("DROP TRIGGER fail_meta_del", []).unwrap();
    }

    // 轮 3: 故障解除后重扫，必须重试失效与重挂探测义务
    let op3 = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 3000)
            .unwrap()
    };
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done3 = fix.wait_terminal(&op3.id).await;
    assert_eq!(done3.status, ScanOperationStatus::Completed);

    // 旧派生事实已被成功删除，新版本已成功确认，probe 义务已挂上
    assert!(
        fix.state
            .store()
            .lock()
            .get_file_meta(&row.file.file_id.to_string())
            .unwrap()
            .is_none(),
        "旧派生事实已失效"
    );
    let versions3 = fix
        .state
        .store()
        .lock()
        .ledger_source_versions(&[file.display().to_string()])
        .unwrap();
    let (_, v3_size, _) = versions3.get(&file.display().to_string()).cloned().unwrap();
    assert_eq!(
        v3_size,
        b"modified-content".len() as i64,
        "故障解除后新版本已确认"
    );

    let obs = fix.state.store().lock().scan_probe_obligations(10).unwrap();
    assert!(
        obs.iter().any(|o| o.ledger_id == row.file.file_id),
        "探测义务必须挂上"
    );
}
