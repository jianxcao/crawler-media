CREATE TABLE subscribes(id TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,media_id TEXT NOT NULL,coverage_kind TEXT NOT NULL,season INTEGER,episode_from INTEGER,episode_to INTEGER,fetch_mode TEXT NOT NULL,filter_id TEXT NOT NULL,wash_cut INTEGER NOT NULL DEFAULT 0,wash_cut_filter_id TEXT,full_season_pack INTEGER NOT NULL DEFAULT 0,downloader_id TEXT,tracking_state TEXT NOT NULL DEFAULT 'active',follow_future INTEGER NOT NULL DEFAULT 0,search_interval_secs INTEGER NOT NULL DEFAULT 1800,keep_old_versions INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,library_id TEXT,jobs_sync_generation TEXT NOT NULL,lifecycle_epoch INTEGER NOT NULL,jobs_sync_revision INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_revision>0),jobs_sync_pending INTEGER NOT NULL DEFAULT 0 CHECK(jobs_sync_pending IN (0,1)),jobs_sync_create INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_create IN (0,1)));
CREATE TABLE subscribe_facts(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,score INTEGER NOT NULL,path TEXT,PRIMARY KEY(subscribe_id,season,episode));
CREATE TABLE subscribe_wanted(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,search_attempts INTEGER NOT NULL DEFAULT 0,last_search_at INTEGER,grabbed_at INTEGER,imported_at INTEGER,grab_title TEXT,last_reject_reason TEXT,PRIMARY KEY(subscribe_id,season,episode));
CREATE TABLE pending_downloads(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,enclosure TEXT NOT NULL,title TEXT NOT NULL,score INTEGER NOT NULL,torrent_json TEXT NOT NULL,submitted_at INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL DEFAULT 'active',last_progress REAL,last_progress_at INTEGER,info_hash TEXT,selective_missing TEXT,probe_wait_attempts INTEGER,probe_wait_since INTEGER,PRIMARY KEY(subscribe_id,enclosure));
CREATE INDEX subscribes_user_idx ON subscribes(user_id);
CREATE INDEX subscribes_media_idx ON subscribes(media_id);
CREATE INDEX pending_downloads_sub_state_idx ON pending_downloads(subscribe_id,state);

CREATE INDEX subscribes_jobs_sync ON subscribes(id) WHERE jobs_sync_pending=1;

CREATE TABLE subscribe_jobs_cleanup(id TEXT PRIMARY KEY NOT NULL,generation TEXT NOT NULL);

CREATE TABLE subscribe_reference_ops(epoch INTEGER PRIMARY KEY AUTOINCREMENT,subscribe_id TEXT NOT NULL,generation TEXT NOT NULL UNIQUE,media_id TEXT NOT NULL,release_mode TEXT NOT NULL CHECK(release_mode IN ('ordinary','compensate')),compensation_json TEXT,state TEXT NOT NULL CHECK(state IN ('creating','retained','committed','aborting','releasing','done')));
CREATE INDEX subscribe_reference_pending ON subscribe_reference_ops(state,epoch);
CREATE UNIQUE INDEX subscribe_creating_id ON subscribe_reference_ops(subscribe_id) WHERE state IN ('creating','retained');
CREATE TABLE subscribe_merge_fences(media_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL);

CREATE TABLE subscribe_merge_controls(operation_id TEXT PRIMARY KEY,state TEXT NOT NULL CHECK(state IN ('active','finished')));

CREATE TABLE subscribe_recovery_cursor(id INTEGER PRIMARY KEY CHECK(id=1),epoch INTEGER NOT NULL);
INSERT INTO subscribe_recovery_cursor VALUES(1,0);
