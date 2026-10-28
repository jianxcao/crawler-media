//! L6: `Queue::tick` 必须在跑 runner **之前**释放 Queue 锁。
//!
//! 旧实现在持锁状态下调用 runner；runner 只要碰一下队列（维护 runner 就是这么做的）
//! 就会在同一把不可重入的锁上自锁。这里 runner 主动再锁一次队列，主线程用
//! `recv_timeout` 等完成信号：死锁会 2 秒后**失败**，而不是把 CI 挂住。

use std::sync::{Arc, mpsc};
use std::time::Duration;

use jobs::{Job, JobKind, JobStatus, Queue, Runner};
use parking_lot::Mutex;

/// 在 `run` 里重新锁住同一个队列，然后报告完成。
struct ReentrantRunner {
    queue: Arc<Mutex<Queue>>,
    done: mpsc::Sender<JobKind>,
}

impl Runner for ReentrantRunner {
    fn run(&self, job: &Job) -> Result<(), String> {
        // 旧实现：tick 持锁调用 runner，这里会永久阻塞。
        let defs = self
            .queue
            .lock()
            .list_defs()
            .map_err(|error| error.to_string())?;
        let _ = defs.len();
        self.done
            .send(job.kind)
            .map_err(|_| "test receiver dropped".to_string())
    }
}

#[test]
fn tick_runner_can_lock_queue() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Mutex::new(Queue::open(dir.path().join("jobs.db")).unwrap()));
    queue.lock().enqueue(JobKind::Transfer, "{}", 0).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let runner = ReentrantRunner {
        queue: Arc::clone(&queue),
        done: done_tx,
    };

    let report = Queue::tick(&queue, &runner, 10);

    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("runner 必须在 tick 内被调用；超时说明 tick 仍持锁调用 runner"),
        JobKind::Transfer
    );
    assert!(
        report.error.is_none(),
        "整轮 tick 必须完整走完: {:?}",
        report.error
    );
    assert_eq!(report.finished.len(), 1, "成功执行的任务必须落成终态");
    assert_eq!(report.finished[0].status, JobStatus::Succeeded);
}

/// 落库阶段同样在锁内完成：tick 返回后队列里不能再有 running 残行。
#[test]
fn tick_persists_outcome_after_releasing_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let queue = Arc::new(Mutex::new(Queue::open(dir.path().join("jobs.db")).unwrap()));
    let job = queue.lock().enqueue(JobKind::Scrape, "{}", 0).unwrap();
    let report = Queue::tick(&queue, &Failing, 0);
    assert!(report.error.is_none(), "{:?}", report.error);
    let stored = queue.lock().get(job.id).unwrap().unwrap();
    assert_eq!(stored.status, JobStatus::Queued);
    assert_eq!(stored.error.as_deref(), Some("boom"));
}

struct Failing;

impl Runner for Failing {
    fn run(&self, _job: &Job) -> Result<(), String> {
        Err("boom".to_string())
    }
}
