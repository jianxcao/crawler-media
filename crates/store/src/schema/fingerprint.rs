use rusqlite::Connection;

use crate::StoreError;

pub(crate) fn migrate_library_fingerprint(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS fingerprint_samples (
            sample_id TEXT PRIMARY KEY,
            ledger_id TEXT NOT NULL,
            source_version TEXT NOT NULL,
            capture_profile_key TEXT NOT NULL,
            kind TEXT NOT NULL,
            window_start_ms INTEGER NOT NULL,
            window_end_ms INTEGER NOT NULL,
            pcm_duration_ms INTEGER,
            fingerprint_json TEXT NOT NULL,
            captured_job_id TEXT NOT NULL,
            captured_at_ms INTEGER NOT NULL,
            metrics_json TEXT NOT NULL,
            UNIQUE (ledger_id, source_version, capture_profile_key, kind, window_start_ms, window_end_ms)
        );
        CREATE INDEX IF NOT EXISTS fingerprint_samples_lookup_idx
            ON fingerprint_samples(ledger_id, kind, source_version, capture_profile_key);

        CREATE TABLE IF NOT EXISTS fingerprint_season_models (
            model_id TEXT PRIMARY KEY,
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL,
            kind TEXT NOT NULL,
            model_version INTEGER NOT NULL,
            membership_key TEXT NOT NULL,
            policy_key TEXT NOT NULL,
            model_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS fingerprint_season_models_lookup_idx
            ON fingerprint_season_models(media_id, season, kind);

        CREATE TABLE IF NOT EXISTS fingerprint_model_members (
            model_id TEXT NOT NULL,
            sample_id TEXT NOT NULL,
            ledger_id TEXT NOT NULL,
            source_version TEXT NOT NULL,
            PRIMARY KEY (model_id, sample_id),
            FOREIGN KEY (model_id) REFERENCES fingerprint_season_models(model_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS fingerprint_model_members_sample_idx
            ON fingerprint_model_members(sample_id);
        CREATE INDEX IF NOT EXISTS fingerprint_model_members_ledger_idx
            ON fingerprint_model_members(ledger_id);

        CREATE TABLE IF NOT EXISTS fingerprint_attempts (
            attempt_id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL,
            ledger_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            window_start_ms INTEGER NOT NULL,
            window_end_ms INTEGER NOT NULL,
            phase TEXT NOT NULL,
            started_at_ms INTEGER NOT NULL,
            finished_at_ms INTEGER,
            status TEXT NOT NULL,
            error_kind TEXT,
            metrics_json TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS fingerprint_attempts_job_ledger_idx
            ON fingerprint_attempts(job_id, ledger_id);
        "#,
    )?;

    super::ensure_column(
        conn,
        "probe_job_units",
        "sampling_plan_json",
        "TEXT",
    )?;
    super::ensure_column(
        conn,
        "probe_job_units",
        "detection_outcome_json",
        "TEXT",
    )?;
    super::ensure_column(
        conn,
        "probe_job_units",
        "reuse_media_info_cache",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    super::ensure_column(
        conn,
        "probe_jobs",
        "analysis_policy_key",
        "TEXT",
    )?;
    super::ensure_column(
        conn,
        "probe_jobs",
        "sampling_mode",
        "TEXT",
    )?;

    Ok(())
}
