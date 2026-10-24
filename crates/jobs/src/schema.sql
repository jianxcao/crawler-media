
        CREATE TABLE job_defs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            schedule TEXT,
            payload TEXT NOT NULL,
            timeout_secs INTEGER,
            concurrency_key TEXT
        );
        CREATE TABLE jobs (
            id TEXT PRIMARY KEY,
            def_id TEXT,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 0,
            run_after INTEGER NOT NULL,
            started_at INTEGER,
            finished_at INTEGER,
            error TEXT,
            progress TEXT,
            concurrency_key TEXT,
            timeout_secs INTEGER
        );
        CREATE UNIQUE INDEX jobs_running_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'running';
        CREATE UNIQUE INDEX jobs_queued_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'queued';
        CREATE INDEX jobs_status_run_after_idx
            ON jobs(status, run_after) WHERE status = 'queued';
        CREATE INDEX jobs_payload_idx ON jobs(payload);
        CREATE INDEX jobs_finished_idx ON jobs(finished_at)
            WHERE finished_at IS NOT NULL;
        -- DB5：热查询索引。20 万行合成数据 + ANALYZE 的 before/after EQP 与耗时见
        -- docs/audits/2026-10-12-remediation-review-repro/jobs-query-plans.md。
        -- 现有 jobs_running_def/jobs_queued_def 是部分**唯一**索引，status IN (...)
        -- 不蕴含 status='queued'，帮不上 live/last child；两者都必须保留。
        CREATE INDEX jobs_running_status_idx
            ON jobs(status) WHERE status = 'running';
        CREATE INDEX jobs_running_key_idx
            ON jobs(concurrency_key) WHERE status = 'running';
        CREATE INDEX jobs_live_def_idx
            ON jobs(def_id, status, run_after) WHERE status IN ('queued','running');
        CREATE INDEX jobs_last_def_idx
            ON jobs(def_id, finished_at DESC, run_after DESC)
            WHERE status IN ('succeeded','failed','cancelled');

CREATE TABLE retention_cursors (
    def_id TEXT PRIMARY KEY, cutoff INTEGER NOT NULL, finished_at INTEGER NOT NULL,
    last_rowid INTEGER NOT NULL, cycle_started_at INTEGER NOT NULL
);
CREATE TABLE job_change_state (
    id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL DEFAULT 0,
    finished_count INTEGER NOT NULL DEFAULT 0
);
INSERT INTO job_change_state(id, revision, finished_count) VALUES (1, 0, 0);
CREATE TRIGGER trg_jobs_change_ins AFTER INSERT ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;

         CREATE TRIGGER trg_jobs_change_upd AFTER UPDATE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count
                     - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
                     + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;

         CREATE TRIGGER trg_jobs_change_del AFTER DELETE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;

         CREATE TRIGGER trg_job_defs_change_ins AFTER INSERT ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;

         CREATE TRIGGER trg_job_defs_change_upd AFTER UPDATE ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;

         CREATE TRIGGER trg_job_defs_change_del AFTER DELETE ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;
