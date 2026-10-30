//! The Jobs writer follows the same database lock order as Store writers.
use parking_lot::{Mutex, MutexGuard};
use store::{WriterRank, WriterRankGuard};

pub struct JobsWriter(Mutex<jobs::Queue>);

pub struct JobsWriterGuard<'a> {
    // Release the actual mutex before removing its rank from the thread's stack.
    queue: MutexGuard<'a, jobs::Queue>,
    _rank: WriterRankGuard,
}

impl JobsWriter {
    pub(crate) fn new(queue: jobs::Queue) -> Self {
        Self(Mutex::new(queue))
    }

    pub fn lock(&self) -> JobsWriterGuard<'_> {
        let rank = WriterRankGuard::enter(WriterRank::Jobs);
        JobsWriterGuard {
            queue: self.0.lock(),
            _rank: rank,
        }
    }

    pub fn try_lock(&self) -> Option<JobsWriterGuard<'_>> {
        let rank = WriterRankGuard::enter(WriterRank::Jobs);
        Some(JobsWriterGuard {
            queue: self.0.try_lock()?,
            _rank: rank,
        })
    }

    pub fn try_lock_for(&self, wait: std::time::Duration) -> Option<JobsWriterGuard<'_>> {
        let rank = WriterRankGuard::enter(WriterRank::Jobs);
        Some(JobsWriterGuard {
            queue: self.0.try_lock_for(wait)?,
            _rank: rank,
        })
    }
}

impl std::ops::Deref for JobsWriterGuard<'_> {
    type Target = jobs::Queue;
    fn deref(&self) -> &Self::Target {
        &self.queue
    }
}
