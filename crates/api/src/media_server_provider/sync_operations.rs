//! Synchronous protocol stages run in the bounded HTTP blocking budget.
use super::*;
impl ApiServerProvider {
    pub(super) fn bounded_store<T>(
        &self,
        work: impl FnOnce(&crate::Store) -> Result<T, String>,
    ) -> Result<T, String> {
        crate::http::store_wait::with_foreground_store(&self.state, |store| {
            read_store(&self.state, |_| work(store))
        })
        .map_err(admission_error)?
    }
    pub(super) fn bounded_reader<T>(
        &self,
        work: impl FnOnce(&crate::Store) -> Result<T, String>,
    ) -> Result<T, String> {
        read_store(&self.state, work)
    }
    pub(super) async fn run_sync<T: Send + 'static>(
        &self,
        work: impl FnOnce(ApiServerProvider) -> T + Send + 'static,
    ) -> Result<T, String> {
        let provider = Self::new(self.state.clone());
        self.state
            .http_work
            .run_to_completion(move || work(provider))
            .await
            .map_err(|error| {
                tracing::error!(%error, "Jellyfin blocking stage failed");
                error.to_string()
            })
    }
    pub(super) fn get_user_sync(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        self.bounded_reader(|store| {
            let Some(user) = store.get_user(user_id).map_err(|e| e.to_string())? else {
                return Ok(None);
            };
            let role = store.user_role(user.id).unwrap_or_else(|_| "member".into());
            Ok(Some(ServerUser {
                id: user.id,
                login: user.login,
                is_admin: role == "admin",
            }))
        })
    }
    pub(super) fn resolve_cover_bytes_sync(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let library = self.bounded_reader(|store| {
            Ok(store
                .get_library(id)
                .map_err(|error| error.to_string())?
                .filter(|lib| crate::http::library::library_visible(store, lib, user_id)))
        })?;
        if let Some(lib) = library {
            let cover = lib
                .cover_path
                .as_deref()
                .filter(|path| !path.is_empty() && *path != "none")
                .map(PathBuf::from)
                .filter(|path| path.is_file());
            return match cover {
                Some(cover) => std::fs::read(cover)
                    .map(Some)
                    .map_err(|error| error.to_string()),
                None => default_library_cover(lib.kind).map(Some),
            };
        }
        Ok(None)
    }
    pub(super) fn resolve_poster_bytes_sync(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let entry = self.bounded_reader(|store| items::resolve_entry(store, user_id, id))?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let path = if entry.is_series {
            crate::http::library::poster_path(&entry.file)
        } else {
            local_primary_image(&entry.file, entry.unit, &entry.media)
                .or_else(|| crate::http::library::poster_path(&entry.file))
        };
        match path {
            Some(path) => std::fs::read(path).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
    pub(super) fn resolve_backdrop_bytes_sync(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let entry = self.bounded_reader(|store| items::resolve_entry(store, user_id, id))?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let Some(path) = backdrop_path(&entry.file, entry.is_series) else {
            return Ok(None);
        };
        std::fs::read(path)
            .map(Some)
            .map_err(|error| error.to_string())
    }
    pub(super) fn resolve_stream_source_bound_sync(
        &self,
        user_id: UserId,
        id: &str,
        media_source_id: Option<&str>,
        play_session_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        // Must not accept bare file UUID as a logical ID
        if id.len() == 36
            && id.chars().filter(|c| *c == '-').count() == 4
            && uuid::Uuid::parse_str(id).is_ok()
        {
            return Ok(None);
        }
        let entry = self.bounded_reader(|store| items::resolve_entry(store, Some(user_id), id))?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        let file = if let Some(msid) = media_source_id {
            // Only the versioned `f1_<file>_<revision>` shape advertised by
            // PlaybackInfo is accepted; a bare file UUID is not a MediaSourceId.
            let Some(physical) = domain::PhysicalSourceId::from_wire_string(msid) else {
                return Ok(None);
            };
            let Some(file) = self.bounded_reader(|store| {
                let Some(file) = store
                    .visible_file(user_id, physical.file_id)
                    .map_err(|e| e.to_string())?
                else {
                    return Ok(None);
                };
                let covers = store
                    .file_covers_unit(
                        physical.file_id,
                        entry.media.id,
                        domain::UnitKey::new(entry.unit.0, entry.unit.1),
                    )
                    .map_err(|e| e.to_string())?;
                if !covers
                    || file.media_id != entry.media.id
                    || file.version.revision != physical.revision
                {
                    return Ok(None);
                }
                Ok(Some(file))
            })?
            else {
                return Ok(None);
            };
            file
        } else {
            entry.file.clone()
        };
        if let Some(session_id) = play_session_id
            && !self.stream_matches_binding(user_id, device_id, session_id, &entry, &file)?
        {
            tracing::warn!(%user_id, item_id = %id, "Jellyfin 取流与签发的播放会话绑定不符，拒绝");
            return Ok(None);
        }

        let path = file.path.clone();
        let is_strm = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"));
        Ok(Some((path, is_strm)))
    }
    pub(super) fn is_device_revoked_sync(
        &self,
        user_id: UserId,
        device_id: &str,
    ) -> Result<bool, String> {
        self.bounded_reader(|store| {
            store
                .is_device_revoked(user_id, device_id)
                .map_err(|e| e.to_string())
        })
    }
    pub(super) fn set_user_marks_sync(
        &self,
        user_id: UserId,
        id: &str,
        played: Option<bool>,
        favorite: Option<bool>,
    ) -> Result<bool, UserMarksError> {
        if !self.state.instance_writable() {
            return Err(UserMarksError::Internal(
                "instance is read-only or under maintenance".into(),
            ));
        }

        crate::http::store_wait::with_foreground_store(&self.state, |store| {
            store
                .with_read_scope(crate::http::store_wait::STORE_READ_WAIT, || {
                    let Some((media_id, season, episode)) =
                        resolve_mark_unit(store, user_id, id).map_err(UserMarksError::Internal)?
                    else {
                        return Ok(false);
                    };
                    let change = crate::store::playback::PlaybackMutation::marks(
                        user_id, season, episode, played, favorite,
                    );
                    store
                        .mutate_playback(media_id, &change, self.state.playback_clock.now_ms())
                        .map_err(|error| user_marks::mark_error(error, user_id, media_id))?;
                    Ok(true)
                })
                .map_err(|error| match error {
                    store::StoreError::ReadBusy
                    | store::StoreError::ReadClosed
                    | store::StoreError::ReadDeadline
                    | store::StoreError::ReadCancelled => UserMarksError::Unavailable,
                    error => UserMarksError::Internal(error.to_string()),
                })?
        })
        .map_err(|response| {
            if response.status() == axum::http::StatusCode::SERVICE_UNAVAILABLE {
                UserMarksError::Unavailable
            } else {
                UserMarksError::Internal(admission_error(response))
            }
        })?
    }
    pub(super) fn issue_play_session_sync(
        &self,
        user_id: UserId,
        device_id: &str,
        id: &str,
    ) -> Result<String, String> {
        if !self.state.instance_writable() {
            return Err("instance is read-only or under maintenance".into());
        }

        let target = playback::resolve_target(self, user_id, id)?
            .ok_or_else(|| format!("Playback item {id} missing or not visible"))?;
        use std::str::FromStr;
        let ledger_id = domain::LedgerId::from_str(&target.ledger_id).map_err(|e| e.to_string())?;
        // 播放域时间走注入时钟；`issue_play_session` 的 `issued_at` 是**秒**。
        let now = self.state.playback_clock.now_ms() / 1000;
        self.bounded_store(|store| {
            store
                .issue_play_session(
                    user_id,
                    device_id,
                    ledger_id,
                    target.media.id,
                    domain::UnitKey::new(target.season, target.episode),
                    &target.file.version,
                    now,
                )
                .map_err(|e| e.to_string())
        })
    }
    pub(super) fn report_playback_event_sync(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: i64,
        paused: bool,
        client: PlaybackClientInfo,
        event: PlaybackEvent,
        identity: media_server::provider::PlaybackEventIdentity,
    ) -> Result<(), String> {
        if !self.state.instance_writable() {
            return Err("instance is read-only or under maintenance".into());
        }

        playback::report_event(
            self,
            user_id,
            id,
            position_ms,
            paused,
            client,
            event,
            identity,
        )
    }
}

pub(super) fn read_store<T>(
    state: &ApiState,
    work: impl FnOnce(&crate::Store) -> Result<T, String>,
) -> Result<T, String> {
    state
        .store
        .with_read_scope(crate::http::store_wait::STORE_READ_WAIT, || {
            work(&state.store)
        })
        .map_err(|error| error.to_string())?
}

fn admission_error(response: axum::response::Response) -> String {
    match response.status() {
        axum::http::StatusCode::CONFLICT => "instance is read-only or under maintenance".into(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE => {
            "Store writer is busy or under maintenance".into()
        }
        status => format!("Store write admission failed: {status}"),
    }
}
