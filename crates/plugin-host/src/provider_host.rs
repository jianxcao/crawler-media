use crate::bindings::indexer_world::IndexerPluginPre;
use crate::bindings::maintenance_world::MaintenancePluginPre;
use crate::engine::invalid;
use crate::ports::State;
use crate::workers::{Dispatch, LifecycleTarget, WorkClass, WorkItem};
use crate::{Invocation, PluginHost};
use plugin_api::{
    ApiError, Category, Descriptor, ErrorCode, Failure, GuestRole, HealthState, HttpRequest,
    HttpTarget, MaintenanceAction, MaintenanceOutcome, SearchQuery, StatsView, TorrentBatch,
    TorrentRaw,
};

pub use plugin_api::{INDEXER_WORLD, MAINTENANCE_WORLD};

const INDEXER_IMPORTS: &[&str] = &[
    "types",
    "runtime",
    "clock",
    "config",
    "log",
    "kv",
    "blob",
    "site",
    "subscribe",
    "library",
    "download",
    "history",
    "catalog",
    "operations",
    "http",
    "browser",
];

const MAINTENANCE_IMPORTS: &[&str] = &[
    "types",
    "runtime",
    "clock",
    "config",
    "log",
    "kv",
    "blob",
    "site",
    "subscribe",
    "library",
    "download",
    "history",
    "catalog",
    "operations",
    "http",
    "browser",
];

#[derive(Clone)]
pub struct CompiledIndexer {
    pub(crate) pre: IndexerPluginPre<State>,
}

#[derive(Clone)]
pub struct CompiledMaintenance {
    pub(crate) pre: MaintenancePluginPre<State>,
}

impl PluginHost {
    pub fn load_indexer(&self, bytes: &[u8], world: &str) -> Result<CompiledIndexer, Failure> {
        if world != INDEXER_WORLD {
            return Err(Failure::InvalidComponent("unsupported exact world".into()));
        }
        let instance = self.instantiate_checked(
            bytes,
            INDEXER_IMPORTS,
            "indexer",
            &[
                "describe",
                "search",
                "rss",
                "detail",
                "resolve-download",
                "user-stats",
                "categories",
            ],
        )?;
        let pre = IndexerPluginPre::new(instance).map_err(invalid)?;
        Ok(CompiledIndexer { pre })
    }

    pub fn load_maintenance(
        &self,
        bytes: &[u8],
        world: &str,
    ) -> Result<CompiledMaintenance, Failure> {
        if world != MAINTENANCE_WORLD {
            return Err(Failure::InvalidComponent("unsupported exact world".into()));
        }
        let instance =
            self.instantiate_checked(bytes, MAINTENANCE_IMPORTS, "maintenance", &["run"])?;
        let pre = MaintenancePluginPre::new(instance).map_err(invalid)?;
        Ok(CompiledMaintenance { pre })
    }

    pub fn validate_indexer(
        &self,
        indexer: &CompiledIndexer,
        invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.validate_target(
            indexer.pre.engine(),
            LifecycleTarget::Indexer(indexer.clone()),
            invocation,
            config_schema,
            state_schema,
        )
    }

    pub fn validate_maintenance(
        &self,
        maintenance: &CompiledMaintenance,
        invocation: Invocation,
        config_schema: &str,
        state_schema: u32,
    ) -> Result<(), Failure> {
        self.validate_target(
            maintenance.pre.engine(),
            LifecycleTarget::Maintenance(maintenance.clone()),
            invocation,
            config_schema,
            state_schema,
        )
    }

    pub fn health_indexer(
        &self,
        indexer: &CompiledIndexer,
        invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.health_target(
            indexer.pre.engine(),
            LifecycleTarget::Indexer(indexer.clone()),
            invocation,
        )
    }

    pub fn health_maintenance(
        &self,
        maintenance: &CompiledMaintenance,
        invocation: Invocation,
    ) -> Result<HealthState, Failure> {
        self.health_target(
            maintenance.pre.engine(),
            LifecycleTarget::Maintenance(maintenance.clone()),
            invocation,
        )
    }

    pub fn indexer_describe(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
    ) -> Result<Vec<Descriptor>, Failure> {
        self.check_ready(indexer.pre.engine())?;
        if invocation.role != GuestRole::Lifecycle {
            invocation.role = GuestRole::Indexer;
        }
        let indexer = indexer.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerDescribe { indexer, reply },
            },
        )?
    }

    pub fn indexer_search(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
        provider_id: &str,
        query: SearchQuery,
    ) -> Result<TorrentBatch, Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        let provider_id = provider_id.to_string();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerSearch {
                    indexer,
                    provider_id,
                    query,
                    reply,
                },
            },
        )?
    }

    pub fn indexer_rss(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
        provider_id: &str,
        cursor: Option<String>,
    ) -> Result<TorrentBatch, Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        let provider_id = provider_id.to_string();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerRss {
                    indexer,
                    provider_id,
                    cursor,
                    reply,
                },
            },
        )?
    }

    pub fn indexer_detail(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
        provider_id: &str,
        torrent_ref: String,
    ) -> Result<TorrentRaw, Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        let provider_id = provider_id.to_string();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerDetail {
                    indexer,
                    provider_id,
                    torrent_ref,
                    reply,
                },
            },
        )?
    }

    pub(crate) fn indexer_resolve_download_with_auth(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
        provider_id: &str,
        download_ref: String,
    ) -> Result<(HttpRequest, bool), Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        let provider_id = provider_id.to_string();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerResolveDownload {
                    indexer,
                    provider_id,
                    download_ref,
                    reply,
                },
            },
        )?
    }

    pub fn indexer_resolve_download(
        &self,
        indexer: &CompiledIndexer,
        invocation: Invocation,
        provider_id: &str,
        download_ref: String,
    ) -> Result<HttpRequest, Failure> {
        let site_id = invocation.site_id.clone();
        let credential_epoch = invocation.credential_epoch.clone();
        let (req, site_auth_granted) = self.indexer_resolve_download_with_auth(
            indexer,
            invocation,
            provider_id,
            download_ref,
        )?;
        validate_resolved_download_request(
            &req,
            site_id.as_deref(),
            credential_epoch.as_deref(),
            site_auth_granted,
        )?;
        Ok(req)
    }

    pub fn indexer_user_stats(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
    ) -> Result<StatsView, Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerUserStats { indexer, reply },
            },
        )?
    }

    pub fn indexer_categories(
        &self,
        indexer: &CompiledIndexer,
        mut invocation: Invocation,
    ) -> Result<Vec<Category>, Failure> {
        self.check_ready(indexer.pre.engine())?;
        invocation.role = GuestRole::Indexer;
        let indexer = indexer.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::IndexerCategories { indexer, reply },
            },
        )?
    }

    pub fn maintenance_run(
        &self,
        maintenance: &CompiledMaintenance,
        mut invocation: Invocation,
        action: MaintenanceAction,
        job_id: String,
    ) -> Result<MaintenanceOutcome, Failure> {
        self.check_ready(maintenance.pre.engine())?;
        invocation.role = GuestRole::Maintenance;
        let maintenance = maintenance.clone();
        self.submit_wait(
            &self.interactive,
            invocation,
            self.core.limits.provider_wall,
            |inv, reply, ticket| WorkItem {
                ticket,
                class: WorkClass::Interactive,
                fuel: self.core.limits.provider_fuel,
                invocation: inv,
                ports: self.ports.clone(),
                dispatch: Dispatch::MaintenanceRun {
                    maintenance,
                    action,
                    job_id,
                    reply,
                },
            },
        )?
    }
}

fn guest_failure(code: ErrorCode, message: impl Into<String>) -> Failure {
    Failure::Guest(ApiError {
        code,
        message: message.into(),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    })
}

fn validate_resolved_download_auth_slot(
    slot: &str,
    bound_site_id: &str,
    credential_epoch: Option<&str>,
    site_auth_granted: bool,
) -> Result<(), Failure> {
    if !site_auth_granted {
        return Err(guest_failure(
            ErrorCode::PermissionDenied,
            "http:site-auth permission denied",
        ));
    }
    if credential_epoch.is_none_or(|ep| ep.trim().is_empty()) {
        return Err(guest_failure(
            ErrorCode::WrongContext,
            "http:site-auth requires bound credential_epoch",
        ));
    }
    let stripped = crate::http_site_ports::strip_site_prefix(slot);
    if stripped.is_empty() {
        return Err(guest_failure(
            ErrorCode::InvalidArgument,
            "auth_slot must not be empty",
        ));
    }
    if slot
        .trim()
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("site:"))
        && stripped != bound_site_id
    {
        return Err(guest_failure(
            ErrorCode::PermissionDenied,
            "auth_slot cannot reference another site",
        ));
    }
    Ok(())
}

fn validate_resolved_download_request(
    req: &HttpRequest,
    site_id: Option<&str>,
    credential_epoch: Option<&str>,
    site_auth_granted: bool,
) -> Result<(), Failure> {
    let method = req.method.trim().to_ascii_uppercase();
    if !matches!(method.as_str(), "GET" | "POST") {
        return Err(guest_failure(
            ErrorCode::InvalidArgument,
            format!("unsupported download method `{method}`"),
        ));
    }
    for header in &req.headers {
        if header.name.trim().is_empty()
            || header
                .name
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || header.value.chars().any(char::is_control)
        {
            return Err(guest_failure(
                ErrorCode::InvalidArgument,
                "invalid characters in download header",
            ));
        }
        let lower = header.name.trim().to_ascii_lowercase();
        if crate::http_site_ports::FORBIDDEN_REQUEST_HEADERS.contains(&lower.as_str()) {
            return Err(guest_failure(
                ErrorCode::PermissionDenied,
                "forbidden request header",
            ));
        }
    }
    match &req.target {
        HttpTarget::PublicUrl(url) => crate::indexer_bridge_map::validate_public_download_url(url)
            .map_err(|e| guest_failure(ErrorCode::InvalidArgument, e.to_string())),
        HttpTarget::SiteRelative(target) => {
            if let Err(msg) = crate::http_site_ports::check_site_relative_path(&target.path) {
                return Err(guest_failure(ErrorCode::InvalidArgument, msg));
            }
            let Some(bound_site_id) = crate::http_site_ports::canonical_site_id(site_id) else {
                return Err(guest_failure(
                    ErrorCode::WrongContext,
                    "site-relative http target requires invocation site_id",
                ));
            };
            if let Some(slot) = target.auth_slot.as_deref() {
                validate_resolved_download_auth_slot(
                    slot,
                    bound_site_id,
                    credential_epoch,
                    site_auth_granted,
                )?;
            }
            Ok(())
        }
    }
}
