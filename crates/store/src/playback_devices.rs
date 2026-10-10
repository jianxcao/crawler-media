use domain::UserId;

use super::{Store, StoreError};

impl Store {
    fn revoked_list(&self) -> Result<Vec<String>, StoreError> {
        Ok(self
            .get_setting("playback.revoked_devices")?
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default())
    }

    fn revoked_device_key(user_id: UserId, device_id: &str) -> String {
        format!("{user_id}:{device_id}")
    }

    pub fn is_device_revoked(&self, user_id: UserId, device_id: &str) -> Result<bool, StoreError> {
        let key = Self::revoked_device_key(user_id, device_id);
        Ok(self.revoked_list()?.iter().any(|id| id == &key))
    }

    pub fn revoke_device(&self, user_id: UserId, device_id: &str) -> Result<(), StoreError> {
        let mut list = self.revoked_list()?;
        let key = Self::revoked_device_key(user_id, device_id);
        if !list.iter().any(|id| id == &key) {
            list.push(key);
        }
        self.put_setting("playback.revoked_devices", &serde_json::to_string(&list)?)
    }

    /// Only actual Jellyfin identities have a device-level credential contract.
    /// Web activity and legacy logs without an identity support ending only.
    pub fn device_is_revocable(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<bool, StoreError> {
        if device_id.is_empty()
            || device_id == "jellyfin-client"
            || device_id.starts_with("unidentified:")
        {
            return Ok(false);
        }
        Ok(self.subscribe.query_row(
            "SELECT EXISTS(SELECT 1 FROM playback_sessions WHERE user_id=?1 AND device_id=?2 AND play_method='DirectPlay')
             OR EXISTS(SELECT 1 FROM playback_logs WHERE user_id=?1 AND device_id=?2 AND play_method='DirectPlay')",
            rusqlite::params![user_id.to_string(),device_id], |row| row.get(0),
        )?)
    }
}
