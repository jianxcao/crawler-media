use crate::bindings::{
    crawler::plugin::{catalog, download, history, library, notify, subscribe, types},
    task_world::crawler::plugin::indexer_cache,
};
use crate::ports::{State, api_error};
use plugin_api::GuestRole;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityView {
    pub handle: String,
    pub revision: u64,
    pub observed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityPage {
    pub items: Vec<EntityView>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaReference {
    pub kind: String,
    pub aliases: Vec<String>,
    pub year: Option<u32>,
    pub external_id: Option<String>,
}

impl From<EntityView> for types::EntityView {
    fn from(value: EntityView) -> Self {
        Self {
            handle: value.handle,
            revision: value.revision,
            observed_at: value.observed_at,
        }
    }
}

impl From<EntityPage> for types::EntityPage {
    fn from(value: EntityPage) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<MediaReference> for types::MediaReference {
    fn from(value: MediaReference) -> Self {
        Self {
            kind: value.kind,
            aliases: value.aliases,
            year: value.year,
            external_id: value.external_id,
        }
    }
}

impl State {
    fn check_non_lifecycle(&self, port_name: &str) -> Result<(), types::ApiError> {
        if self.invocation.role == GuestRole::Lifecycle {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                &format!("{port_name} port is forbidden during lifecycle validation"),
            ));
        }
        Ok(())
    }
}

impl subscribe::Host for State {
    fn list(&mut self, page: types::PageRequest) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("subscribe")?;
        self.check("read:subscribes")?;
        let out = self
            .ports
            .subscribe_list(&self.invocation, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }

    fn get(&mut self, handle: String) -> Result<types::EntityView, types::ApiError> {
        self.check_non_lifecycle("subscribe")?;
        self.check("read:subscribes")?;
        let out = self
            .ports
            .subscribe_get(&self.invocation, &handle)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

impl library::Host for State {
    fn list(&mut self, page: types::PageRequest) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("library")?;
        self.check("read:library")?;
        let out = self
            .ports
            .library_list(&self.invocation, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }

    fn media(
        &mut self,
        handle: String,
        page: types::PageRequest,
    ) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("library")?;
        self.check("read:library")?;
        let out = self
            .ports
            .library_media(&self.invocation, &handle, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }

    fn files(
        &mut self,
        handle: String,
        page: types::PageRequest,
    ) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("library")?;
        self.check("read:library")?;
        let out = self
            .ports
            .library_files(&self.invocation, &handle, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

impl download::Host for State {
    fn list(
        &mut self,
        downloader: String,
        page: types::PageRequest,
    ) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("download")?;
        self.check("read:downloads")?;
        let out = self
            .ports
            .download_list(&self.invocation, &downloader, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }

    fn get(&mut self, handle: String) -> Result<types::EntityView, types::ApiError> {
        self.check_non_lifecycle("download")?;
        self.check("read:downloads")?;
        let out = self
            .ports
            .download_get(&self.invocation, &handle)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

impl history::Host for State {
    fn query(
        &mut self,
        kind: String,
        handles: Vec<String>,
        from: u64,
        until: u64,
        page: types::PageRequest,
    ) -> Result<types::EntityPage, types::ApiError> {
        self.check_non_lifecycle("history")?;
        self.check("read:history")?;
        let out = self
            .ports
            .history_query(&self.invocation, &kind, &handles, from, until, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

impl catalog::Host for State {
    fn lookup(&mut self, identity: String) -> Result<types::MediaReference, types::ApiError> {
        self.check_non_lifecycle("catalog")?;
        self.check("read:catalog")?;
        let out = self
            .ports
            .catalog_lookup(&self.invocation, &identity)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}

impl notify::Host for State {
    fn emit_event(
        &mut self,
        event_id: String,
        channel_ids: Vec<String>,
    ) -> Result<String, types::ApiError> {
        self.check_alive()?;
        if self.invocation.role != GuestRole::Notify {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "notify.emit-event is restricted to notify hook contexts",
            ));
        }
        self.check("notify:send")?;
        let out = self
            .ports
            .notify_emit_event(&self.invocation, &event_id, &channel_ids)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out)
    }
}

impl indexer_cache::Host for State {
    fn cached(
        &mut self,
        operation_id: String,
        page: types::PageRequest,
    ) -> Result<types::TorrentBatch, types::ApiError> {
        self.check_alive()?;
        if !matches!(self.invocation.role, GuestRole::Task | GuestRole::Endpoint) {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "indexer-cache.cached is restricted to task and endpoint contexts",
            ));
        }
        if !self.ports.authorized(&self.invocation, "read:sites")
            && !self
                .ports
                .authorized(&self.invocation, "command:indexer.search")
            && !self
                .ports
                .authorized(&self.invocation, "command:indexer.rss")
        {
            return Err(api_error(
                types::ErrorCode::PermissionDenied,
                "permission denied",
            ));
        }
        let out = self
            .ports
            .indexer_cache_cached(&self.invocation, &operation_id, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}
