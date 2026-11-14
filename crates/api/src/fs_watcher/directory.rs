//! Directory notifications may only describe generated sidecars. Compare actual
//! media with durable scan versions before scheduling another Library scan.

use std::path::{Path, PathBuf};

use crate::management::ApiState;

pub(super) fn changed_media_files(state: &ApiState, dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_media_files(dir, &mut files);
    let paths: Vec<String> = files
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let versions = match state.store.as_ref().ledger_source_versions(&paths) {
        Ok(versions) => versions,
        Err(error) => {
            tracing::error!(path = %dir.display(), %error, "读取目录事件的媒体源版本失败，保留扫描请求");
            return files;
        }
    };
    files.retain(|path| {
        let current = store::Store::scan_seen_stamp(path);
        !versions
            .get(&path.display().to_string())
            .is_some_and(|recorded| store::Store::scan_seen_stamp_matches(recorded, &current))
    });
    tracing::debug!(path = %dir.display(), changed_media = files.len(), "目录事件媒体变更检查完成");
    files
}

fn collect_media_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(path = %dir.display(), %error, "读取目录事件中的媒体文件失败");
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(path = %dir.display(), %error, "读取目录事件中的条目失败");
                continue;
            }
        };
        let path = entry.path();
        if super::is_ignored_filesystem_metadata(&path) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "读取目录条目类型失败");
                continue;
            }
        };
        // Never follow links outside the Library or recurse through link cycles.
        if file_type.is_dir() {
            collect_media_files(&path, files);
        } else if file_type.is_file() && library::is_video_file(&path) {
            files.push(path);
        }
    }
}
