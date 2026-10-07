use api::fs_watcher::StrmGraceTracker;
use std::path::PathBuf;

#[test]
fn strm_grace_tracker_handles_deletion_and_identical_recreation() {
    let tracker = StrmGraceTracker::new();
    let test_path = PathBuf::from("/test/media/The.Matrix.1999.strm");
    let stream_url = "https://example.com/matrix.mkv?token=123";

    // 模拟文件被删除，进入延时观察池
    tracker.mark_deleted(test_path.clone(), Some(stream_url.to_string()), 1000);

    // 模拟在 45s 宽限期内重建相同内容的 STRM 文件
    // on_created_or_modified 应返回 false（表示内容完全相同，静默恢复，不触发刮削或重新拉海报）
    let need_refresh = tracker.on_created_or_modified(&test_path, Some(stream_url));
    assert!(!need_refresh, "相同 URL 重建应静默撤回删除，无需触发刮削");

    // 再次调用 sweep_expired，该文件已不在待删除队列中
    let expired = tracker.sweep_expired(1100, 45);
    assert!(
        !expired.contains(&test_path),
        "撤回删除后不应出现在过期清理列表中"
    );
}

#[test]
fn strm_grace_uses_url_observed_before_file_disappeared() {
    let tracker = StrmGraceTracker::new();
    let path = PathBuf::from("/test/media/episode.strm");
    tracker.on_created_or_modified(&path, Some("https://example.com/episode.mkv"));
    tracker.mark_deleted(path.clone(), None, 1000);
    assert!(!tracker.on_created_or_modified(&path, Some("https://example.com/episode.mkv")));
    tracker.mark_deleted(path.clone(), None, 1010);
    assert!(tracker.on_created_or_modified(&path, Some("https://example.com/changed.mkv")));
}

#[test]
fn strm_grace_tracker_handles_recreation_with_new_url() {
    let tracker = StrmGraceTracker::new();
    let test_path = PathBuf::from("/test/media/The.Matrix.1999.strm");
    let old_url = "https://example.com/matrix.mkv?token=123";
    let new_url = "https://example.com/matrix_remux.mkv?token=456";

    tracker.mark_deleted(test_path.clone(), Some(old_url.to_string()), 1000);

    // 宽限期内重建但更换了 URL
    let need_refresh = tracker.on_created_or_modified(&test_path, Some(new_url));
    assert!(need_refresh, "URL 变动应允许刷新元数据与流信息");

    let expired = tracker.sweep_expired(1100, 45);
    assert!(!expired.contains(&test_path));
}

#[test]
fn strm_grace_tracker_expires_unrecovered_files() {
    let tracker = StrmGraceTracker::new();
    let non_existent_path = PathBuf::from("/non/existent/path/deleted.strm");

    tracker.mark_deleted(
        non_existent_path.clone(),
        Some("https://example.com/lost.mkv".to_string()),
        1000,
    );

    // 在 40s 时未超时
    let not_expired = tracker.sweep_expired(1040, 45);
    assert!(not_expired.is_empty(), "40秒时不应超时");

    // 在 50s 时超时且磁盘真实不存在，应返回以供彻底删除
    let expired = tracker.sweep_expired(1050, 45);
    assert_eq!(expired, vec![non_existent_path]);
}
