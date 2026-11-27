CREATE TABLE subscribes(id TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,media_id TEXT NOT NULL,coverage_kind TEXT NOT NULL,season INTEGER,episode_from INTEGER,episode_to INTEGER,fetch_mode TEXT NOT NULL,filter_id TEXT NOT NULL,wash_cut INTEGER NOT NULL DEFAULT 0,wash_cut_filter_id TEXT,full_season_pack INTEGER NOT NULL DEFAULT 0,downloader_id TEXT,tracking_state TEXT NOT NULL DEFAULT 'active',follow_future INTEGER NOT NULL DEFAULT 0,search_interval_secs INTEGER NOT NULL DEFAULT 1800,keep_old_versions INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,library_id TEXT,jobs_sync_generation TEXT NOT NULL,lifecycle_epoch INTEGER NOT NULL,jobs_sync_revision INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_revision>0),jobs_sync_pending INTEGER NOT NULL DEFAULT 0 CHECK(jobs_sync_pending IN (0,1)),jobs_sync_create INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_create IN (0,1)),policy_revision INTEGER NOT NULL DEFAULT 1 CHECK(policy_revision>0));
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
CREATE INDEX subscribe_reference_media_pending ON subscribe_reference_ops(media_id,epoch) WHERE state IN ('creating','retained');
CREATE UNIQUE INDEX subscribe_creating_id ON subscribe_reference_ops(subscribe_id) WHERE state IN ('creating','retained');
CREATE TABLE subscribe_merge_fences(media_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL);
CREATE TABLE subscribe_library_fences(library_id TEXT PRIMARY KEY NOT NULL);

CREATE TABLE subscribe_merge_controls(operation_id TEXT PRIMARY KEY,state TEXT NOT NULL CHECK(state IN ('active','finished')));

CREATE TABLE subscribe_recovery_cursor(id INTEGER PRIMARY KEY CHECK(id=1),epoch INTEGER NOT NULL);
INSERT INTO subscribe_recovery_cursor VALUES(1,0);

CREATE TABLE download_operations(
  operation_id TEXT PRIMARY KEY NOT NULL CHECK(length(operation_id)>0),
  idempotency_key TEXT NOT NULL CHECK(length(idempotency_key)>0),
  source_kind TEXT NOT NULL CHECK(length(source_kind)>0),
  subscribe_id TEXT,
  plugin_id TEXT,
  subject TEXT NOT NULL CHECK(length(subject)>0),
  cause_id TEXT NOT NULL CHECK(length(cause_id)>0),
  site_id TEXT,
  downloader_id TEXT,
  endpoint_fingerprint TEXT,
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT,
  bytes_digest TEXT,
  verified_bytes INTEGER CHECK(verified_bytes IS NULL OR verified_bytes>=0),
  request_digest TEXT NOT NULL CHECK(length(request_digest)>0),
  state TEXT NOT NULL CHECK(state IN ('intent','reserved','submitting','submitted','rejected','deferred','failed','unknown')),
  outcome_code TEXT,
  error_message TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  submitted_at INTEGER,
  reconciled_at INTEGER,
  UNIQUE(subject,idempotency_key),
  CHECK(state NOT IN ('submitting','submitted','unknown') OR downloader_id IS NOT NULL)
);
CREATE INDEX download_operations_subscribe_idx ON download_operations(subscribe_id,updated_at DESC,operation_id) WHERE subscribe_id IS NOT NULL;
CREATE INDEX download_operations_unresolved_idx ON download_operations(updated_at,operation_id) WHERE state IN ('intent','reserved','submitting','unknown');
CREATE INDEX download_operations_scope_idx ON download_operations(plugin_id,site_id,downloader_id,state,updated_at DESC,operation_id);
CREATE INDEX download_operations_hash_idx ON download_operations(downloader_id,info_hash) WHERE info_hash IS NOT NULL;

CREATE TABLE download_ownership(
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT,
  operation_id TEXT NOT NULL REFERENCES download_operations(operation_id),
  subscribe_id TEXT,
  plugin_id TEXT,
  subject TEXT NOT NULL CHECK(length(subject)>0),
  site_id TEXT,
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  ownership_state TEXT NOT NULL CHECK(ownership_state IN ('owned','foreign','unproven','broken','deleted')),
  provenance_kind TEXT NOT NULL CHECK(provenance_kind IN ('created_fresh','reconciled_operation','existing_owned','existing_foreign','broken_existing','unproven')),
  verified_bytes INTEGER CHECK(verified_bytes IS NULL OR verified_bytes>=0),
  bytes_digest TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(downloader_id,ownership_tag)
);
CREATE INDEX download_ownership_hash_idx ON download_ownership(downloader_id,info_hash) WHERE info_hash IS NOT NULL;
CREATE INDEX download_ownership_plugin_idx ON download_ownership(plugin_id,downloader_id,ownership_state,updated_at DESC,ownership_tag);
CREATE INDEX download_ownership_subscribe_idx ON download_ownership(subscribe_id) WHERE subscribe_id IS NOT NULL;

CREATE TABLE boost_reservations(
  reservation_id TEXT PRIMARY KEY NOT NULL CHECK(length(reservation_id)>0),
  operation_id TEXT NOT NULL UNIQUE REFERENCES download_operations(operation_id),
  quota_key TEXT NOT NULL CHECK(length(quota_key)>0),
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  site_id TEXT,
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  info_hash TEXT,
  bytes_digest TEXT,
  reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes>=0),
  stage TEXT NOT NULL CHECK(stage IN ('provisional','final','unknown_held','committed','released')),
  expires_at INTEGER,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK(stage NOT IN ('final','unknown_held') OR bytes_digest IS NOT NULL)
);
CREATE INDEX boost_reservations_quota_active_idx ON boost_reservations(quota_key,stage,updated_at,reservation_id) WHERE stage IN ('provisional','final','unknown_held');
CREATE INDEX boost_reservations_plugin_hourly_idx ON boost_reservations(plugin_id,created_at) WHERE stage IN ('final','unknown_held','committed');
CREATE UNIQUE INDEX boost_reservations_active_hash_idx ON boost_reservations(quota_key,downloader_id,info_hash) WHERE info_hash IS NOT NULL AND stage IN ('provisional','final','unknown_held');

CREATE TABLE boost_task_facts(
  task_fact_id TEXT PRIMARY KEY NOT NULL CHECK(length(task_fact_id)>0),
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  quota_key TEXT NOT NULL CHECK(length(quota_key)>0),
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT NOT NULL CHECK(length(info_hash)>0),
  site_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('downloading','seeding','paused','retained_only','cleanup_planned','cleaned')),
  occupied_bytes INTEGER NOT NULL CHECK(occupied_bytes>=0),
  retained_bytes INTEGER NOT NULL DEFAULT 0 CHECK(retained_bytes>=0),
  hr_active INTEGER CHECK(hr_active IS NULL OR hr_active IN (0,1)),
  hr_until INTEGER,
  hold_until INTEGER,
  free_until INTEGER,
  protection_state TEXT NOT NULL DEFAULT 'unknown' CHECK(protection_state IN ('unknown','protected','eligible','blocked')),
  protection_reason TEXT,
  cleanup_plan_handle TEXT,
  cleanup_plan_expires_at INTEGER,
  cleanup_plan_json TEXT CHECK(cleanup_plan_json IS NULL OR json_valid(cleanup_plan_json)),
  observed_at INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(downloader_id,ownership_tag) REFERENCES download_ownership(downloader_id,ownership_tag),
  UNIQUE(quota_key,downloader_id,info_hash),
  CHECK((state IN ('downloading','seeding','paused') AND retained_bytes=0) OR (state IN ('retained_only','cleanup_planned') AND occupied_bytes=0) OR (state='cleaned' AND occupied_bytes=0 AND retained_bytes=0))
);
CREATE INDEX boost_task_facts_quota_state_idx ON boost_task_facts(quota_key,state,updated_at DESC,task_fact_id);
CREATE INDEX boost_task_facts_plan_idx ON boost_task_facts(cleanup_plan_handle) WHERE cleanup_plan_handle IS NOT NULL;
CREATE INDEX boost_task_facts_plugin_idx ON boost_task_facts(plugin_id,downloader_id,state,task_fact_id);

CREATE TABLE subscribe_deferred_candidates(
  defer_id TEXT PRIMARY KEY NOT NULL CHECK(length(defer_id)>0),
  subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,
  subscribe_generation TEXT NOT NULL CHECK(length(subscribe_generation)>0),
  policy_revision INTEGER NOT NULL CHECK(policy_revision>0),
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  info_hash TEXT,
  plugin_id TEXT,
  hook_id TEXT NOT NULL CHECK(length(hook_id)>0),
  reason TEXT NOT NULL CHECK(length(reason)>0),
  attempt_count INTEGER NOT NULL CHECK(attempt_count>=1 AND attempt_count<=10),
  first_deferred_at INTEGER NOT NULL,
  next_retry_at INTEGER NOT NULL,
  max_deadline_at INTEGER NOT NULL CHECK(max_deadline_at>=first_deferred_at AND max_deadline_at<=first_deferred_at+259200),
  state TEXT NOT NULL CHECK(state IN ('deferred','rejected','invalidated','resolved')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscribe_id,subscribe_generation,policy_revision,enclosure)
);
CREATE INDEX subscribe_deferred_due_idx ON subscribe_deferred_candidates(next_retry_at,defer_id) WHERE state='deferred';
CREATE INDEX subscribe_deferred_sub_idx ON subscribe_deferred_candidates(subscribe_id,state,enclosure);
-- Plugin event outbox and per-subscriber delivery state (timestamps in Unix milliseconds).
CREATE TABLE plugin_event_entity_sequences(
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  last_sequence INTEGER NOT NULL CHECK(last_sequence>0),
  PRIMARY KEY(entity_type,entity_id)
) WITHOUT ROWID;

CREATE TABLE plugin_event_outbox(
  event_id TEXT PRIMARY KEY NOT NULL CHECK(length(event_id)>0),
  schema_version INTEGER NOT NULL DEFAULT 1 CHECK(schema_version>0),
  event_domain TEXT NOT NULL CHECK(length(event_domain)>0),
  event_kind TEXT NOT NULL CHECK(length(event_kind)>0),
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  entity_sequence INTEGER NOT NULL CHECK(entity_sequence>0),
  entity_revision INTEGER NOT NULL DEFAULT 1 CHECK(entity_revision>=0),
  cause_id TEXT NOT NULL CHECK(length(cause_id)>0),
  subject TEXT NOT NULL DEFAULT 'system' CHECK(length(subject)>0),
  site_id TEXT CHECK(site_id IS NULL OR length(site_id)>0),
  subscribe_id TEXT CHECK(subscribe_id IS NULL OR length(subscribe_id)>0),
  library_id TEXT CHECK(library_id IS NULL OR length(library_id)>0),
  handles_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(handles_json) AND json_type(handles_json)='array'),
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json) AND json_type(payload_json)='object'),
  occurred_at INTEGER NOT NULL CHECK(occurred_at>=0),
  fanout_state TEXT NOT NULL DEFAULT 'pending' CHECK(fanout_state IN ('pending','seeded')),
  created_at INTEGER NOT NULL CHECK(created_at>=0),
  UNIQUE(entity_type,entity_id,entity_sequence)
);
CREATE INDEX plugin_event_outbox_pending_fanout_idx ON plugin_event_outbox(created_at,event_id) WHERE fanout_state='pending';
CREATE INDEX plugin_event_outbox_created_idx ON plugin_event_outbox(created_at,event_id);
CREATE INDEX plugin_event_outbox_domain_created_idx ON plugin_event_outbox(event_domain,event_kind,created_at,event_id);
CREATE INDEX plugin_event_outbox_entity_created_idx ON plugin_event_outbox(entity_type,entity_id,created_at,event_id);

CREATE TABLE plugin_event_deliveries(
  event_id TEXT NOT NULL REFERENCES plugin_event_outbox(event_id) ON DELETE CASCADE,
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  entity_sequence INTEGER NOT NULL CHECK(entity_sequence>0),
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','delivering','delivered','dead_letter')),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count>=0),
  next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at>=0),
  claimed_at INTEGER CHECK(claimed_at IS NULL OR claimed_at>=0),
  lease_until INTEGER CHECK(lease_until IS NULL OR lease_until>=0),
  claim_nonce TEXT CHECK(claim_nonce IS NULL OR length(claim_nonce)>0),
  last_error TEXT,
  component_state_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(component_state_json) AND json_type(component_state_json)='object'),
  delivered_at INTEGER CHECK(delivered_at IS NULL OR delivered_at>=0),
  dead_lettered_at INTEGER CHECK(dead_lettered_at IS NULL OR dead_lettered_at>=0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL CHECK(created_at>=0),
  updated_at INTEGER NOT NULL CHECK(updated_at>=0),
  PRIMARY KEY(event_id,plugin_id),
  CHECK((status='delivering')=(claimed_at IS NOT NULL)),
  CHECK((status='delivering')=(lease_until IS NOT NULL)),
  CHECK((status='delivering')=(claim_nonce IS NOT NULL)),
  CHECK(status!='delivering' OR (attempt_count>0 AND lease_until>=claimed_at)),
  CHECK((status='delivered')=(delivered_at IS NOT NULL)),
  CHECK((status='dead_letter')=(dead_lettered_at IS NOT NULL)),
  CHECK(status!='dead_letter' OR (last_error IS NOT NULL AND length(last_error)>0))
);
CREATE INDEX plugin_event_deliveries_due_idx ON plugin_event_deliveries(next_attempt_at,event_id,plugin_id) WHERE status='pending';
CREATE INDEX plugin_event_deliveries_lease_idx ON plugin_event_deliveries(lease_until,event_id,plugin_id) WHERE status='delivering';
CREATE INDEX plugin_event_deliveries_entity_order_idx ON plugin_event_deliveries(plugin_id,entity_type,entity_id,entity_sequence) WHERE status IN ('pending','delivering');
CREATE INDEX plugin_event_deliveries_dead_letter_idx ON plugin_event_deliveries(dead_lettered_at DESC,event_id,plugin_id) WHERE status='dead_letter';
CREATE INDEX plugin_event_deliveries_plugin_status_idx ON plugin_event_deliveries(plugin_id,status,updated_at DESC,event_id);
