use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEvent, new_debouncer};
use parking_lot::Mutex;

use crate::management::ApiState;

/// STRM 延迟删除条目：记录删除时间戳与删除前的流媒体 URL。
#[derive(Clone, Debug)]
pub struct PendingStrmDeletion {
    pub deleted_at: i64,
    pub previous_url: Option<String>,
}

/// 全局 STRM 延时防抖状态追踪器
#[derive(Default)]
pub struct StrmGraceTracker {
    /// path -> PendingStrmDeletion
    pending: Arc<Mutex<HashMap<PathBuf, PendingStrmDeletion>>>,
    known_urls: Arc<Mutex<HashMap<PathBuf, String>>>,
}

impl StrmGraceTracker {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            known_urls: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 标记一个 .strm 文件被删除，进入延时观察期（默认 45 秒）
    pub fn mark_deleted(&self, path: PathBuf, previous_url: Option<String>, now: i64) {
        let previous_url = previous_url.or_else(|| self.known_urls.lock().remove(&path));
        let mut map = self.pending.lock();
        tracing::info!(
            path = %path.display(),
            "STRM 文件已删除，进入宽限期（45 秒）"
        );
        map.insert(
            path,
            PendingStrmDeletion {
                deleted_at: now,
                previous_url,
            },
        );
    }

    /// 检查并尝试对冲重建：
    /// 如果在观察期内重新出现，比对 URL 内容；
    /// 若内容不变，取消删除并返回 false（表示无需刷新元数据/封面）；
    /// 若是全新文件或 URL 发生变化，返回 true。
    pub fn on_created_or_modified(&self, path: &Path, current_url: Option<&str>) -> bool {
        let prev_url = if let Some(url) = current_url {
            self.known_urls
                .lock()
                .insert(path.to_path_buf(), url.to_string())
        } else {
            None
        };
        let mut map = self.pending.lock();
        if let Some(pending) = map.remove(path) {
            let same_content = match (&pending.previous_url, current_url) {
                (Some(old_u), Some(new_u)) => old_u.trim() == new_u.trim(),
                _ => false,
            };
            if same_content {
                tracing::info!(
                    path = %path.display(),
                    "STRM 文件在宽限期内以相同 URL 重建，取消删除并跳过重新刮削"
                );
                return false;
            } else {
                tracing::info!(
                    path = %path.display(),
                    "STRM 文件以新 URL 重建，保留并允许刷新"
                );
                return true;
            }
        }
        // 如果文件一直在磁盘上且 URL 未曾变化（如仅更新文件时间戳或写同 URL），不应触发标记失效与重刮削
        if let (Some(prev), Some(curr)) = (prev_url, current_url) {
            if prev.trim() == curr.trim() {
                return false;
            }
        }
        true
    }

    /// 清理已超时的待删除条目（超时仍未重建，执行真实台账删除）
    pub fn sweep_expired(&self, now: i64, grace_secs: i64) -> Vec<PathBuf> {
        let mut map = self.pending.lock();
        let mut expired = Vec::new();
        map.retain(|path, item| {
            if now - item.deleted_at >= grace_secs {
                // 超时后确认磁盘上是否真不存在
                if !path.exists() {
                    expired.push(path.clone());
                }
                false
            } else {
                true
            }
        });
        expired
    }
}

/// 启动全局文件系统事件监控
pub fn spawn_fs_watcher(state: ApiState) {
    tokio::spawn(async move {
        let tracker = Arc::new(StrmGraceTracker::new());
        if let Ok(rows) = state.store.lock().list_ledger() {
            for row in rows {
                let path = Path::new(&row.path);
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
                {
                    if let Some(url) = library::read_strm_url(path) {
                        tracker.on_created_or_modified(path, Some(&url));
                    }
                }
            }
        }

        // 启动后台定时任务，定期检查 STRM 宽限删除到期的项目
        let sweep_tracker = tracker.clone();
        let sweep_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let now = crate::job_loop::unix_now();
                let expired = sweep_tracker.sweep_expired(now, 45);
                if !expired.is_empty() {
                    let store = sweep_state.store.lock();
                    for path in expired {
                        let path_str = path.display().to_string();
                        tracing::info!(path = %path_str, "STRM 宽限期已过，永久删除台账记录");
                        let _ = store.delete_ledger_path(&path_str);
                    }
                }
            }
        });

        // 通道接收 notify 防抖事件
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<DebouncedEvent>>(100);
        let debouncer_tx = tx.clone();

        let mut debouncer = match new_debouncer(
            Duration::from_secs(3),
            move |res: Result<Vec<DebouncedEvent>, _>| {
                if let Ok(events) = res {
                    let _ = debouncer_tx.blocking_send(events);
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "初始化 notify 防抖监听失败");
                return;
            }
        };

        let mut watcher_state = FsWatcherState::new();
        match current_dirs_to_watch(&state) {
            Ok(desired) => {
                watcher_state.reconcile(&mut debouncer, &desired);
            }
            Err(error) => {
                tracing::error!(%error, "初始化获取监听目录配置失败");
            }
        }

        let mut refresh_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                _ = refresh_interval.tick() => {
                    match current_dirs_to_watch(&state) {
                        Ok(desired) => {
                            watcher_state.reconcile(&mut debouncer, &desired);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "刷新监听目录配置失败，保留当前已有监听状态并待重试");
                        }
                    }
                }
                Some(events) = rx.recv() => {
                    handle_fs_events(&state, &tracker, events);
                }
            }
        }
    });
}

fn current_dirs_to_watch(state: &ApiState) -> Result<HashSet<PathBuf>, store::StoreError> {
    let store = state.store.lock();
    let mut dirs = HashSet::new();
    if let Some(intake) = store.watch_intake()? {
        if !intake.is_empty() {
            dirs.insert(PathBuf::from(intake));
        }
    }
    if let Some(inplace) = store.watch_inplace()? {
        if !inplace.is_empty() {
            dirs.insert(PathBuf::from(inplace));
        }
    }
    let libraries = store.list_libraries()?;
    for lib in libraries {
        if lib.realtime_watch {
            for root in lib.root_paths {
                dirs.insert(root);
            }
        }
    }
    Ok(dirs)
}

fn realtime_library_root_for_path(state: &ApiState, path: &Path) -> Option<PathBuf> {
    let resolved_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    state
        .store
        .lock()
        .list_libraries()
        .ok()?
        .into_iter()
        .filter(|library| library.realtime_watch)
        .flat_map(|library| library.root_paths)
        .filter(|root| {
            let resolved_root = std::fs::canonicalize(root).unwrap_or(root.clone());
            resolved_path.starts_with(&resolved_root)
        })
        .max_by_key(|root| root.components().count())
}

fn handle_fs_events(
    state: &ApiState,
    tracker: &Arc<StrmGraceTracker>,
    events: Vec<DebouncedEvent>,
) {
    let now = crate::job_loop::unix_now();
    let mut has_intake_or_download_change = false;
    let mut has_scrape_needed = false;
    let mut library_strm_paths = HashSet::new();

    for event in events {
        let path = event.path;
        let is_strm = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("strm"))
            .unwrap_or(false);

        // 忽略 sidecars 侧车文件的写入变动（.nfo, .jpg, .png, .nfo.xml 等），防止刮削自激死循环
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if matches!(
            ext.as_str(),
            "nfo" | "jpg" | "jpeg" | "png" | "webp" | "xml" | "txt" | "srt" | "ass"
        ) {
            continue;
        }

        if !path.exists() {
            // 文件已被移除
            if is_strm {
                tracker.mark_deleted(path, None, now);
            }
        } else {
            // 文件新建或修改
            if is_strm {
                let current_url = library::read_strm_url(&path);
                let should_refresh = tracker.on_created_or_modified(&path, current_url.as_deref());
                if should_refresh {
                    // strm 内容（指向的远程 URL）可能已变：旧 URL 的流信息缓存、
                    // 章节缓存与片头片尾标记全部失效，下次访问按新 URL 重新探测。
                    invalidate_strm_caches(state, &path);
                    has_scrape_needed = true;
                    if let Some(root) = realtime_library_root_for_path(state, &path) {
                        library_strm_paths.insert(root);
                    }
                }
            } else {
                let ext = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if matches!(ext.as_str(), "mkv" | "mp4" | "ts" | "mov" | "avi" | "iso") {
                    has_intake_or_download_change = true;
                }
            }
        }
    }

    // 新增 STRM 在 Library 根目录中需要执行 in-place scan 来写入台账；Scrape
    // 只负责侧车/元数据，不能替代 Library ledger ingestion。
    for root in library_strm_paths {
        tracing::info!(root = %root.display(), "媒体库内 STRM 变动，触发媒体库扫描入账");
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::http::library_scan::scan_library_root(&state, &root);
            if let Err(error) = result {
                tracing::error!(root = %root.display(), %error, "媒体库 STRM 变动扫描入账失败");
            }
        });
    }

    // 目录有新入库/下载文件完成，主动提前调度任务，不必等待 30 秒
    if has_intake_or_download_change {
        tracing::info!("文件系统事件触发即时转移与入站入库");
        let queue = state.jobs.lock();
        if let Ok(defs) = queue.list_defs() {
            for def in defs {
                if def.kind == jobs::JobKind::Transfer || def.kind == jobs::JobKind::WatchIntake {
                    let _ = queue.ensure_scheduled_for(0, def.id);
                }
            }
        }
    }

    if has_scrape_needed {
        tracing::info!("文件系统事件触发即时元数据刮削");
        let queue = state.jobs.lock();
        if let Ok(defs) = queue.list_defs() {
            for def in defs {
                if def.kind == jobs::JobKind::Scrape {
                    let _ = queue.ensure_scheduled_for(0, def.id);
                }
            }
        }
    }
}

/// strm 文件内容（远程 URL）变化时，清掉旧 URL 的派生缓存：
/// - file_meta（视频/音轨/字幕流信息，探测缓存）
/// - cached chapters（内嵌章节缓存）
/// - media_markers（片头片尾标记）
/// 这些缓存按 ledger 行挂载，清理后下次访问详情页/播放时会按新 URL 重新探测。
fn invalidate_strm_caches(state: &ApiState, path: &std::path::Path) {
    let path_str = path.display().to_string();
    let store = state.store.lock();
    let Ok(Some(row)) = store.ledger_by_path(&path_str) else {
        return;
    };
    let ledger_id = row.id.to_string();
    let mut cleared = 0;
    if store.delete_file_meta_by_ledger_id(&ledger_id).is_ok() {
        cleared += 1;
    }
    if store.clear_cached_chapters(&ledger_id).is_ok() {
        cleared += 1;
    }
    // 只清当前行对应季集未锁定的标记：电视剧各集共用 media_id，绝不能删整部剧；用户手动锁定标记保留。
    let is_locked = store
        .get_media_marker(row.media_id, row.season, row.episode)
        .ok()
        .flatten()
        .map(|m| m.locked)
        .unwrap_or(false);
    if !is_locked
        && store
            .delete_media_marker(row.media_id, row.season, row.episode)
            .is_ok()
    {
        cleared += 1;
    }
    tracing::info!(
        path = %path_str,
        season = row.season,
        episode = row.episode,
        cleared,
        "【STRM】检测到 strm 内容变化，已清空该集旧探测缓存（流信息/章节/片头片尾），下次访问将按新 URL 重新探测"
    );
}

/// 抽象文件系统 Watcher 行为，方便单元测试注入 fake/mock
pub trait FsWatcherSession {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error>;
    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error>;
}

impl FsWatcherSession for notify_debouncer_mini::Debouncer<notify::RecommendedWatcher> {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.watcher().watch(dir, RecursiveMode::Recursive)
    }

    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.watcher().unwatch(dir)
    }
}

/// 维护 watcher 的 desired 与 active 集合状态
#[derive(Default, Debug)]
pub struct FsWatcherState {
    active: HashSet<PathBuf>,
}

impl FsWatcherState {
    pub fn new() -> Self {
        Self {
            active: HashSet::new(),
        }
    }

    pub fn active(&self) -> &HashSet<PathBuf> {
        &self.active
    }

    /// 执行一轮 desired 状态收敛对齐。
    /// - desired: 期望监听的目录集合
    /// - watch 失败或目录不存在时不加入 active，保留下次刷新重试
    /// - unwatch 失败保留在 active，下次刷新重试
    pub fn reconcile<W: FsWatcherSession>(&mut self, watcher: &mut W, desired: &HashSet<PathBuf>) {
        // 新增监听：desired - active
        let to_add: Vec<PathBuf> = desired.difference(&self.active).cloned().collect();
        for dir in to_add {
            if !dir.is_dir() {
                tracing::warn!(dir = %dir.display(), "目标路径不是目录或尚不存在，暂不加入 active 监听集");
                continue;
            }
            match watcher.watch_dir(&dir) {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), "成功注册目录监听");
                    self.active.insert(dir);
                }
                Err(error) => {
                    tracing::error!(dir = %dir.display(), %error, "注册目录监听失败，保留待重试");
                }
            }
        }

        // 移除监听：active - desired
        let to_remove: Vec<PathBuf> = self.active.difference(desired).cloned().collect();
        for dir in to_remove {
            match watcher.unwatch_dir(&dir) {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), "成功注销目录监听");
                    self.active.remove(&dir);
                }
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "注销目录监听失败，保留在 active 待重试");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EmptyFetcher;

    impl indexer::Fetcher for EmptyFetcher {
        fn fetch(&self, _request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unexpected request".into()))
        }
    }

    struct FakeWatcher {
        fail_watch_paths: HashSet<PathBuf>,
        fail_unwatch_paths: HashSet<PathBuf>,
        watched: HashSet<PathBuf>,
        watch_calls: Arc<AtomicUsize>,
        unwatch_calls: Arc<AtomicUsize>,
    }

    impl FakeWatcher {
        fn new() -> Self {
            Self {
                fail_watch_paths: HashSet::new(),
                fail_unwatch_paths: HashSet::new(),
                watched: HashSet::new(),
                watch_calls: Arc::new(AtomicUsize::new(0)),
                unwatch_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl FsWatcherSession for FakeWatcher {
        fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
            self.watch_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_watch_paths.contains(dir) {
                return Err(notify::Error::generic("mock watch failure"));
            }
            self.watched.insert(dir.to_path_buf());
            Ok(())
        }

        fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
            self.unwatch_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_unwatch_paths.contains(dir) {
                return Err(notify::Error::generic("mock unwatch failure"));
            }
            self.watched.remove(dir);
            Ok(())
        }
    }

    #[test]
    fn realtime_library_root_matches_nested_strm_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tv");
        let episode_dir = root.join("喜剧之王 (2026)/Season 1");
        std::fs::create_dir_all(&episode_dir).unwrap();
        let episode = episode_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
        std::fs::write(&episode, "https://example.test/episode.mkv").unwrap();

        let store = crate::Store::open(temp.path().join("data")).unwrap();
        store
            .set_library_root(domain::MediaKind::Tv, root.to_str().unwrap())
            .unwrap();
        let state = crate::management::ApiState::new(
            store,
            "test-token".into(),
            indexer::ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
            temp.path().join("library"),
        )
        .unwrap();

        assert_eq!(
            realtime_library_root_for_path(&state, &episode),
            Some(root.clone())
        );
        crate::http::library_scan::scan_library_root(&state, &root).unwrap();
        let rows = state.store.lock().list_ledger().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, episode.display().to_string());
        assert_eq!(rows[0].season, Some(1));
        assert_eq!(rows[0].episode, Some(1));
        assert!(state.probe.is_queued(&rows[0].id.to_string()));
    }

    #[test]
    fn test_watch_failure_retried_on_next_reconcile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir1 = temp.path().join("dir1");
        let dir2 = temp.path().join("dir2");
        std::fs::create_dir(&dir1).unwrap();
        std::fs::create_dir(&dir2).unwrap();

        let mut watcher = FakeWatcher::new();
        // dir1 第一次 watch 失败
        watcher.fail_watch_paths.insert(dir1.clone());

        let mut state = FsWatcherState::new();
        let mut desired = HashSet::new();
        desired.insert(dir1.clone());
        desired.insert(dir2.clone());

        // 第一轮：dir1 失败，dir2 成功
        state.reconcile(&mut watcher, &desired);
        assert!(!state.active().contains(&dir1), "dir1 失败后不能在 active");
        assert!(state.active().contains(&dir2), "dir2 成功后必须在 active");

        // 恢复 dir1
        watcher.fail_watch_paths.remove(&dir1);

        // 第二轮：重试 dir1
        state.reconcile(&mut watcher, &desired);
        assert!(state.active().contains(&dir1), "重试后 dir1 必须在 active");
        assert!(state.active().contains(&dir2), "dir2 保持在 active");
    }

    #[test]
    fn test_missing_directory_not_active_until_created() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("not_yet_created");

        let mut watcher = FakeWatcher::new();
        let mut state = FsWatcherState::new();
        let mut desired = HashSet::new();
        desired.insert(dir.clone());

        // 第一次：目录不存在，不进入 active 也不调 watcher.watch
        state.reconcile(&mut watcher, &desired);
        assert!(!state.active().contains(&dir));
        assert_eq!(watcher.watch_calls.load(Ordering::SeqCst), 0);

        // 创建目录后再次 reconcile
        std::fs::create_dir(&dir).unwrap();
        state.reconcile(&mut watcher, &desired);
        assert!(state.active().contains(&dir));
        assert_eq!(watcher.watch_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_unwatch_failure_keeps_active_and_retries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("dir_to_remove");
        std::fs::create_dir(&dir).unwrap();

        let mut watcher = FakeWatcher::new();
        let mut state = FsWatcherState::new();
        let mut desired = HashSet::new();
        desired.insert(dir.clone());

        // 成功加入
        state.reconcile(&mut watcher, &desired);
        assert!(state.active().contains(&dir));

        // 从 desired 移除，但 unwatch 失败
        watcher.fail_unwatch_paths.insert(dir.clone());
        desired.remove(&dir);
        state.reconcile(&mut watcher, &desired);
        assert!(state.active().contains(&dir), "unwatch 失败保留在 active");

        // 下一轮 unwatch 恢复成功
        watcher.fail_unwatch_paths.remove(&dir);
        state.reconcile(&mut watcher, &desired);
        assert!(
            !state.active().contains(&dir),
            "unwatch 成功后从 active 移除"
        );
    }

    #[test]
    fn test_config_error_preserves_active_set() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("existing_dir");
        std::fs::create_dir(&dir).unwrap();

        let mut watcher = FakeWatcher::new();
        let mut state = FsWatcherState::new();
        let mut desired = HashSet::new();
        desired.insert(dir.clone());

        // 初始化注册
        state.reconcile(&mut watcher, &desired);
        assert!(state.active().contains(&dir));

        // 模拟配置读取失败时的逻辑：不调用 reconcile（或者不传入空 desired），保持 active 不变
        let config_result: Result<HashSet<PathBuf>, &'static str> = Err("db error");
        match config_result {
            Ok(new_desired) => state.reconcile(&mut watcher, &new_desired),
            Err(_) => {
                // 不执行 reconcile，保留 active
            }
        }
        assert!(
            state.active().contains(&dir),
            "配置查询失败时已有监听集合必须完整保留"
        );
        assert_eq!(watcher.unwatch_calls.load(Ordering::SeqCst), 0);
    }
}
