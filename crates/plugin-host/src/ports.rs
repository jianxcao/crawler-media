use crate::browser_ports::{BrowserSessionPort, ManagedBrowserSession};
use crate::limits::MemoryBudget;
use crate::phase1_ports::{EntityPage, EntityView, MediaReference};
use crate::{
    Invocation,
    bindings::{
        crawler::plugin::{blob, boost, clock, config, kv, log, operations, runtime, types},
        task_world::crawler::plugin::commands,
    },
};
use plugin_api::{
    ApiError, BoostSnapshotView, BrowserPurpose, CommandRequest, ConfigView, EntryPage, GuestRole,
    HttpRequest, HttpResponse, LogLevel, OperationPage, OperationView, PageRequest, SitePage,
    SiteView, StateEntry, StatsView, TorrentBatch,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Implementations must honor the invocation deadline. No network services are linked in P0a.
pub trait HostPorts: Send + Sync + 'static {
    fn begin_invocation(
        &self,
        _inv: &Invocation,
    ) -> Result<Option<Box<dyn std::any::Any + Send + Sync>>, ApiError> {
        Ok(None)
    }
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64)
    }
    fn authorized(&self, _: &Invocation, _: &str) -> bool {
        false
    }
    fn config_get(&self, _: &Invocation) -> Result<ConfigView, ApiError> {
        Err(unsupported("configuration port unavailable"))
    }
    fn log_write(
        &self,
        _: &Invocation,
        _: LogLevel,
        _: &str,
        _: &[(String, String)],
    ) -> Result<(), ApiError> {
        Err(unsupported("audit port unavailable"))
    }
    fn kv_get(&self, _: &Invocation, _: &str) -> Result<Option<KvValue>, ApiError> {
        Err(unsupported("KV port unavailable"))
    }
    fn kv_list(&self, _: &Invocation, _: &str, _: PageRequest) -> Result<EntryPage, ApiError> {
        Err(unsupported("KV list unavailable"))
    }
    fn kv_put(
        &self,
        _: &Invocation,
        _: &str,
        _: &[u8],
        _: Option<&str>,
    ) -> Result<String, ApiError> {
        Err(unsupported("KV writes unavailable"))
    }
    fn kv_delete(&self, _: &Invocation, _: &str, _: Option<&str>) -> Result<(), ApiError> {
        Err(unsupported("KV writes unavailable"))
    }
    fn blob_get(&self, _: &Invocation, _: &str) -> Result<StateEntry, ApiError> {
        Err(unsupported("Blob port unavailable"))
    }
    fn blob_list(&self, _: &Invocation, _: &str, _: PageRequest) -> Result<EntryPage, ApiError> {
        Err(unsupported("Blob list unavailable"))
    }
    fn blob_put(
        &self,
        _: &Invocation,
        _: &str,
        _: &[u8],
        _: Option<&str>,
    ) -> Result<String, ApiError> {
        Err(unsupported("Blob writes unavailable"))
    }
    fn blob_delete(&self, _: &Invocation, _: &str, _: Option<&str>) -> Result<(), ApiError> {
        Err(unsupported("Blob writes unavailable"))
    }
    fn operations_get(&self, _: &Invocation, _: &str) -> Result<OperationView, ApiError> {
        Err(unsupported("Operations port unavailable"))
    }
    fn operations_list(&self, _: &Invocation, _: PageRequest) -> Result<OperationPage, ApiError> {
        Err(unsupported("Operations port unavailable"))
    }
    fn commands_submit(&self, _: &Invocation, _: CommandRequest) -> Result<String, ApiError> {
        Err(unsupported("Commands port unavailable"))
    }
    fn http_fetch(&self, _: &Invocation, _: HttpRequest) -> Result<HttpResponse, ApiError> {
        Err(unsupported("HTTP port unavailable"))
    }
    fn http_fetch_many(
        &self,
        inv: &Invocation,
        reqs: Vec<HttpRequest>,
    ) -> Vec<Result<HttpResponse, ApiError>> {
        std::thread::scope(|s| {
            let handles: Vec<_> = reqs
                .into_iter()
                .map(|req| {
                    s.spawn(move || {
                        let _guest = plugin_api::GuestExecutionGuard::enter();
                        self.http_fetch(inv, req)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(ApiError {
                            code: plugin_api::ErrorCode::UnknownOutcome,
                            message: "HTTP worker panicked".into(),
                            detail: None,
                            retry_after_ms: None,
                            operation_id: None,
                        })
                    })
                })
                .collect()
        })
    }
    fn site_list(&self, _: &Invocation, _: PageRequest) -> Result<SitePage, ApiError> {
        Err(unsupported("Site list unavailable"))
    }
    fn site_get(&self, _: &Invocation, _: &str) -> Result<SiteView, ApiError> {
        Err(unsupported("Site port unavailable"))
    }
    fn site_stats(&self, _: &Invocation, _: &str) -> Result<StatsView, ApiError> {
        Err(unsupported("Site stats unavailable"))
    }
    fn site_credential_epoch(&self, _: &str) -> Result<Option<String>, ApiError> {
        Err(unsupported("Site credential epoch unavailable"))
    }
    fn browser_open(
        &self,
        _: &Invocation,
        _: BrowserPurpose,
    ) -> Result<Box<dyn BrowserSessionPort>, ApiError> {
        Err(unsupported("Browser port unavailable"))
    }
    fn boost_snapshot(
        &self,
        _: &Invocation,
        _: &[String],
        _: &[String],
    ) -> Result<BoostSnapshotView, ApiError> {
        Err(unsupported("Boost port unavailable"))
    }
    fn boost_run_cycle(&self, _: &Invocation, _: &str) -> Result<String, ApiError> {
        Err(unsupported("Boost run_cycle unavailable"))
    }
    fn subscribe_list(&self, _: &Invocation, _: PageRequest) -> Result<EntityPage, ApiError> {
        Err(unsupported("Subscribe port unavailable"))
    }
    fn subscribe_get(&self, _: &Invocation, _: &str) -> Result<EntityView, ApiError> {
        Err(unsupported("Subscribe port unavailable"))
    }
    fn library_list(&self, _: &Invocation, _: PageRequest) -> Result<EntityPage, ApiError> {
        Err(unsupported("Library port unavailable"))
    }
    fn library_media(
        &self,
        _: &Invocation,
        _: &str,
        _: PageRequest,
    ) -> Result<EntityPage, ApiError> {
        Err(unsupported("Library port unavailable"))
    }
    fn library_files(
        &self,
        _: &Invocation,
        _: &str,
        _: PageRequest,
    ) -> Result<EntityPage, ApiError> {
        Err(unsupported("Library port unavailable"))
    }
    fn download_list(
        &self,
        _: &Invocation,
        _: &str,
        _: PageRequest,
    ) -> Result<EntityPage, ApiError> {
        Err(unsupported("Download port unavailable"))
    }
    fn download_get(&self, _: &Invocation, _: &str) -> Result<EntityView, ApiError> {
        Err(unsupported("Download port unavailable"))
    }
    fn history_query(
        &self,
        _: &Invocation,
        _: &str,
        _: &[String],
        _: u64,
        _: u64,
        _: PageRequest,
    ) -> Result<EntityPage, ApiError> {
        Err(unsupported("History port unavailable"))
    }
    fn catalog_lookup(&self, _: &Invocation, _: &str) -> Result<MediaReference, ApiError> {
        Err(unsupported("Catalog port unavailable"))
    }
    fn notify_emit_event(&self, _: &Invocation, _: &str, _: &[String]) -> Result<String, ApiError> {
        Err(unsupported("Notify port unavailable"))
    }
    fn indexer_cache_cached(
        &self,
        _: &Invocation,
        _: &str,
        _: PageRequest,
    ) -> Result<TorrentBatch, ApiError> {
        Err(unsupported("Indexer cache port unavailable"))
    }
}

pub struct KvValue {
    pub value: Vec<u8>,
    pub etag: String,
}

pub struct NoServices;
impl HostPorts for NoServices {}

pub(crate) struct State {
    pub invocation: Invocation,
    pub context: types::Context,
    pub ports: Arc<dyn HostPorts>,
    pub memory: MemoryBudget,
    pub http_calls_used: u32,
    pub browsers: HashMap<u32, ManagedBrowserSession>,
    pub next_browser_rep: u32,
}

impl Drop for State {
    fn drop(&mut self) {
        crate::browser_ports::close_all_browsers(&mut self.browsers);
    }
}

pub(crate) fn unsupported(message: &str) -> ApiError {
    ApiError {
        code: plugin_api::ErrorCode::Unsupported,
        message: message.into(),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

pub(crate) fn api_error(code: types::ErrorCode, message: &str) -> types::ApiError {
    types::ApiError {
        code,
        message: message.into(),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

impl State {
    pub(crate) fn check_alive(&self) -> Result<(), types::ApiError> {
        if self.invocation.cancelled() {
            return Err(api_error(types::ErrorCode::Cancelled, "cancelled"));
        }
        if self.invocation.expired() {
            return Err(api_error(
                types::ErrorCode::DeadlineExceeded,
                "deadline exceeded",
            ));
        }
        Ok(())
    }

    pub(crate) fn check(&self, capability: &str) -> Result<(), types::ApiError> {
        self.check_alive()?;
        if !self.ports.authorized(&self.invocation, capability) {
            return Err(api_error(
                types::ErrorCode::PermissionDenied,
                "permission denied",
            ));
        }
        Ok(())
    }

    pub(crate) fn check_state_write(&self, capability: &str) -> Result<(), types::ApiError> {
        self.check_alive()?;
        if matches!(
            self.invocation.role,
            GuestRole::Lifecycle | GuestRole::Filter | GuestRole::Gate | GuestRole::Notify
        ) {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "state writes are forbidden in this role",
            ));
        }
        if capability == "blob"
            && !matches!(self.invocation.role, GuestRole::Task | GuestRole::Endpoint)
        {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "blob writes are restricted to task and endpoint contexts",
            ));
        }
        self.check(capability)
    }
}

impl types::Host for State {}
impl runtime::Host for State {
    fn current_context(&mut self) -> types::Context {
        self.context.clone()
    }
    fn cancelled(&mut self) -> bool {
        self.invocation.cancelled()
    }
}
impl clock::Host for State {
    fn now(&mut self) -> u64 {
        self.ports.now_ms()
    }
}
impl config::Host for State {
    fn get(&mut self) -> Result<types::ConfigView, types::ApiError> {
        self.check("config")?;
        let view = self
            .ports
            .config_get(&self.invocation)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(view.into())
    }
}
impl log::Host for State {
    fn write(
        &mut self,
        level: log::Level,
        message: String,
        fields: Vec<(String, String)>,
    ) -> Result<(), types::ApiError> {
        self.check("log")?;
        self.ports
            .log_write(&self.invocation, level.into(), &message, &fields)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(())
    }
}
impl kv::Host for State {
    fn get(&mut self, key: String) -> Result<Option<types::Entry>, types::ApiError> {
        self.check("kv")?;
        let value = self
            .ports
            .kv_get(&self.invocation, &key)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(value.map(|value| types::Entry {
            key,
            value: value.value,
            etag: value.etag,
        }))
    }
    fn list(
        &mut self,
        prefix: String,
        page: types::PageRequest,
    ) -> Result<types::EntryPage, types::ApiError> {
        self.check("kv")?;
        let out = self
            .ports
            .kv_list(&self.invocation, &prefix, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
    fn put(
        &mut self,
        key: String,
        value: Vec<u8>,
        if_match: Option<String>,
    ) -> Result<String, types::ApiError> {
        self.check_state_write("kv")?;
        let etag = self
            .ports
            .kv_put(&self.invocation, &key, &value, if_match.as_deref())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(etag)
    }
    fn delete(&mut self, key: String, if_match: Option<String>) -> Result<(), types::ApiError> {
        self.check_state_write("kv")?;
        self.ports
            .kv_delete(&self.invocation, &key, if_match.as_deref())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(())
    }
}
impl blob::Host for State {
    fn get(&mut self, key: String) -> Result<types::Entry, types::ApiError> {
        self.check("blob")?;
        let entry = self
            .ports
            .blob_get(&self.invocation, &key)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(entry.into())
    }
    fn list(
        &mut self,
        prefix: String,
        page: types::PageRequest,
    ) -> Result<types::EntryPage, types::ApiError> {
        self.check("blob")?;
        let out = self
            .ports
            .blob_list(&self.invocation, &prefix, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
    fn put(
        &mut self,
        key: String,
        bytes: Vec<u8>,
        if_match: Option<String>,
    ) -> Result<String, types::ApiError> {
        self.check_state_write("blob")?;
        let etag = self
            .ports
            .blob_put(&self.invocation, &key, &bytes, if_match.as_deref())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(etag)
    }
    fn delete(&mut self, key: String, if_match: Option<String>) -> Result<(), types::ApiError> {
        self.check_state_write("blob")?;
        self.ports
            .blob_delete(&self.invocation, &key, if_match.as_deref())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(())
    }
}
impl operations::Host for State {
    fn get(&mut self, id: String) -> Result<types::OperationView, types::ApiError> {
        self.check("operations")?;
        let op = self
            .ports
            .operations_get(&self.invocation, &id)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(op.into())
    }
    fn list(&mut self, page: types::PageRequest) -> Result<types::OperationPage, types::ApiError> {
        self.check("operations")?;
        let out = self
            .ports
            .operations_list(&self.invocation, page.into())
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(out.into())
    }
}
impl commands::Host for State {
    fn submit(
        &mut self,
        request: types::CommandRequest,
    ) -> Result<types::Accepted, types::ApiError> {
        self.check_alive()?;
        if !matches!(self.invocation.role, GuestRole::Task | GuestRole::Endpoint) {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "commands.submit is restricted to task and endpoint contexts",
            ));
        }
        let req: CommandRequest = request.into();
        let cap = format!("command:{}", req.action.kind_name());
        self.check(&cap)?;
        let operation_id = self
            .ports
            .commands_submit(&self.invocation, req)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(types::Accepted { operation_id })
    }
}
impl boost::Host for State {
    fn snapshot(
        &mut self,
        sites: Vec<String>,
        downloaders: Vec<String>,
    ) -> Result<boost::SnapshotView, types::ApiError> {
        self.check_alive()?;
        if !matches!(
            self.invocation.role,
            GuestRole::Task | GuestRole::Endpoint | GuestRole::Gate | GuestRole::Filter
        ) {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "boost.snapshot is restricted to task, endpoint, gate, and filter contexts",
            ));
        }
        self.check("read:boost")?;
        let snapshot = self
            .ports
            .boost_snapshot(&self.invocation, &sites, &downloaders)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(snapshot.into())
    }

    fn run_cycle(&mut self, override_json: String) -> Result<String, types::ApiError> {
        self.check_alive()?;
        if self.invocation.role != GuestRole::Task {
            return Err(api_error(
                types::ErrorCode::WrongContext,
                "boost.run-cycle is restricted to task context",
            ));
        }
        self.check("command:boost.run")?;
        let summary = self
            .ports
            .boost_run_cycle(&self.invocation, &override_json)
            .map_err(types::ApiError::from)?;
        self.check_alive()?;
        Ok(summary)
    }
}
