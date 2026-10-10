use rusqlite::Connection;
use super::StoreError;

pub const PROBE_STATE_TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS probe_stage_state (
    ledger_id TEXT NOT NULL,
    context_key TEXT NOT NULL,
    stage TEXT NOT NULL,
    status TEXT NOT NULL,
    failure_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at_ms INTEGER,
    error_kind TEXT,
    error TEXT,
    active_job_id TEXT,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (ledger_id, context_key, stage)
);

CREATE INDEX IF NOT EXISTS probe_stage_retry_idx
    ON probe_stage_state (status, next_retry_at_ms)
    WHERE status = 'failed' AND next_retry_at_ms IS NOT NULL;

CREATE TABLE IF NOT EXISTS probe_season_comparison_history (
    media_id TEXT NOT NULL,
    season INTEGER NOT NULL,
    comparison_digest TEXT NOT NULL,
    published_at_ms INTEGER NOT NULL,
    PRIMARY KEY (media_id, season)
);
"#;

pub fn migrate_probe_state(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(PROBE_STATE_TABLES)?;
    Ok(())
}
