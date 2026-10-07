use std::fs;

use library::{TransferMode, transfer_file};

#[test]
fn hardlink_replacement_preserves_old_seeding_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let old_source = tmp.path().join("old-seeding.mkv");
    let new_source = tmp.path().join("new-seeding.mkv");
    let destination = tmp.path().join("library/movie.mkv");
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&old_source, b"old release bytes").unwrap();
    fs::write(&new_source, b"new release bytes").unwrap();
    fs::hard_link(&old_source, &destination).unwrap();

    transfer_file(&new_source, &destination, TransferMode::Hardlink).unwrap();

    assert_eq!(fs::read(&destination).unwrap(), b"new release bytes");
    assert_eq!(fs::read(&old_source).unwrap(), b"old release bytes");
    assert_eq!(fs::read(&new_source).unwrap(), b"new release bytes");
}

#[test]
fn same_filesystem_default_is_hardlink_before_library_root_exists() {
    use std::os::unix::fs::MetadataExt;

    use library::resolve_mode;

    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source.mkv");
    let root = tmp.path().join("new/library/root");
    let destination = root.join("movie.mkv");
    fs::write(&source, b"bytes").unwrap();

    let mode = resolve_mode(&source, &root, None);
    transfer_file(&source, &destination, mode).unwrap();

    assert_eq!(
        fs::metadata(source).unwrap().ino(),
        fs::metadata(destination).unwrap().ino()
    );
}

#[test]
fn move_failure_preserves_source_file_without_data_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source-precious.mkv");
    fs::write(&source, b"precious media content").unwrap();

    // 目标路径被预先创建为一个同名目录，导致向其发布时必然产生冲突与重命名失败
    let destination = tmp.path().join("conflict_dest");
    fs::create_dir_all(&destination).unwrap();

    let res = transfer_file(&source, &destination, TransferMode::Move);
    assert!(res.is_err(), "向目录发布文件必须返回失败错误");

    // 关键断言：源文件必须依然完好存在，绝对不能发生数据丢失！
    assert!(source.exists(), "转移发布失败时源文件必须被安全保留在原位");
    assert_eq!(fs::read(&source).unwrap(), b"precious media content");
}
