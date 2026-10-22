use crate::management::ApiState;
use domain::UserId;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

pub const USER_REMOVAL_PREFIX: &str = "user_removal.";
/// Terminal outcome of one removal, keyed by operation id; kept for [`RESULT_RETENTION_SECS`].
pub const USER_REMOVAL_RESULT_PREFIX: &str = "user_removal.result.";
/// Operation id of the finished removal of one User, so a repeated DELETE answers with it.
pub const USER_REMOVAL_DONE_PREFIX: &str = "user_removal.done.";
pub const RESULT_RETENTION_SECS: i64 = 7 * 24 * 3600;
const RECORD_VERSION: u32 = 1;

fn default_version() -> u32 {
    RECORD_VERSION
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRemovalRecord {
    #[serde(default = "default_version")]
    pub version: u32,
    pub operation_id: String,
    pub user_id: String,
    pub phase: String,
    pub cursor: Option<String>,
    pub error: Option<String>,
    pub updated_at: i64,
}

pub struct UserRemovalService {
    state: Arc<ApiState>,
}

impl UserRemovalService {
    pub fn new(state: Arc<ApiState>) -> Self {
        Self { state }
    }

    /// Accept (or resume) the removal of one User and drive it to completion.
    ///
    /// The persisted `user_removal.<id>` record is authoritative: a repeated request or a
    /// restart resumes the same operation from its stored phase instead of starting a new
    /// one. Each phase is idempotent; a failure is written back into the record (phase kept,
    /// `error` set) and returned, never swallowed.
    pub fn start_removal(&self, user_id: UserId) -> Result<String, String> {
        let record = self.accept(user_id)?;
        self.drive(user_id, record)
    }

    /// Accept a removal without driving it: the record is persisted and the `disabling`
    /// phase (disable + revoke tokens) runs right away, so the User loses access before the
    /// caller answers. The returned record is what `DELETE` reports; the remaining phases
    /// run in [`UserRemovalService::continue_removal`].
    pub fn accept(&self, user_id: UserId) -> Result<UserRemovalRecord, String> {
        let record = self.accept_record(user_id)?;
        if record.phase != "disabling" {
            return Ok(record);
        }
        let store = self.state.store.lock();
        let mut record = record;
        match run_phase(&store, user_id, "disabling") {
            Ok(Some(next)) => {
                record.phase = next.into();
                record.error = None;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%user_id, operation_id = %record.operation_id, %error, "User removal disabling failed");
                record.error = Some(error);
            }
        }
        record.updated_at = crate::job_loop::unix_now();
        write_record(&store, user_id, &record)?;
        Ok(record)
    }

    /// Drive an accepted removal to its end (or to a recorded failure).
    pub fn continue_removal(&self, user_id: UserId) -> Result<String, String> {
        let record = read_record(&self.state.store.lock(), user_id)?
            .ok_or_else(|| format!("no pending removal for user {user_id}"))?;
        self.drive(user_id, record)
    }

    fn accept_record(&self, user_id: UserId) -> Result<UserRemovalRecord, String> {
        let record = {
            let store = self.state.store.lock();
            let admin_id = store.admin_user_id().map_err(|e| e.to_string())?;
            if user_id == admin_id {
                return Err("Cannot delete admin user".into());
            }
            match read_record(&store, user_id)? {
                Some(record) => record,
                None => {
                    store
                        .get_user(user_id)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "User not found".to_string())?;
                    let record = UserRemovalRecord {
                        version: RECORD_VERSION,
                        operation_id: uuid::Uuid::new_v4().to_string(),
                        user_id: user_id.to_string(),
                        phase: "disabling".into(),
                        cursor: None,
                        error: None,
                        updated_at: crate::job_loop::unix_now(),
                    };
                    write_record(&store, user_id, &record)?;
                    record
                }
            }
        };
        Ok(record)
    }

    fn drive(&self, user_id: UserId, mut record: UserRemovalRecord) -> Result<String, String> {
        let operation_id = record.operation_id.clone();
        let store = self.state.store.lock();
        loop {
            let step = run_phase(&store, user_id, &record.phase);
            match step {
                Ok(Some(next)) => {
                    record.phase = next.into();
                    record.error = None;
                    record.updated_at = crate::job_loop::unix_now();
                    write_record(&store, user_id, &record)?;
                }
                Ok(None) => {
                    // Terminal result first: a crash before the pending key is removed
                    // re-runs the idempotent `finalizing` phase and rewrites the result.
                    record.phase = "succeeded".into();
                    record.error = None;
                    record.updated_at = crate::job_loop::unix_now();
                    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
                    store
                        .put_setting(
                            &format!("{USER_REMOVAL_RESULT_PREFIX}{operation_id}"),
                            &json,
                        )
                        .map_err(|e| e.to_string())?;
                    store
                        .put_setting(
                            &format!("{USER_REMOVAL_DONE_PREFIX}{user_id}"),
                            &operation_id,
                        )
                        .map_err(|e| e.to_string())?;
                    store
                        .delete_setting(&record_key(user_id))
                        .map_err(|e| e.to_string())?;
                    tracing::info!(%user_id, %operation_id, "User removal completed successfully");
                    return Ok(operation_id);
                }
                Err(error) => {
                    tracing::error!(%user_id, %operation_id, phase = %record.phase, %error, "User removal phase failed");
                    record.error = Some(error.clone());
                    record.updated_at = crate::job_loop::unix_now();
                    write_record(&store, user_id, &record)?;
                    return Err(error);
                }
            }
        }
    }

    /// Startup recovery: resume every persisted removal from its stored phase.
    /// Returns an error naming every removal that is still failing.
    pub fn recover_pending_removals(&self) -> Result<(), String> {
        let keys = self
            .state
            .store
            .lock()
            .list_setting_keys_with_prefix(USER_REMOVAL_PREFIX)
            .map_err(|e| e.to_string())?;
        let mut failures = Vec::new();
        prune_results(&self.state.store.lock(), crate::job_loop::unix_now());
        for key in keys {
            if key.starts_with(USER_REMOVAL_RESULT_PREFIX)
                || key.starts_with(USER_REMOVAL_DONE_PREFIX)
            {
                continue;
            }
            let user_id_str = key.trim_start_matches(USER_REMOVAL_PREFIX);
            let Ok(uid) = UserId::from_str(user_id_str) else {
                failures.push(format!("{key}: invalid user id"));
                continue;
            };
            let record = match read_record(&self.state.store.lock(), uid) {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(error) => {
                    failures.push(format!("{uid}: {error}"));
                    continue;
                }
            };
            tracing::warn!(user_id = %uid, operation_id = %record.operation_id, phase = %record.phase, "Recovering pending user removal");
            if let Err(error) = self.drive(uid, record) {
                failures.push(format!("{uid}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

fn record_key(user_id: UserId) -> String {
    format!("{USER_REMOVAL_PREFIX}{user_id}")
}

fn read_record(store: &store::Store, user_id: UserId) -> Result<Option<UserRemovalRecord>, String> {
    let Some(raw) = store
        .get_setting(&record_key(user_id))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("user removal record for {user_id} is unreadable: {e}"))
}

fn write_record(
    store: &store::Store,
    user_id: UserId,
    record: &UserRemovalRecord,
) -> Result<(), String> {
    let json = serde_json::to_string(record).map_err(|e| e.to_string())?;
    store
        .put_setting(&record_key(user_id), &json)
        .map_err(|e| e.to_string())
}

/// Run one idempotent phase; returns the next phase, or `None` when finished.
fn run_phase(
    store: &store::Store,
    user_id: UserId,
    phase: &str,
) -> Result<Option<&'static str>, String> {
    let admin_id = store.admin_user_id().map_err(|e| e.to_string())?;
    match phase {
        "disabling" => {
            // The User may already be gone if a previous run finished the last phase.
            if let Some(mut user) = store.get_user(user_id).map_err(|e| e.to_string())? {
                user.enabled = false;
                store.save_user(&user).map_err(|e| e.to_string())?;
            }
            store
                .revoke_user_tokens(user_id)
                .map_err(|e| e.to_string())?;
            Ok(Some("cleaning"))
        }
        "cleaning" => {
            store
                .transfer_user_subscribes(user_id, admin_id)
                .map_err(|e| e.to_string())?;
            store
                .clean_user_library_data(user_id, admin_id)
                .map_err(|e| e.to_string())?;
            Ok(Some("finalizing"))
        }
        "finalizing" => {
            // Idempotent: a User already deleted by an interrupted run is not an error.
            store
                .finish_user_removal(user_id)
                .map_err(|e| e.to_string())?;
            Ok(None)
        }
        other => Err(format!("unknown user removal phase {other}")),
    }
}

/// Public status of a removal, as `DELETE` and `GET /users/removals/{id}` report it.
pub fn status_dto(record: &UserRemovalRecord) -> serde_json::Value {
    let state = if record.phase == "succeeded" {
        "succeeded"
    } else if record.error.is_some() {
        "failed"
    } else {
        "pending"
    };
    serde_json::json!({
        "operation_id": record.operation_id,
        "user_id": record.user_id,
        "phase": record.phase,
        "state": state,
        "error": record.error,
    })
}

/// Find a removal by operation id: a pending record (bounded by the number of removals in
/// flight) or a retained terminal result.
pub fn find_removal(
    store: &store::Store,
    operation_id: &str,
) -> Result<Option<UserRemovalRecord>, String> {
    if let Some(raw) = store
        .get_setting(&format!("{USER_REMOVAL_RESULT_PREFIX}{operation_id}"))
        .map_err(|e| e.to_string())?
    {
        return serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| e.to_string());
    }
    let keys = store
        .list_setting_keys_with_prefix(USER_REMOVAL_PREFIX)
        .map_err(|e| e.to_string())?;
    for key in keys {
        if key.starts_with(USER_REMOVAL_RESULT_PREFIX) || key.starts_with(USER_REMOVAL_DONE_PREFIX)
        {
            continue;
        }
        let Some(raw) = store.get_setting(&key).map_err(|e| e.to_string())? else {
            continue;
        };
        if let Ok(record) = serde_json::from_str::<UserRemovalRecord>(&raw)
            && record.operation_id == operation_id
        {
            return Ok(Some(record));
        }
    }
    Ok(None)
}

/// The finished removal of a User whose row is already gone, if its result is retained.
pub fn finished_removal_of(
    store: &store::Store,
    user_id: UserId,
) -> Result<Option<UserRemovalRecord>, String> {
    let Some(operation_id) = store
        .get_setting(&format!("{USER_REMOVAL_DONE_PREFIX}{user_id}"))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    find_removal(store, &operation_id)
}

pub fn pending_removal_of(
    store: &store::Store,
    user_id: UserId,
) -> Result<Option<UserRemovalRecord>, String> {
    read_record(store, user_id)
}

/// Drop terminal results older than the retention window; pending records are never touched.
fn prune_results(store: &store::Store, now: i64) {
    let Ok(keys) = store.list_setting_keys_with_prefix(USER_REMOVAL_RESULT_PREFIX) else {
        return;
    };
    for key in keys {
        let Ok(Some(raw)) = store.get_setting(&key) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<UserRemovalRecord>(&raw) else {
            continue;
        };
        if record.phase == "succeeded" && now - record.updated_at > RESULT_RETENTION_SECS {
            if let Err(error) = store.delete_setting(&key) {
                tracing::warn!(%key, %error, "expired user removal result could not be pruned");
                continue;
            }
            let _ = store.delete_setting(&format!("{USER_REMOVAL_DONE_PREFIX}{}", record.user_id));
        }
    }
}
