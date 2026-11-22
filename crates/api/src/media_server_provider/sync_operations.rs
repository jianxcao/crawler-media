//! Synchronous protocol stages run in the bounded HTTP blocking budget.
use super::*;
impl ApiServerProvider {
    pub(super) fn bounded_write_store<T>(
        &self,
        work: impl FnOnce(&crate::Store) -> Result<T, String>,
    ) -> Result<T, String> {
        crate::http::store_wait::with_foreground_store(&self.state, work)
            .map_err(admission_error)?
    }
    pub(super) fn bounded_reader<T>(
        &self,
        databases: &[store::Db],
        work: impl FnOnce(&crate::Store) -> Result<T, String>,
    ) -> Result<T, String> {
        read_store(&self.state, databases, work)
    }
    pub(super) fn bounded_reader_query<T>(
        &self,
        databases: &[store::Db],
        work: impl FnOnce(&crate::Store) -> Result<T, media_server::provider::QueryError>,
    ) -> Result<T, media_server::provider::QueryError> {
        read_store_query(&self.state, databases, work)
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
    pub(super) async fn run_sync_query<T: Send + 'static>(
        &self,
        work: impl FnOnce(ApiServerProvider) -> T + Send + 'static,
    ) -> Result<T, media_server::provider::QueryError> {
        let provider = Self::new(self.state.clone());
        self.state
            .http_work
            .run_to_completion(move || work(provider))
            .await
            .map_err(|error| {
                tracing::error!(%error, "Jellyfin blocking stage failed");
                match error {
                    crate::blocking_budget::BlockingError::Busy
                    | crate::blocking_budget::BlockingError::Timeout => {
                        media_server::provider::QueryError::Unavailable
                    }
                    crate::blocking_budget::BlockingError::Worker(msg) => {
                        media_server::provider::QueryError::Internal(msg)
                    }
                }
            })
    }
    pub(super) fn get_user_sync(&self, user_id: UserId) -> Result<Option<ServerUser>, String> {
        self.bounded_reader(&[store::Db::App], |store| {
            let Some(user) = store.get_user(user_id).map_err(|e| e.to_string())? else {
                return Ok(None);
            };
            if !user.enabled
                || store
                    .get_setting(&format!("user_removal.{user_id}"))
                    .map_err(|e| e.to_string())?
                    .is_some()
            {
                return Ok(None);
            }
            Ok(Some(ServerUser {
                id: user.id,
                login: user.login,
                is_admin: user.role == domain::UserRole::Admin,
            }))
        })
    }
    pub(super) fn resolve_cover_bytes_sync(
        &self,
        user_id: Option<UserId>,
        id: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let library = self.bounded_reader(item_databases(user_id), |store| {
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
        let entry = self.bounded_reader(item_databases(user_id), |store| {
            items::resolve_entry(store, user_id, id)
        })?;
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
        let entry = self.bounded_reader(item_databases(user_id), |store| {
            items::resolve_entry(store, user_id, id)
        })?;
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
        let explicit_source = match media_source_id {
            Some(media_source_id) => {
                let Some(source) = domain::PhysicalSourceId::from_wire_string(media_source_id)
                else {
                    return Ok(None);
                };
                Some(source)
            }
            None => None,
        };
        let bound_source = if explicit_source.is_none() {
            if let Some(session_id) = play_session_id {
                let Some(source) = self.session_physical_source(user_id, device_id, session_id)?
                else {
                    return Ok(None);
                };
                Some(source)
            } else {
                None
            }
        } else {
            None
        };
        let preferred_source = explicit_source.as_ref().or(bound_source.as_ref());
        let entry = self.bounded_reader(&[store::Db::Library, store::Db::App], |store| {
            items::resolve_entry_with_source(store, Some(user_id), id, preferred_source)
        })?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        // Resolve an explicit source through the requested typed item's playable entries.
        // For Season IDs this makes the source's own covered Episode the logical target,
        // rather than reusing whichever Episode is currently preferred for the Season.
        let file = entry.file.clone();
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

    pub(super) fn resolve_playback_file_sync(
        &self,
        user_id: UserId,
        file_id: &str,
    ) -> Result<Option<(PathBuf, bool)>, String> {
        let Ok(ledger_id) = file_id.parse::<domain::LedgerId>() else {
            return Ok(None);
        };
        let file = self.bounded_reader(&[store::Db::Library, store::Db::App], |store| {
            let Some(file) = store
                .visible_file(user_id, ledger_id)
                .map_err(|error| error.to_string())?
            else {
                return Ok(None);
            };
            let accepts = store
                .library_accepts_work(&file.library_id.to_string())
                .map_err(|error| error.to_string())?;
            Ok(accepts.then_some(file))
        })?;
        let Some(file) = file else {
            return Ok(None);
        };
        let path = file.path;
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
        self.bounded_reader(&[store::Db::App], |store| {
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
            return Err(UserMarksError::Conflict(
                "instance is read-only or under maintenance".into(),
            ));
        }

        let Some((media_id, season, episode)) = self
            .state
            .store
            .with_read_snapshot(
                &[store::Db::Library, store::Db::App],
                crate::http::store_wait::STORE_READ_WAIT,
                || resolve_mark_unit(&self.state.store, user_id, id),
            )
            .map_err(user_marks::snapshot_error)?
            .map_err(user_marks::snapshot_error)?
        else {
            return Ok(false);
        };

        let change = crate::store::playback::PlaybackMutation::marks(
            user_id, season, episode, played, favorite,
        );
        let outcome = crate::http::store_wait::with_foreground_store(&self.state, |store| {
            store.mutate_playback(media_id, &change, self.state.playback_clock.now_ms())
        })
        .map_err(|response| match response.status() {
            axum::http::StatusCode::SERVICE_UNAVAILABLE => UserMarksError::Unavailable,
            axum::http::StatusCode::CONFLICT => {
                UserMarksError::Conflict("instance is read-only or under maintenance".into())
            }
            _ => UserMarksError::Internal(admission_error(response)),
        })?;
        match outcome {
            Ok(()) => Ok(true),
            Err(error) => user_marks::mark_error(error, user_id, media_id),
        }
    }
    pub(super) fn issue_play_session_sync(
        &self,
        user_id: UserId,
        device_id: &str,
        snapshot: &MediaItemSnapshot,
    ) -> Result<String, String> {
        if !self.state.instance_writable() {
            return Err("instance is read-only or under maintenance".into());
        }

        // 播放域时间走注入时钟；`issue_play_session` 的 `issued_at` 是**秒**。
        let now = self.state.playback_clock.now_ms() / 1000;
        self.bounded_write_store(|store| {
            store
                .issue_play_session(
                    user_id,
                    device_id,
                    snapshot.file.file_id,
                    snapshot.media.id,
                    snapshot.unit.unwrap_or(domain::UnitKey::WHOLE),
                    &snapshot.file.version,
                    now,
                )
                .map_err(|e| e.to_string())
        })
    }
    pub(super) fn report_playback_event_sync(
        &self,
        user_id: UserId,
        id: &str,
        position_ms: Option<i64>,
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

pub(super) fn store_query_error(error: store::StoreError) -> media_server::provider::QueryError {
    if crate::library_read_work::store_error_is_unavailable(&error) {
        media_server::provider::QueryError::Unavailable
    } else {
        media_server::provider::QueryError::Internal(error.to_string())
    }
}

pub(super) fn read_store_query<T>(
    state: &ApiState,
    databases: &[store::Db],
    work: impl FnOnce(&crate::Store) -> Result<T, media_server::provider::QueryError>,
) -> Result<T, media_server::provider::QueryError> {
    state
        .store
        .with_read_snapshot(databases, crate::http::store_wait::STORE_READ_WAIT, || {
            work(&state.store)
        })
        .map_err(store_query_error)?
}

pub(super) fn read_store<T>(
    state: &ApiState,
    databases: &[store::Db],
    work: impl FnOnce(&crate::Store) -> Result<T, String>,
) -> Result<T, String> {
    state
        .store
        .with_read_snapshot(databases, crate::http::store_wait::STORE_READ_WAIT, || {
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

/// Public artwork needs only published Library facts; authenticated reads also
/// consult App roles before applying Library visibility.
pub(super) fn item_databases(user: Option<UserId>) -> &'static [store::Db] {
    if user.is_some() {
        &[store::Db::Library, store::Db::App]
    } else {
        &[store::Db::Library]
    }
}

#[cfg(test)]
mod tests {
    use super::store_query_error;
    use media_server::provider::QueryError;
    use store::StoreError;

    #[test]
    fn store_query_error_maps_unavailable_variants_to_unavailable() {
        for err in [
            StoreError::QueryUnavailable {
                library_id: domain::LibraryId::new(),
            },
            StoreError::ReadBusy,
            StoreError::ReadClosed,
            StoreError::ReadDeadline,
            StoreError::ReadCancelled,
            StoreError::WriteBusy,
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )),
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
                None,
            )),
        ] {
            assert!(
                matches!(store_query_error(err), QueryError::Unavailable),
                "expected QueryError::Unavailable"
            );
        }
        assert!(matches!(
            store_query_error(StoreError::Invalid("bad input".into())),
            QueryError::Internal(_)
        ));
    }
}
