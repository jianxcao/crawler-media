use crate::management::ApiState;
use domain::UserId;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

pub const USER_REMOVAL_PREFIX: &str = "user_removal.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserRemovalRecord {
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
        self.drive(user_id, record)
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
        for key in keys {
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

fn read_record(
    store: &store::Store,
    user_id: UserId,
) -> Result<Option<UserRemovalRecord>, String> {
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
            store.revoke_user_tokens(user_id).map_err(|e| e.to_string())?;
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
            store.finish_user_removal(user_id).map_err(|e| e.to_string())?;
            Ok(None)
        }
        other => Err(format!("unknown user removal phase {other}")),
    }
}
