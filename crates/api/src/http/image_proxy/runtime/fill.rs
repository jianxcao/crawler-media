//! A fill keeps its registry entry and permit through actual worker completion.
use super::*;
use tokio::sync::watch;

type Outcome = Result<Arc<Image>, DeliveryError>;
pub(super) enum WaitResult {
    Shared(Outcome),
    Deadline,
}
const ADMISSION_CAPACITY: usize = 64;

#[derive(Clone, Debug, thiserror::Error)]
pub(in crate::http::image_proxy) enum DeliveryError {
    #[error("image delivery capacity exhausted")]
    Busy,
    #[error("image delivery deadline reached")]
    Timeout,
    #[error("image upstream failed: {0}")]
    Upstream(String),
    #[error("image cache failed: {0}")]
    Cache(String),
}
impl From<String> for DeliveryError {
    fn from(error: String) -> Self {
        Self::Upstream(error)
    }
}
impl From<&str> for DeliveryError {
    fn from(error: &str) -> Self {
        Self::Upstream(error.into())
    }
}
impl DeliveryError {
    pub(super) fn class(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Timeout => "timeout",
            Self::Upstream(_) => "upstream",
            Self::Cache(_) => "cache",
        }
    }
}
#[derive(Default)]
struct Completion {
    timed_out: bool,
    outcome: Option<(&'static str, u64)>,
    logged: bool,
}
pub(super) struct Fill {
    pub(super) id: u64,
    pub(super) progress: Arc<Progress>,
    started: Instant,
    completion: Mutex<Completion>,
    outcome: watch::Sender<Option<Outcome>>,
}
impl Fill {
    fn new() -> Self {
        Self {
            id: phase::id(),
            progress: Progress::new("fill_gate"),
            started: Instant::now(),
            completion: Mutex::new(Completion::default()),
            outcome: watch::channel(None).0,
        }
    }
    pub(super) async fn wait(&self, observer_deadline: Instant) -> WaitResult {
        let mut receiver = self.outcome.subscribe();
        loop {
            if let Some(outcome) = receiver.borrow().clone() {
                return WaitResult::Shared(outcome);
            }
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(observer_deadline),
                receiver.changed(),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return WaitResult::Shared(Err(DeliveryError::Cache(
                        "image worker lost its result".into(),
                    )));
                }
                Err(_) => return WaitResult::Deadline,
            }
        }
    }
    pub(super) fn observer_timed_out(&self) {
        let outcome = {
            let mut state = self.completion.lock();
            state.timed_out = true;
            Self::take_completion(&mut state)
        };
        self.log_completion(outcome);
    }
    fn finished(&self, outcome: &Outcome) {
        let outcome = {
            let mut state = self.completion.lock();
            let class = match outcome {
                Ok(_) => "success",
                Err(error) => error.class(),
            };
            state.outcome = Some((class, phase::millis(self.started)));
            Self::take_completion(&mut state)
        };
        self.log_completion(outcome);
    }
    fn take_completion(state: &mut Completion) -> Option<(&'static str, u64)> {
        if state.timed_out && !state.logged && state.outcome.is_some() {
            state.logged = true;
            state.outcome
        } else {
            None
        }
    }
    fn log_completion(&self, outcome: Option<(&'static str, u64)>) {
        if let Some((outcome, fill_elapsed_ms)) = outcome {
            tracing::info!(target: "api::image_delivery", event = "image_fill_complete", fill_id = self.id, outcome, fill_elapsed_ms);
        }
    }
}
impl ImageProxy {
    pub(super) fn start_fill(
        self: &Arc<Self>,
        url: String,
        deadline: Instant,
    ) -> Result<(Arc<Fill>, &'static str), DeliveryError> {
        let fill = {
            let mut pending = self.pending.lock();
            if let Some(fill) = pending.get(&url) {
                return Ok((fill.clone(), "joined"));
            }
            if pending.len() >= ADMISSION_CAPACITY {
                return Err(DeliveryError::Busy);
            }
            let fill = Arc::new(Fill::new());
            pending.insert(url.clone(), fill.clone());
            fill
        };
        let mut flight = Flight {
            proxy: self.clone(),
            url,
            fill: fill.clone(),
            completed: false,
        };
        let proxy = self.clone();
        tokio::spawn(async move {
            let permit = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                proxy.gate.clone().acquire_owned(),
            )
            .await;
            let permit = match permit {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => {
                    flight.complete(Err(DeliveryError::Busy));
                    return;
                }
                Err(_) => {
                    flight.complete(Err(DeliveryError::Timeout));
                    return;
                }
            };
            flight.fill.progress.set("fill_worker_queue");
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let outcome = phase::scope(flight.fill.progress.clone(), || {
                    fetch_cached(&proxy, &flight.url, deadline)
                });
                flight.complete(outcome);
            })
            .await;
            if let Err(error) = result {
                tracing::error!(%error, "Image delivery worker failed");
            }
        });
        Ok((fill, "leader"))
    }
}
fn fetch_cached(proxy: &ImageProxy, url: &str, deadline: Instant) -> Outcome {
    let mut fetch_error = None;
    let outcome = proxy.cache.get(url, || {
        if Instant::now() >= deadline {
            fetch_error = Some(DeliveryError::Timeout);
            return Err("image queue deadline reached".into());
        }
        // Synchronous transport retains this worker's permit until it really ends.
        phase::set("upstream_request");
        (proxy.fetch)(url).map_err(|error| {
            fetch_error = Some(error.clone());
            error.to_string()
        })
    });
    outcome
        .map(Arc::new)
        .map_err(|error| fetch_error.unwrap_or(DeliveryError::Cache(error)))
}
struct Flight {
    proxy: Arc<ImageProxy>,
    url: String,
    fill: Arc<Fill>,
    completed: bool,
}
impl Flight {
    fn complete(&mut self, outcome: Outcome) {
        if !self.completed {
            self.fill.finished(&outcome);
            self.fill.outcome.send_replace(Some(outcome));
            self.completed = true;
        }
    }
}
impl Drop for Flight {
    fn drop(&mut self) {
        if !self.completed {
            tracing::error!("Image worker terminated without a final result");
            self.complete(Err(DeliveryError::Cache(
                "image worker terminated unexpectedly".into(),
            )));
        }
        let mut pending = self.proxy.pending.lock();
        if pending
            .get(&self.url)
            .is_some_and(|current| Arc::ptr_eq(current, &self.fill))
        {
            pending.remove(&self.url);
        }
    }
}
