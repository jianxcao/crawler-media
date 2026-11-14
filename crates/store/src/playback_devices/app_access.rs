use crate::{Store, StoreError};
use domain::UserId;

impl Store {
    pub fn is_device_revoked(&self, user_id: UserId, device_id: &str) -> Result<bool, StoreError> {
        self.with_app_read(|app| app.is_device_revoked(user_id, device_id))
    }

    /// Whether this authenticated device has a new ordinary credential to record.
    /// This read also keeps CLI and already observed requests off the writer.
    pub fn playback_credential_needs_binding(
        &self,
        user_id: UserId,
        device_id: &str,
        token: &str,
    ) -> Result<bool, StoreError> {
        self.with_app_read(|app| app.playback_credential_needs_binding(user_id, device_id, token))
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
        self.with_app(|app| app.bind_playback_credential(user_id, device_id, token))
    }

    pub fn revoke_device(&self, user_id: UserId, device_id: &str) -> Result<(), StoreError> {
        self.with_app(|app| app.revoke_device(user_id, device_id))
    }

    /// Old/unbound and CLI-only identities support ending playback, not revocation.
    pub fn device_is_revocable(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_app_read(|app| app.device_is_revocable(user_id, device_id))
    }
}
