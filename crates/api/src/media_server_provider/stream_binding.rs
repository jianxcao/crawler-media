//! Protocol streams consume the play session PlaybackInfo issued: the session
//! must belong to this user and device, still be open, and name exactly the
//! logical unit and physical source revision/version being streamed.

use domain::UserId;
use store::playback::PlaySessionIdState;

use super::ApiServerProvider;
use super::items::ResolvedEntry;

impl ApiServerProvider {
    pub(super) fn session_physical_source(
        &self,
        user_id: UserId,
        device_id: Option<&str>,
        session_id: &str,
    ) -> Result<Option<domain::PhysicalSourceId>, String> {
        let Some(device_id) = device_id else {
            return Ok(None);
        };
        self.bounded_reader(&[store::Db::Library], |store| {
            let binding = store
                .play_session_binding(user_id, device_id, session_id)
                .map_err(|error| error.to_string())?;
            Ok(binding.and_then(|binding| {
                if binding.state == PlaySessionIdState::Closed {
                    return None;
                }
                Some(domain::PhysicalSourceId {
                    file_id: binding.ledger_id,
                    revision: binding.file_revision?,
                })
            }))
        })
    }

    pub(super) fn stream_matches_binding(
        &self,
        user_id: UserId,
        device_id: Option<&str>,
        session_id: &str,
        entry: &ResolvedEntry,
        file: &domain::LibraryFile,
    ) -> Result<bool, String> {
        let Some(device_id) = device_id else {
            return Ok(false);
        };
        let Some(binding) = self.bounded_reader(&[store::Db::Library], |store| {
            store
                .play_session_binding(user_id, device_id, session_id)
                .map_err(|e| e.to_string())
        })?
        else {
            return Ok(false);
        };
        let file_id = file.file_id.to_string();
        Ok(binding.state != PlaySessionIdState::Closed
            && binding.media_id == entry.media.id
            && binding.unit == domain::UnitKey::new(entry.unit.0, entry.unit.1)
            && binding.file_id.as_deref() == Some(file_id.as_str())
            && binding.file_revision == Some(file.version.revision)
            && binding.source_version.as_deref() == Some(file.version.source_version.as_str()))
    }
}
