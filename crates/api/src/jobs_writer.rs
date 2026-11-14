//! Bounded Jobs mutation admission. Reservations contain no database writer.
use parking_lot::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use store::{StoreError, WriterRank, WriterRankGuard};

pub struct JobsWriter {
    admission: Mutex<()>,
    queue: Mutex<jobs::Queue>,
}

pub struct JobsReservation<'a> {
    writer: &'a JobsWriter,
    _admission: MutexGuard<'a, ()>,
    _ownership: AdmissionOwnership,
}

pub struct JobsWriterGuard<'a> {
    // Release the mutex, then rank, then admission.
    queue: MutexGuard<'a, jobs::Queue>,
    _rank: WriterRankGuard,
    _admission: Option<MutexGuard<'a, ()>>,
    _ownership: Option<AdmissionOwnership>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum JobsMutationError {
    #[error(transparent)]
    Acquire(#[from] StoreError),
    #[error(transparent)]
    Queue(#[from] jobs::JobError),
}

impl JobsWriter {
    pub(crate) fn new(queue: jobs::Queue) -> Self {
        Self {
            admission: Mutex::new(()),
            queue: Mutex::new(queue),
        }
    }

    pub fn reserve(&self) -> Result<JobsReservation<'_>, StoreError> {
        self.reserve_for(crate::http::store_wait::STORE_WRITE_WAIT)
    }

    fn reserve_for(&self, wait: Duration) -> Result<JobsReservation<'_>, StoreError> {
        WriterRankGuard::ensure_unheld(WriterRank::Jobs)?;
        let ownership = AdmissionOwnership::enter()?;
        let wait = store::Writers::remaining_write_wait(wait);
        let admission = self
            .admission
            .try_lock_for(wait)
            .ok_or(StoreError::WriteBusy)?;
        store::Writers::check_write_admission()?;
        Ok(JobsReservation {
            writer: self,
            _admission: admission,
            _ownership: ownership,
        })
    }

    pub fn acquire(&self) -> Result<JobsWriterGuard<'_>, StoreError> {
        self.try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
    }

    pub fn try_lock_for(&self, wait: Duration) -> Result<JobsWriterGuard<'_>, StoreError> {
        let wait = wait.min(crate::http::store_wait::STORE_WRITE_WAIT);
        let started = Instant::now();
        let reservation = self.reserve_for(wait)?;
        let rank = WriterRankGuard::enter(WriterRank::Jobs)?;
        let remaining =
            store::Writers::remaining_write_wait(wait.saturating_sub(started.elapsed()));
        let queue = self
            .queue
            .try_lock_for(remaining)
            .ok_or(StoreError::WriteBusy)?;
        store::Writers::check_write_admission()?;
        Ok(JobsWriterGuard {
            queue,
            _rank: rank,
            _admission: Some(reservation._admission),
            _ownership: Some(reservation._ownership),
        })
    }
}

impl JobsReservation<'_> {
    pub fn acquire(&self) -> Result<JobsWriterGuard<'_>, StoreError> {
        WriterRankGuard::ensure_unheld(WriterRank::Jobs)?;
        let rank = WriterRankGuard::enter(WriterRank::Jobs)?;
        let queue = self
            .writer
            .queue
            .try_lock_for(store::Writers::remaining_write_wait(
                crate::http::store_wait::STORE_WRITE_WAIT,
            ))
            .ok_or(StoreError::WriteBusy)?;
        store::Writers::check_write_admission()?;
        Ok(JobsWriterGuard {
            queue,
            _rank: rank,
            _admission: None,
            _ownership: None,
        })
    }
}

impl std::ops::Deref for JobsWriterGuard<'_> {
    type Target = jobs::Queue;
    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}

thread_local! { static ADMISSION_HELD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
struct AdmissionOwnership(std::marker::PhantomData<std::rc::Rc<()>>);
impl AdmissionOwnership {
    fn enter() -> Result<Self, StoreError> {
        ADMISSION_HELD.with(|held| {
            if held.replace(true) {
                tracing::error!("Jobs admission reservation reentry");
                return Err(StoreError::LockOrder {
                    held: vec![WriterRank::Jobs],
                    requested: WriterRank::Jobs,
                });
            }
            Ok(Self(std::marker::PhantomData))
        })
    }
}
impl Drop for AdmissionOwnership {
    fn drop(&mut self) {
        ADMISSION_HELD.with(|held| held.set(false));
    }
}
