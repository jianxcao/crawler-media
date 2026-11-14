//! L5: 维护期写门禁。
//!
//! `VACUUM` 会拿独占锁。维护期间让前台写（播放进度、订阅、Library 写入）和它并发，
//! 写侧会先耗满 SQLite `busy_timeout`（5 s）再报 500——用户等了 5 秒才看到失败。
//! 维护开始时把门关上，写入口立刻拿到 503 + `Retry-After`，客户端按秒重试即可。
//!
//! 两件事刻意分开，不合成一个状态码：
//! - 实例只读（另一个实例持有数据目录租约）是 **409**，见 `ApiState::require_writable`；
//! - 维护关闸是 **503 + `Retry-After`**，code 复用身份读取繁忙的 `runtime.busy`。

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::{HeaderValue, StatusCode, header};

/// 门禁关闭期间的写请求拒绝原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteClosed;

/// 一次关闸的句柄：`drop` 时重新放行写请求。
///
/// 必须被持有到独占阶段结束；嵌套关闭按计数释放，只有最后一个句柄 drop 才放行。
#[must_use = "关门句柄一 drop 就重新放行写请求"]
pub struct GateGuard<'a> {
    gate: &'a WriteGate,
}

impl Drop for GateGuard<'_> {
    fn drop(&mut self) {
        // `close`/`drop` 严格配对，因此计数不会到 0 以下；泄漏的句柄只会让门禁
        // 保持关闭（fail-closed），不会错误放行。
        self.gate.closed.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 进程内写门禁。默认打开；维护任务在独占阶段关闭它。
#[derive(Default)]
pub struct WriteGate {
    closed: AtomicUsize,
}

impl WriteGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 关闸。返回的句柄 drop 时放行；可嵌套关闭。
    pub fn close(&self) -> GateGuard<'_> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        GateGuard { gate: self }
    }

    /// 前台写入口调用：门禁关着时返回 [`WriteClosed`]。
    ///
    /// 只读一次原子计数，不拿锁、不等待——写入口不该为门禁付出排队代价。
    pub fn admit(&self) -> Result<(), WriteClosed> {
        if self.is_closed() {
            Err(WriteClosed)
        } else {
            Ok(())
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) > 0
    }
}

/// 关闸期间的统一拒绝：503 + `Retry-After: 1`，code 与身份读取繁忙一致（`runtime.busy`）。
pub(crate) fn busy_response(_closed: WriteClosed) -> axum::response::Response {
    tracing::warn!("写门禁已关闭（维护中），写请求按繁忙返回 503");
    let mut response = crate::http::err(
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime.busy",
        "服务繁忙，请稍后重试",
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[cfg(test)]
mod tests {
    use super::WriteGate;

    #[test]
    fn admit_passes_while_open_and_fails_while_closed() {
        let gate = WriteGate::new();
        assert!(gate.admit().is_ok(), "默认必须放行");
        {
            let _closed = gate.close();
            assert!(gate.is_closed());
            assert!(gate.admit().is_err(), "关闸期间必须拒绝");
        }
        assert!(!gate.is_closed());
        assert!(gate.admit().is_ok(), "句柄 drop 后必须恢复");
    }

    #[test]
    fn nested_closes_release_only_after_the_last_guard() {
        let gate = WriteGate::new();
        let outer = gate.close();
        {
            let _inner = gate.close();
            assert!(gate.admit().is_err());
        }
        assert!(
            gate.admit().is_err(),
            "内层句柄 drop 后外层仍关闸，不能提前放行"
        );
        drop(outer);
        assert!(gate.admit().is_ok());
    }

    /// 门禁读路径不加锁：并发写请求之间不互相排队（唯一的共享状态是一次原子读）。
    #[test]
    fn concurrent_admits_observe_the_same_state() {
        let gate = std::sync::Arc::new(WriteGate::new());
        let closed = gate.close();
        let seen: Vec<bool> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let gate = std::sync::Arc::clone(&gate);
                    scope.spawn(move || gate.admit().is_err())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(seen.iter().all(|rejected| *rejected));
        drop(closed);
        assert!(gate.admit().is_ok());
    }
}
