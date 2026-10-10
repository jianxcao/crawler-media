use domain::UserId;
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};
use crate::users::SETTING_CLI_TOKEN;

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

    /// Record an observed authenticated protocol credential, never a CLI token.
    /// One ordinary token may be shared by multiple devices: revoking it necessarily
    /// invalidates all of those devices, but never independently issued credentials.
    pub fn bind_playback_credential(
        &self,
        user_id: UserId,
        device_id: &str,
        token: &str,
    ) -> Result<(), StoreError> {
        if !valid_device_identity(device_id) {
            return Ok(());
        }
        self.app.execute(
            "INSERT OR IGNORE INTO playback_device_credentials (user_id, device_id, token)
             SELECT ?1, ?2, token FROM user_tokens
             WHERE user_id=?1 AND token=?3
             AND token <> COALESCE((SELECT value FROM settings WHERE key=?4), '')",
            params![user_id.to_string(), device_id, token, SETTING_CLI_TOKEN],
        )?;
        Ok(())
    }

    pub fn revoke_device(&self, user_id: UserId, device_id: &str) -> Result<(), StoreError> {
        let tx = self.app.unchecked_transaction()?;
        let tokens = {
            let mut stmt = tx.prepare(
                "SELECT c.token FROM playback_device_credentials c JOIN user_tokens t
                 ON t.token=c.token AND t.user_id=c.user_id
                 WHERE c.user_id=?1 AND c.device_id=?2
                 AND c.token <> COALESCE((SELECT value FROM settings WHERE key=?3), '')",
            )?;
            stmt.query_map(
                params![user_id.to_string(), device_id, SETTING_CLI_TOKEN],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        if tokens.is_empty() || !valid_device_identity(device_id) {
            return Err(StoreError::Protected(
                "设备没有可撤销的已绑定会话凭据".into(),
            ));
        }
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key='playback.revoked_devices'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let mut list: Vec<String> = raw
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let key = Self::revoked_device_key(user_id, device_id);
        if !list.contains(&key) {
            list.push(key);
        }
        for token in &tokens {
            tx.execute(
                "DELETE FROM user_tokens WHERE user_id=?1 AND token=?2",
                params![user_id.to_string(), token],
            )?;
            tx.execute(
                "DELETE FROM playback_device_credentials WHERE user_id=?1 AND token=?2",
                params![user_id.to_string(), token],
            )?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO settings (key,value) VALUES ('playback.revoked_devices',?1)",
            params![serde_json::to_string(&list)?],
        )?;
        tx.commit()?;
        tracing::info!(%user_id, device_id, credentials = tokens.len(), "Playback device credentials revoked");
        Ok(())
    }

    /// Old/unbound and CLI-only identities support ending playback, not revocation.
    pub fn device_is_revocable(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<bool, StoreError> {
        if !valid_device_identity(device_id) {
            return Ok(false);
        }
        Ok(self.app.query_row(
            "SELECT EXISTS(SELECT 1 FROM playback_device_credentials c JOIN user_tokens t
             ON t.token=c.token AND t.user_id=c.user_id
             WHERE c.user_id=?1 AND c.device_id=?2
             AND c.token <> COALESCE((SELECT value FROM settings WHERE key=?3), ''))",
            params![user_id.to_string(), device_id, SETTING_CLI_TOKEN],
            |r| r.get(0),
        )?)
    }
}

fn valid_device_identity(device_id: &str) -> bool {
    !device_id.trim().is_empty()
        && device_id != "jellyfin-client"
        && !device_id.starts_with("unidentified:")
}
