//! Read operations borrow a connection whose owner enforces read-only policy.
use domain::{JobDefId, JobId};
use rusqlite::Connection;

use crate::{Job, JobChildren, JobDef, JobError};

/// A read-only query surface over a leased Jobs connection.
/// Construction performs no DDL, connection opening or queue initialization.
pub struct QueueRead<'a> {
    conn: &'a Connection,
}

impl<'a> QueueRead<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn list_defs(&self) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::list_defs(self.conn)
    }

    pub fn def(&self, id: JobDefId) -> Result<Option<JobDef>, JobError> {
        crate::schedule::get_def(self.conn, id)
    }

    pub fn def_name(&self, id: JobDefId) -> Result<Option<String>, JobError> {
        Ok(self.def(id)?.map(|def| def.name))
    }

    pub fn get(&self, id: JobId) -> Result<Option<Job>, JobError> {
        crate::store::get_job(self.conn, id)
    }

    pub fn defs_for_payload(&self, payload: &str) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::defs_for_payload(self.conn, payload)
    }

    pub fn jobs_for_payload(&self, payload: &str, limit: usize) -> Result<Vec<Job>, JobError> {
        crate::store::jobs_for_payload(self.conn, payload, limit)
    }

    pub fn change_state(&self) -> Result<crate::JobChangeState, JobError> {
        crate::change_state::get_job_change_state(self.conn)
    }

    pub fn count_finished_since(&self, since: i64) -> Result<i64, JobError> {
        crate::store::count_finished_since(self.conn, since)
    }

    pub fn get_live_child(&self, id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_live_child(self.conn, id)
    }

    pub fn has_running_child(&self, id: JobDefId) -> Result<bool, JobError> {
        crate::schedule::has_running_child(self.conn, id)
    }

    pub fn get_last_child(&self, id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_last_child(self.conn, id)
    }

    pub fn children_for_defs(&self, ids: &[JobDefId]) -> Result<Vec<JobChildren>, JobError> {
        crate::read::children_for_defs(self.conn, ids)
    }

    pub fn preview_terminal_window(
        &self,
        cursor_key: &str,
        cutoff: i64,
        limit: usize,
    ) -> Result<crate::RetentionWindow, JobError> {
        crate::retention_cursor::preview_terminal_window(self.conn, cursor_key, cutoff, limit)
    }
}
