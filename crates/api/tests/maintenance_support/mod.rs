//! DB8 Task 4 维护测试的共享 fixture：临时 ApiState、任务入队/领取、假执行器。
//!
//! 只被 `maintenance_runtime.rs` / `maintenance_sqlite.rs` 引用，不算独立测试目标
//! （放在子目录里，Cargo 不会把它当成 test target）。
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::maintenance::{
    DatabaseExecutor, DatabaseName, DatabaseOutcome, DatabaseReport, DatabaseTarget,
    MaintenanceContext,
};
use api::{ApiState, Store};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use jobs::{Job, JobKind};
use parking_lot::Mutex;
use serde_json::Value;
use store::test_support::{FixtureFile, publish_fixture_file};

pub const TOKEN: &str = "maintenance-secret";
pub const DAY: i64 = 86_400;
pub const DB_LIGHT: &str = r#"{"version":1,"vacuum_enabled":false}"#;
pub const DB_VACUUM: &str = r#"{"version":1,"vacuum_enabled":true}"#;
pub const KEEP: &str = r#"{"version":1,"retention_days":1,"dry_run":false}"#;
pub const PREVIEW: &str = r#"{"version":1,"retention_days":1,"dry_run":true}"#;

pub struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("maintenance 测试不访问网络".into()))
    }
}

pub fn build_state(root: &Path, store: Store, executor: Arc<dyn DatabaseExecutor>) -> ApiState {
    let state = ApiState::new(
        store,
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
    .with_maintenance_executor(executor);
    // 只留维护任务：实例默认 def 在注册时就会补一个 queued 子任务，会把 claim 抢走。
    let queue = state.jobs();
    let guard = queue.lock();
    for def in guard.list_defs().unwrap() {
        if !def.kind.is_maintenance() {
            guard.set_def_enabled_by_id(def.id, false).unwrap();
        }
    }
    drop(guard);
    state
}

pub fn temp_state(executor: Arc<dyn DatabaseExecutor>) -> (tempfile::TempDir, ApiState) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let state = build_state(tmp.path(), store, executor);
    (tmp, state)
}

pub fn data_dir(state: &ApiState) -> PathBuf {
    state.store().data_dir().to_path_buf()
}

pub fn unix_now() -> i64 {
    api::store::unix_now()
}

/// 为一个已登记的 Media 发布真实文件，返回可用于签发会话的 file id。
///
/// 维护测试原本用合成 `LedgerId` 签发会话；现在签发要求文件真实存在且已发布。
pub fn seed_published_file(store: &Store, root: &Path, media: domain::MediaId) -> domain::LedgerId {
    let library_root = root.join("fixture-library");
    std::fs::create_dir_all(&library_root).unwrap();
    if store
        .list_libraries()
        .unwrap()
        .iter()
        .all(|library| library.name != "Fixture")
    {
        store
            .create_library(
                domain::MediaKind::Movie,
                "Fixture",
                &[library_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
    }
    let path = library_root.join(format!("{media}.mkv"));
    std::fs::write(&path, b"x").unwrap();
    let id = domain::LedgerId::new();
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(id),
            media_id: media,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap();
    id
}

/// 入队并领取，返回处于 running 的 Job（`maintenance::run` 需要它仍在库里）。
pub fn enqueue_claim(state: &ApiState, kind: JobKind, payload: &str, now: i64) -> Job {
    let queue = state.jobs();
    let guard = queue.lock();
    let job = guard.enqueue(kind, payload, 0).unwrap();
    let claimed = guard.claim(now, 1).unwrap();
    assert_eq!(
        claimed.len(),
        1,
        "必须正好领取刚入队的维护任务: {claimed:?}"
    );
    assert_eq!(
        claimed[0].id, job.id,
        "领到的不是刚入队的任务: enqueued={:?} claimed={:?}",
        job.kind, claimed[0].kind
    );
    claimed.into_iter().next().unwrap()
}

pub fn progress_of(state: &ApiState, job: &Job) -> Value {
    let raw = state
        .jobs()
        .lock()
        .get(job.id)
        .unwrap()
        .unwrap()
        .progress
        .expect("维护任务必须把报告写进 job.progress");
    serde_json::from_str(&raw).unwrap()
}

pub fn db_entry<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["databases"]
        .as_array()
        .unwrap_or_else(|| panic!("报告缺少 databases 数组: {report}"))
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("报告缺少 {name} 的条目: {report}"))
}

/// 记录被调用目标的假执行器：不碰真实文件，用来证明 runner 的编排与锁行为。
#[derive(Default)]
pub struct Recorder {
    seen: Mutex<Vec<(DatabaseName, PathBuf, bool)>>,
}

impl Recorder {
    pub fn seen(&self) -> Vec<(DatabaseName, PathBuf, bool)> {
        self.seen.lock().clone()
    }
}

impl DatabaseExecutor for Recorder {
    fn execute(
        &self,
        _conn: &rusqlite::Connection,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        _context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        self.seen
            .lock()
            .push((target.name, target.path.clone(), vacuum_enabled));
        Ok(DatabaseReport {
            name: target.name.as_str().to_string(),
            outcome: DatabaseOutcome::Optimized,
            free_pages: 0,
            page_size: 4096,
            size_before: 0,
            size_after: 0,
            skip_reason: None,
            elapsed_ms: 1,
        })
    }
}
