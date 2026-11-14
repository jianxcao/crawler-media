//! Jobs queries share two strict read-only connections, independently of Queue.
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use store::{LibraryReadError, QueryBudget, ReaderPool, StoreError};

const READ_WAIT: Duration = Duration::from_secs(2);

pub(crate) fn open(path: &Path) -> Result<Arc<ReaderPool>, StoreError> {
    ReaderPool::open(
        path.to_path_buf(),
        NonZeroUsize::new(2).expect("Jobs reader capacity is nonzero"),
        "jobs",
        validate,
    )
    .map(Arc::new)
}

fn validate(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    jobs::schema::validate_layout(conn).map_err(job_error)
}

fn job_error(error: jobs::JobError) -> StoreError {
    match error {
        jobs::JobError::Layout(message) => StoreError::LayoutMismatch(message),
        error => StoreError::Io(std::io::Error::other(error.to_string())),
    }
}

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + READ_WAIT,
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(crate) fn read<T>(
    readers: &ReaderPool,
    work: impl FnOnce(&jobs::QueueRead<'_>) -> Result<T, jobs::JobError>,
) -> Result<T, LibraryReadError> {
    let budget = budget();
    let mut lease = readers.checkout(READ_WAIT)?;
    lease.with_connection(&budget, |conn| {
        work(&jobs::QueueRead::new(conn)).map_err(job_error)
    })
}

pub(crate) async fn read_async<T: Send + 'static>(
    readers: Arc<ReaderPool>,
    work: impl FnOnce(&jobs::QueueRead<'_>) -> Result<T, jobs::JobError> + Send + 'static,
) -> Result<T, LibraryReadError> {
    let budget = budget();
    let _cancel = CancelOnDrop(budget.cancelled.clone());
    let mut lease = readers.checkout_async(READ_WAIT).await?;
    tokio::task::spawn_blocking(move || {
        lease.with_connection(&budget, |conn| {
            work(&jobs::QueueRead::new(conn)).map_err(job_error)
        })
    })
    .await
    .map_err(|error| StoreError::Protected(format!("Jobs reader worker failed: {error}")))?
}
