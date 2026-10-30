use super::super::*;
use super::fixture::*;

#[test]
fn cancellation_before_control_registration_prevents_scan() {
    use std::time::{Duration, Instant};
    let fix = Fixture::new();
    std::fs::write(fix.root.join("Movie1 (2024).mkv"), b"video").unwrap();
    let signature = fix.signature();
    let op = fix
        .state
        .store()
        .enqueue_scan(&fix.library_id, &signature, &[], 1000)
        .unwrap();
    let controls = fix.state.scan_coordinator.active_controls.lock();
    fix.state.scan_coordinator.trigger_next(&fix.state);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fix
        .state
        .library_work
        .active_control(
            &fix.library_id,
            crate::library_lifecycle::LibraryWorkKind::Ordinary,
        )
        .is_none()
    {
        assert!(Instant::now() < deadline, "scan did not acquire its lease");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fix.state.store().cancel_scan(&op.id, 2000).unwrap());
    drop(controls);
    fix.state.scan_coordinator.cancel_operation(&op.id);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fix
        .state
        .library_work
        .active_control(
            &fix.library_id,
            crate::library_lifecycle::LibraryWorkKind::Ordinary,
        )
        .is_some()
    {
        assert!(Instant::now() < deadline, "scan did not release its lease");
        std::thread::sleep(Duration::from_millis(10));
    }
    let store = fix.state.store();
    let store = store.as_ref();
    assert_eq!(
        store.scan_operation(&op.id).unwrap().unwrap().status,
        ScanOperationStatus::Cancelled
    );
    assert!(store.list_file_coverage().unwrap().is_empty());
}

/// P2-4：scope 指纹必须无歧义——根路径里的逗号不能造成不同配置碰撞。
#[test]
fn scope_signature_does_not_collide_on_commas_in_roots() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::Store::open(dir.path().join("data")).unwrap();
    let a = store
        .create_library(
            MediaKind::Movie,
            "A",
            &["/x/a,/x/b", "/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let b = store
        .create_library(
            MediaKind::Movie,
            "B",
            &["/x/a", "/x/b,/x/c"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    assert_ne!(
        scan_scope_signature(&a),
        scan_scope_signature(&b),
        "逗号拼接会让不同根配置碰撞: {}",
        scan_scope_signature(&a)
    );
    // kind 也必须参与编码。根路径在全库范围内唯一（`library_roots.path_key`），
    // 第二个库无法复用同一组根，因此直接改写同一份配置的 kind。
    let mut tv = a.clone();
    tv.kind = MediaKind::Tv;
    assert_ne!(scan_scope_signature(&a), scan_scope_signature(&tv));
}

/// P2-4：准入通过之后、scope 捕获之前 PATCH roots，旧 operation 必须取消且零登记。
#[tokio::test]
async fn scope_change_between_admission_and_capture_cancels_without_ledgering() {
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

    // 屏障：准入已通过，但在捕获 scope 之前把 roots 换到别处。
    let moved_root = fix.root.parent().unwrap().join("movies-moved");
    std::fs::create_dir_all(&moved_root).unwrap();
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let moved = moved_root.to_str().unwrap().to_string();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.as_ref();
            guard
                .update_library(&library_id, None, Some(&[moved.as_str()]), None)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Cancelled,
        "{:?}",
        done.report
    );
    assert_eq!(done.report["error"], "library.scope_changed");
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("captured scope changed")),
        "取消原因必须来自捕获 scope 复核: {}",
        done.report
    );
    assert!(
        fix.state.store().list_file_coverage().unwrap().is_empty(),
        "取消的 operation 不得登记任何文件"
    );
}

/// P2-NEW-2：文件版本观察（模拟慢 NAS stat）期间不得持有 Library writer。
///
/// 屏障阻塞在观察点；其他读取与 Library 写入仍须继续。
#[tokio::test]
async fn slow_file_observation_keeps_the_library_writer_free() {
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

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_observe_hook::set(
        &hook_op_id,
        Box::new(move || {
            let _ = entered_tx.send(());
            // 模拟慢 NAS stat：观察窗口内一直阻塞。
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("slow observation gate was not released");
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
        .await
        .expect("worker did not enter the observation gate")
        .expect("observation worker stopped before entering the gate");

    let store = fix.state.store();
    // Exercise the actual database writer rather than a removed outer Store mutex.
    let writer_available = store.with_writer(store::Db::Library, |_| Ok(()));
    let readable = store.scan_operation(&op.id);
    release_tx.send(()).unwrap();
    assert!(
        writer_available.is_ok(),
        "慢文件观察期间 Library writer 必须可用（P2-NEW-2）: {writer_available:?}"
    );
    assert!(
        readable.unwrap().is_some(),
        "慢观察期间其他 Store 使用者必须仍能读"
    );

    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Completed,
        "{:?}",
        done.report
    );
    assert_eq!(done.report["scanned"], 1, "{}", done.report);
}

/// P2-4(b)：Video 也必须在发布前把捕获 scope 绑定到持久化 signature。
#[tokio::test]
async fn video_scope_change_between_admission_and_capture_cancels_without_ledgering() {
    let fix = Fixture::with_kind(MediaKind::Video);
    let file = fix.root.join("clip.mp4");
    std::fs::write(&file, b"1").unwrap();
    let sig = fix.signature();
    let op = {
        let store = fix.state.store();
        let guard = store.as_ref();
        guard
            .enqueue_scan(&fix.library_id, &sig, &[], 1000)
            .unwrap()
    };

    let moved_root = fix.root.parent().unwrap().join("movies-moved");
    std::fs::create_dir_all(&moved_root).unwrap();
    // 新根里有可入账的文件：如果 Video 绕过复核，就会把它登记进台账。
    std::fs::write(moved_root.join("other.mp4"), b"x").unwrap();
    let state = fix.state.clone();
    let library_id = fix.library_id.clone();
    let moved = moved_root.to_str().unwrap().to_string();
    let hook_op_id = op.id.clone();
    crate::http::library_scan::runner::test_scope_hook::set(
        &hook_op_id,
        Box::new(move || {
            let store = state.store();
            let guard = store.as_ref();
            guard
                .update_library(&library_id, None, Some(&[moved.as_str()]), None)
                .unwrap();
        }),
    );

    fix.state.scan_coordinator.trigger_next(&fix.state);
    let done = fix.wait_terminal(&op.id).await;
    assert_eq!(
        done.status,
        ScanOperationStatus::Cancelled,
        "Video 捕获 scope 变化必须取消: {:?}",
        done.report
    );
    assert!(
        done.report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("captured scope changed")),
        "取消原因必须来自捕获 scope 复核: {}",
        done.report
    );
    assert!(
        fix.state.store().list_file_coverage().unwrap().is_empty(),
        "取消的 Video operation 不得登记任何文件"
    );
}
