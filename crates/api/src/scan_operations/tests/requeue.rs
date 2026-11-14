use super::super::*;
use super::fixture::*;

/// P3-1 + P3-3：轮内并入的事件必须让本轮补扫，且终态报告是跨轮累计。
#[tokio::test]
async fn dirty_event_during_round_requeues_with_cumulative_report() {
    let fix = Fixture::new();
    let dir = fix.root.join("Show A");
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.as_ref();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, std::slice::from_ref(&dir), 1000)
            .unwrap();
        guard
            .record_scan_seen(&op.id, std::slice::from_ref(&f1))
            .unwrap();
        op
    };
    let f2 = dir.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();

    // 屏障：本轮已经开始（认领后），此时并入同一 target 的第二个事件。
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let target = dir.clone();
    let hook_sig = sig.clone();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.as_ref();
            guard
                .enqueue_scan(&library_id, &hook_sig, std::slice::from_ref(&target), 1001)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{:?}",
        done.report
    );
    assert_eq!(done.dirty_rev, 1);
    assert_eq!(done.consumed_rev, 1, "轮内事件必须被补扫轮消费");
    assert_eq!(
        done.report["scanned"], 1,
        "累计报告必须包含第一轮已处理的文件: {}",
        done.report
    );
    assert_eq!(done.report["inserted"], 1, "{}", done.report);
    let rows = fix.state.store().list_file_coverage().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.file.path.ends_with("Movie2 (2024).mkv"))
    );
}

/// P3-NEW-1：补扫轮把状态标记为 Running 时，requeue 事务刚落库的累计报告必须还在。
#[tokio::test]
async fn running_transition_keeps_requeued_cumulative_report() {
    let fix = Fixture::new();
    let dir = fix.root.join("Show A");
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("Movie1 (2024).mkv");
    std::fs::write(&f1, b"1").unwrap();
    fix.insert_media_and_ledger("Show A/Movie1 (2024).mkv");
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.as_ref();
        let op = guard
            .enqueue_scan(&fix.library_id, &sig, std::slice::from_ref(&dir), 1000)
            .unwrap();
        guard
            .record_scan_seen(&op.id, std::slice::from_ref(&f1))
            .unwrap();
        op
    };
    let f2 = dir.join("Movie2 (2024).mkv");
    std::fs::write(&f2, b"2").unwrap();

    // 第二轮（补扫轮）在 Running 写入之后、捕获 scope 之前读一次持久化报告。
    let observed: Arc<std::sync::Mutex<Option<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(None));
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let target = dir.clone();
    let hook_sig = sig.clone();
    let hook_key = op.id.clone();
    let op_id = op.id.clone();
    let observed_round2 = observed.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_key,
        Box::new(move || {
            // 第一轮：并入第二个事件 → 本轮补扫重排并落库累计报告。
            {
                let store = state.store();
                let guard = store.as_ref();
                guard
                    .enqueue_scan(&library_id, &hook_sig, std::slice::from_ref(&target), 1001)
                    .unwrap();
            }
            let observed = observed_round2.clone();
            let state_round2 = state.clone();
            let round2_key = op_id.clone();
            let op_id_round2 = op_id.clone();
            crate::http::library_scan::runner::test_scope_hook::set(
                &round2_key,
                Box::new(move || {
                    let report = state_round2
                        .store()
                        .scan_operation(&op_id_round2)
                        .unwrap()
                        .map(|op| op.report);
                    *observed.lock().unwrap() = report;
                }),
            );
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{:?}",
        done.report
    );
    assert_eq!(done.dirty_rev, 1);
    let seen = observed
        .lock()
        .unwrap()
        .clone()
        .expect("第二轮必须触发观察钩子");
    assert_eq!(
        seen["scanned"], 1,
        "补扫轮标记 Running 不得清空 requeue 落库的累计报告: {seen}"
    );
    assert_eq!(seen["inserted"], 1, "{seen}");
}

/// P3-NEW-2：失败轮因 dirty 事件补扫后成功，失败记录与 HTTP 状态必须保留。
#[tokio::test]
async fn requeued_failed_round_keeps_bounded_failure_history() {
    let fix = Fixture::new();
    let file = fix.root.join("Movie1 (2024).mkv");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.as_ref();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    // 第一轮：元数据阶段注入失败（ledger 已提交），同时并入第二个事件触发补扫。
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let hook_sig = sig.clone();
    let op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.as_ref();
            guard
                .enqueue_scan(&library_id, &hook_sig, &[], 1001)
                .unwrap();
        }),
    );
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
        ScanOperationStatus::Completed,
        "{:?}",
        done.report
    );
    assert_eq!(
        done.report["failure_count"], 1,
        "失败次数必须跨轮累计: {}",
        done.report
    );
    let failures = done.report["failures"]
        .as_array()
        .unwrap_or_else(|| panic!("失败历史必须保留: {}", done.report));
    assert_eq!(
        failures[0]["error"], "library.scan_metadata",
        "{}",
        done.report
    );
    assert_eq!(failures[0]["http_status"], 500, "{}", done.report);
    assert_eq!(
        done.report["last_failure"]["http_status"], 500,
        "最近一次失败（含 HTTP 状态）必须可读: {}",
        done.report
    );
    // 第一轮已提交的前缀 + 第二轮的成功轮，都要在累计计数里。
    assert_eq!(done.report["scanned"], 2, "{}", done.report);
    assert_eq!(done.report["inserted"], 1, "{}", done.report);
    assert_eq!(fix.state.store().list_file_coverage().unwrap().len(), 1);
}
