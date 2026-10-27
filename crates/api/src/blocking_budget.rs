//! 有界 blocking 预算：把 owned 快照上的 CPU/IO/网络工作移出 async worker。
//!
//! 这不是全局请求超时，也不是取消机制：`run` 的外层 `tokio::time::timeout`
//! 只决定调用方愿意等待多久，permit 由 `spawn_blocking` 闭包持有到闭包真正
//! 结束，因此超时后阻塞工作仍会跑完并释放槽位。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, thiserror::Error)]
pub enum BlockingError {
    #[error("blocking capacity exhausted")]
    Busy,
    #[error("operation deadline elapsed")]
    Timeout,
    #[error("blocking worker failed: {0}")]
    Worker(String),
}

pub struct BlockingBudget {
    permits: Arc<Semaphore>,
}

impl BlockingBudget {
    /// `concurrency` 是启动配置错误而非 IO/parse 路径，允许直接断言。
    pub fn new(concurrency: usize) -> Self {
        assert!(concurrency > 0, "BlockingBudget concurrency must be > 0");
        Self {
            permits: Arc::new(Semaphore::new(concurrency)),
        }
    }
    pub fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }

    /// 立即预留一个 permit（拿不到即 `Busy`）。扇出调用方在派生任务**之前**预留，
    /// 这样“窗口还能再开几个”只看真实剩余额度，不会把本请求已派生但尚未
    /// 取到 permit 的任务重复扣减（否则窗口会随调度时序缩小）。
    pub fn try_reserve(&self) -> Result<Reservation, BlockingError> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map(|permit| Reservation { permit })
            .map_err(|_| BlockingError::Busy)
    }

    /// 在 blocking 线程上执行 `work`，最多等待 `deadline`。
    ///
    /// 超时不会中止 `work`（也不会提前释放 permit）；调用方必须把
    /// `Timeout` 视为“结果未知”，不能当成动作未执行。
    pub async fn run<T: Send + 'static>(
        &self,
        deadline: Duration,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, BlockingError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| BlockingError::Busy)?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit: OwnedSemaphorePermit = permit;
            work()
        });
        match tokio::time::timeout(deadline, task).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(BlockingError::Worker(error.to_string())),
            Err(_) => Err(BlockingError::Timeout),
        }
    }

    /// 执行有副作用的动作：不包等待预算，一直等到 blocking worker 结束。
    pub async fn run_to_completion<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, BlockingError> {
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| BlockingError::Busy)?;
        tokio::task::spawn_blocking(move || {
            let _permit: OwnedSemaphorePermit = permit;
            work()
        })
        .await
        .map_err(|error| BlockingError::Worker(error.to_string()))
    }
}

/// `BlockingBudget::try_reserve` 预留的一个 permit；`run` 语义与 `BlockingBudget::run` 相同。
pub struct Reservation {
    permit: OwnedSemaphorePermit,
}

impl Reservation {
    /// Wait for side-effecting blocking work to actually finish.
    pub async fn run_to_completion<T: Send + 'static>(
        self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, BlockingError> {
        let permit = self.permit;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| BlockingError::Worker(error.to_string()))
    }

    /// 在 blocking 线程上执行 `work`，最多等待 `deadline`。
    ///
    /// Review L4 约定：`deadline` 到期**不会**中止已经在跑的闭包，所以闭包必须
    /// 1. 在每次拿到进程级 `Store` 锁之前调用 `budget.check()`，超时就不要再去拿锁；
    /// 2. 锁内只做 SQL——文件 IO / HTTP / 渲染留在拿锁之前或放锁之后；
    /// 3. 等锁写成有界等待（如 `try_lock_for`），并把超时分类成「繁忙、稍后重试」，
    ///    而不是把 worker 无限期挂在锁上。
    pub async fn run<T: Send + 'static>(
        self,
        deadline: Duration,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, BlockingError> {
        let permit = self.permit;
        let task = tokio::task::spawn_blocking(move || {
            let _permit: OwnedSemaphorePermit = permit;
            work()
        });
        match tokio::time::timeout(deadline, task).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(BlockingError::Worker(error.to_string())),
            Err(_) => Err(BlockingError::Timeout),
        }
    }
}
