CREATE TABLE instance_lock(id INTEGER PRIMARY KEY CHECK(id=1),owner TEXT NOT NULL,heartbeat_at INTEGER NOT NULL);
CREATE TABLE users(id TEXT PRIMARY KEY NOT NULL,login TEXT NOT NULL UNIQUE,enabled INTEGER NOT NULL DEFAULT 1,role TEXT NOT NULL DEFAULT 'member',password TEXT NOT NULL DEFAULT '');
CREATE TABLE sites(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,profile_id TEXT NOT NULL,cookie TEXT,api_key TEXT,rss_url TEXT,proxy TEXT,rate_limit_per_minute INTEGER,cdp_url TEXT,downloader_id TEXT,enabled INTEGER NOT NULL DEFAULT 1,maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK(maintenance_via IN ('http','browser')),browser_search INTEGER NOT NULL DEFAULT 0 CHECK(browser_search IN (0,1)),timezone TEXT NOT NULL DEFAULT 'UTC' CHECK(length(timezone)>0),revision INTEGER NOT NULL CHECK(revision > 0),credential_epoch TEXT NOT NULL);
CREATE INDEX sites_enabled_idx ON sites(enabled);
CREATE TABLE filters(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,atoms_json TEXT NOT NULL,keep_old_versions INTEGER NOT NULL DEFAULT 0);
CREATE TABLE user_tokens(token TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,created_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX user_tokens_user_idx ON user_tokens(user_id);
CREATE TABLE playback_device_credentials(user_id TEXT NOT NULL,device_id TEXT NOT NULL,token TEXT NOT NULL REFERENCES user_tokens(token) ON DELETE CASCADE,PRIMARY KEY(user_id,device_id,token));
CREATE TABLE settings(key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL);
CREATE INDEX playback_credentials_token ON playback_device_credentials(token);
CREATE TABLE search_snapshots(id TEXT PRIMARY KEY NOT NULL,vertical TEXT NOT NULL,query TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE downloaders(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,url TEXT NOT NULL,username TEXT,password TEXT,category TEXT,is_default INTEGER NOT NULL DEFAULT 0,path_maps TEXT,enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE plugins(plugin_id TEXT PRIMARY KEY NOT NULL,active_package_hash TEXT NOT NULL,lifecycle TEXT NOT NULL CHECK(lifecycle IN ('installed-disabled','enabling','enabled','draining','disabled')),desired_enabled INTEGER NOT NULL CHECK(desired_enabled IN (0,1)),revision INTEGER NOT NULL CHECK(revision>0),config_revision INTEGER NOT NULL CHECK(config_revision>=0),generation INTEGER NOT NULL CHECK(generation>=0),active_operation_id TEXT,failure_reason TEXT);
CREATE TABLE plugin_versions(plugin_id TEXT NOT NULL REFERENCES plugins(plugin_id),package_hash TEXT NOT NULL,version TEXT NOT NULL,manifest_json TEXT NOT NULL,config_schema_json TEXT NOT NULL,state_schema TEXT NOT NULL,artifact_path TEXT NOT NULL,signing_key_id TEXT,signature_status TEXT NOT NULL,ui_metadata_ref TEXT,credential_bound INTEGER NOT NULL CHECK(credential_bound IN (0,1)),PRIMARY KEY(plugin_id,package_hash),UNIQUE(plugin_id,version));
CREATE TABLE plugin_configs(plugin_id TEXT PRIMARY KEY NOT NULL REFERENCES plugins(plugin_id),schema_revision TEXT NOT NULL,config_revision INTEGER NOT NULL CHECK(config_revision>0),public_json TEXT NOT NULL);
CREATE TABLE plugin_grants(plugin_id TEXT NOT NULL,package_hash TEXT NOT NULL,capability TEXT NOT NULL,object_scope TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('granted','revoked')),sensitive INTEGER NOT NULL CHECK(sensitive IN (0,1)),PRIMARY KEY(plugin_id,package_hash,capability,object_scope),FOREIGN KEY(plugin_id,package_hash) REFERENCES plugin_versions(plugin_id,package_hash));
CREATE TABLE plugin_kv(plugin_id TEXT NOT NULL,security_scope TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),value BLOB NOT NULL CHECK(length(value)<=65536),PRIMARY KEY(plugin_id,security_scope,key),CHECK(length(CAST(key AS BLOB))<=256));
CREATE TABLE plugin_blobs(plugin_id TEXT NOT NULL,security_scope TEXT NOT NULL,key TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),value BLOB NOT NULL CHECK(length(value)<=1048576),PRIMARY KEY(plugin_id,security_scope,key),CHECK(length(CAST(key AS BLOB))<=256));
CREATE TABLE plugin_quota(plugin_id TEXT NOT NULL,security_scope TEXT NOT NULL,kv_bytes INTEGER NOT NULL CHECK(kv_bytes>=0 AND kv_bytes<=5242880),blob_bytes INTEGER NOT NULL CHECK(blob_bytes>=0 AND blob_bytes<=52428800),revision INTEGER NOT NULL CHECK(revision>=0),PRIMARY KEY(plugin_id,security_scope));
CREATE TABLE plugin_config_secrets(plugin_id TEXT NOT NULL REFERENCES plugins(plugin_id),security_scope TEXT NOT NULL,key TEXT NOT NULL,value BLOB NOT NULL CHECK(length(value)<=65536),PRIMARY KEY(plugin_id,security_scope,key),CHECK(length(CAST(key AS BLOB))<=256));
CREATE TABLE plugin_operations(operation_id TEXT PRIMARY KEY NOT NULL,plugin_id TEXT REFERENCES plugins(plugin_id),plugin_scope TEXT NOT NULL,subject TEXT NOT NULL,idempotency_key TEXT NOT NULL,kind TEXT NOT NULL,canonical_method TEXT NOT NULL,canonical_path TEXT NOT NULL,body_digest BLOB NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),generation INTEGER,state TEXT NOT NULL CHECK(state IN ('queued','running','waiting','blocked','succeeded','rejected','deferred','failed','cancelled','unknown')),phase TEXT NOT NULL,next_run_at INTEGER,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(subject,plugin_scope,idempotency_key),CHECK(state!='deferred' OR (next_run_at IS NOT NULL AND next_run_at>=0)));
CREATE INDEX plugin_operations_pending ON plugin_operations(COALESCE(next_run_at,0),operation_id) WHERE state IN ('queued','deferred');
CREATE INDEX plugin_operations_subject ON plugin_operations(subject,operation_id);
CREATE INDEX plugin_operations_plugin ON plugin_operations(plugin_id,state,operation_id);
CREATE INDEX plugin_operations_unresolved ON plugin_operations(operation_id) WHERE state NOT IN ('succeeded','rejected','failed','cancelled');
CREATE TABLE plugin_commands(operation_id TEXT PRIMARY KEY NOT NULL REFERENCES plugin_operations(operation_id),command_class TEXT NOT NULL CHECK(command_class IN ('management','runtime')),package_hash TEXT,expected_revision INTEGER,security_scope TEXT,authority_generation INTEGER,target_scopes TEXT NOT NULL,cause_id TEXT NOT NULL,cause_depth INTEGER NOT NULL CHECK(cause_depth>=0 AND cause_depth<=4),cause_visited TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),attempt INTEGER NOT NULL CHECK(attempt>=0),claim_nonce TEXT,effect TEXT NOT NULL CHECK(effect IN ('none','confirmed','unknown')),CHECK(command_class!='runtime' OR (package_hash IS NOT NULL AND security_scope IS NOT NULL AND authority_generation IS NOT NULL AND target_scopes!='[]')));
CREATE TABLE plugin_command_payloads(operation_id TEXT PRIMARY KEY NOT NULL REFERENCES plugin_commands(operation_id),security_scope TEXT NOT NULL,payload BLOB NOT NULL CHECK(length(payload)<=1048576));
CREATE TABLE plugin_operation_results(operation_id TEXT PRIMARY KEY NOT NULL REFERENCES plugin_operations(operation_id),security_scope TEXT NOT NULL,result BLOB NOT NULL CHECK(length(result)<=1048576));
CREATE TABLE plugin_task_bindings(binding_id TEXT PRIMARY KEY NOT NULL,plugin_id TEXT NOT NULL REFERENCES plugins(plugin_id),task_id TEXT NOT NULL,definition_revision INTEGER NOT NULL CHECK(definition_revision>0),generation INTEGER NOT NULL CHECK(generation>=0),enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),interval_secs INTEGER CHECK(interval_secs>0),public_config TEXT NOT NULL,projection_revision INTEGER NOT NULL CHECK(projection_revision>=0),projection_generation INTEGER,job_def_id TEXT,UNIQUE(plugin_id,task_id));
CREATE INDEX plugin_task_bindings_pending ON plugin_task_bindings(binding_id) WHERE definition_revision!=projection_revision OR generation IS NOT projection_generation;
CREATE TABLE plugin_trust_keys(key_id TEXT PRIMARY KEY NOT NULL CHECK(length(key_id)>0),label TEXT NOT NULL CHECK(length(label)>0),public_key TEXT NOT NULL CHECK(length(public_key)>0),fingerprint TEXT NOT NULL UNIQUE CHECK(length(fingerprint)>0),status TEXT NOT NULL CHECK(status IN ('trusted','revoked')),revision INTEGER NOT NULL CHECK(revision>0),created_at INTEGER NOT NULL,created_by TEXT NOT NULL CHECK(length(created_by)>0),revoked_at INTEGER,revoked_by TEXT,revoked_reason TEXT,CHECK((status='revoked')=(revoked_at IS NOT NULL)),CHECK((status='revoked')=(revoked_by IS NOT NULL)));
CREATE INDEX plugin_trust_keys_status ON plugin_trust_keys(status,key_id);
CREATE INDEX plugin_versions_signing_key ON plugin_versions(signing_key_id,plugin_id) WHERE signing_key_id IS NOT NULL;
CREATE TABLE site_maintenance_receipts(receipt_id TEXT PRIMARY KEY NOT NULL CHECK(length(receipt_id)>0),site_id TEXT NOT NULL REFERENCES sites(id) ON DELETE CASCADE,action TEXT NOT NULL CHECK(action IN ('verify-session','check-in')),window_key TEXT NOT NULL CHECK(length(window_key)>0),timezone TEXT NOT NULL CHECK(length(timezone)>0),window_start_utc INTEGER NOT NULL,window_end_utc INTEGER NOT NULL CHECK(window_end_utc>window_start_utc),plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),provider_id TEXT NOT NULL CHECK(length(provider_id)>0),generation INTEGER NOT NULL CHECK(generation>=0),credential_epoch TEXT NOT NULL CHECK(length(credential_epoch)>0),state TEXT NOT NULL CHECK(state IN ('pending','completed','unknown','failed')),outcome TEXT,evidence TEXT,revision INTEGER NOT NULL CHECK(revision>0),created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(site_id,action,window_key));
CREATE INDEX site_maintenance_receipts_site_window ON site_maintenance_receipts(site_id,action,window_end_utc DESC,receipt_id);
CREATE INDEX site_maintenance_receipts_unresolved ON site_maintenance_receipts(updated_at,receipt_id) WHERE state IN ('pending','unknown');
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
CREATE TABLE plugin_ui_trust(
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  package_sha256 TEXT NOT NULL CHECK(length(package_sha256)>0),
  bundle_sha256 TEXT NOT NULL CHECK(length(bundle_sha256)>0),
  granted_by_user_id TEXT NOT NULL CHECK(length(granted_by_user_id)>0),
  granted_at TEXT NOT NULL CHECK(length(granted_at)>0),
  revoked_at TEXT CHECK(revoked_at IS NULL OR length(revoked_at)>0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  PRIMARY KEY(plugin_id,package_sha256)
);
