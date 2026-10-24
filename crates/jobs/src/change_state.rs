use rusqlite::Connection;

use crate::types::JobError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobChangeState {
    pub revision: i64,
    pub finished_count: i64,
}

pub fn get_job_change_state(conn: &Connection) -> Result<JobChangeState, JobError> {
    conn.query_row(
        "SELECT revision, finished_count FROM job_change_state WHERE id = 1",
        [],
        |r| {
            Ok(JobChangeState {
                revision: r.get(0)?,
                finished_count: r.get(1)?,
            })
        },
    )
    .map_err(Into::into)
}
