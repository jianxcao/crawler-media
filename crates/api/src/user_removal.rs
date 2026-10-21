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

    /// Public entry point for initiating or driving user removal.
    /// Performs operations in order:
    /// 1. Persist operation record in app.settings: `user_removal.<id>`
    /// 2. Disable user and revoke all user tokens (immediately 401)
    /// 3. Cross-database cleanup:
    ///    - Transfer subscribes to admin
    ///    - Transfer private collections to admin
    ///    - Delete playback facts and user_library_items
    /// 4. Delete user record and update/delete operation record
    pub fn start_removal(&self, user_id: UserId) -> Result<String, String> {
        let store = self.state.store.lock();
        let admin_id = store.admin_user_id().map_err(|e| e.to_string())?;
        if user_id == admin_id {
            return Err("Cannot delete admin user".into());
        }

        let user = store
            .get_user(user_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "User not found".to_string())?;

        let op_key = format!("{}{}", USER_REMOVAL_PREFIX, user_id);
        let now = crate::job_loop::unix_now();
        let operation_id = uuid::Uuid::new_v4().to_string();

        let mut record = UserRemovalRecord {
            operation_id: operation_id.clone(),
            user_id: user_id.to_string(),
            phase: "disabling".into(),
            cursor: None,
            error: None,
            updated_at: now,
        };

        // Step 1: Persist initial operation state
        let record_json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        store
            .put_setting(&op_key, &record_json)
            .map_err(|e| e.to_string())?;

        // Step 2: Disable user and revoke all tokens
        let mut disabled_user = user.clone();
        disabled_user.enabled = false;
        store.save_user(&disabled_user).map_err(|e| e.to_string())?;
        store
            .revoke_user_tokens(user_id)
            .map_err(|e| e.to_string())?;

        record.phase = "cleaning".into();
        record.updated_at = crate::job_loop::unix_now();
        let record_json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        store
            .put_setting(&op_key, &record_json)
            .map_err(|e| e.to_string())?;

        // Step 3: Cross-database cleanup
        // a. Transfer subscribes to admin
        store
            .transfer_user_subscribes(user_id, admin_id)
            .map_err(|e| e.to_string())?;

        // b. Transfer collections to admin & clear playback and user_library_items
        store
            .clean_user_library_data(user_id, admin_id)
            .map_err(|e| e.to_string())?;

        // Step 4: Finalize - delete user row and remove operation record
        store
            .finish_user_removal(user_id)
            .map_err(|e| e.to_string())?;
        let _ = store.delete_setting(&op_key);

        tracing::info!(user_id = %user_id, operation_id = %operation_id, "User removal completed successfully");
        Ok(operation_id)
    }

    /// Startup recovery entry point: scans app.settings for `user_removal.<id>` records
    /// and drives unfinished removals to completion.
    pub fn recover_pending_removals(&self) -> Result<(), String> {
        let store = self.state.store.lock();
        let keys = store
            .list_setting_keys_with_prefix(USER_REMOVAL_PREFIX)
            .map_err(|e| e.to_string())?;
        drop(store);

        for key in keys {
            let user_id_str = key.trim_start_matches(USER_REMOVAL_PREFIX);
            if let Ok(uid) = UserId::from_str(user_id_str) {
                tracing::warn!(user_id = %uid, "Recovering pending user removal");
                let _ = self.start_removal(uid);
            }
        }
        Ok(())
    }
}
