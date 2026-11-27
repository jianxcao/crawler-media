use crate::bindings::{crawler::plugin::types, indexer_world, maintenance_world};
use plugin_api as api;

impl From<types::SiteTarget> for api::SiteTarget {
    fn from(value: types::SiteTarget) -> Self {
        Self {
            path: value.path,
            query: value.query.into_iter().map(Into::into).collect(),
            auth_slot: value.auth_slot,
        }
    }
}

impl From<api::SiteTarget> for types::SiteTarget {
    fn from(value: api::SiteTarget) -> Self {
        Self {
            path: value.path,
            query: value.query.into_iter().map(Into::into).collect(),
            auth_slot: value.auth_slot,
        }
    }
}

impl From<types::HttpTarget> for api::HttpTarget {
    fn from(value: types::HttpTarget) -> Self {
        match value {
            types::HttpTarget::PublicUrl(url) => Self::PublicUrl(url),
            types::HttpTarget::SiteRelative(target) => Self::SiteRelative(target.into()),
        }
    }
}

impl From<api::HttpTarget> for types::HttpTarget {
    fn from(value: api::HttpTarget) -> Self {
        match value {
            api::HttpTarget::PublicUrl(url) => Self::PublicUrl(url),
            api::HttpTarget::SiteRelative(target) => Self::SiteRelative(target.into()),
        }
    }
}

impl From<types::HttpRequest> for api::HttpRequest {
    fn from(value: types::HttpRequest) -> Self {
        Self {
            method: value.method,
            target: value.target.into(),
            headers: value.headers.into_iter().map(Into::into).collect(),
            body: value.body,
            max_bytes: value.max_bytes,
        }
    }
}

impl From<api::HttpRequest> for types::HttpRequest {
    fn from(value: api::HttpRequest) -> Self {
        Self {
            method: value.method,
            target: value.target.into(),
            headers: value.headers.into_iter().map(Into::into).collect(),
            body: value.body,
            max_bytes: value.max_bytes,
        }
    }
}

impl From<api::HttpResponse> for types::HttpResponse {
    fn from(value: api::HttpResponse) -> Self {
        Self {
            status: value.status,
            headers: value.headers.into_iter().map(Into::into).collect(),
            body: value.body,
            truncated: value.truncated,
            receipt: value.receipt,
        }
    }
}

impl From<types::HttpResponse> for api::HttpResponse {
    fn from(value: types::HttpResponse) -> Self {
        Self {
            status: value.status,
            headers: value.headers.into_iter().map(Into::into).collect(),
            body: value.body,
            truncated: value.truncated,
            receipt: value.receipt,
        }
    }
}

impl From<api::SiteView> for types::SiteView {
    fn from(value: api::SiteView) -> Self {
        Self {
            handle: value.handle,
            id: value.id,
            name: value.name,
            enabled: value.enabled,
            profile_ref: value.profile_ref,
            capabilities: value.capabilities,
            maintenance_policy: value.maintenance_policy,
        }
    }
}

impl From<api::SitePage> for types::SitePage {
    fn from(value: api::SitePage) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<api::StatsView> for types::StatsView {
    fn from(value: api::StatsView) -> Self {
        Self {
            uploaded: value.uploaded,
            downloaded: value.downloaded,
        }
    }
}

impl From<types::StatsView> for api::StatsView {
    fn from(value: types::StatsView) -> Self {
        Self {
            uploaded: value.uploaded,
            downloaded: value.downloaded,
        }
    }
}

impl From<types::Category> for api::Category {
    fn from(value: types::Category) -> Self {
        Self {
            id: value.id,
            name: value.name,
        }
    }
}

impl From<types::Descriptor> for api::Descriptor {
    fn from(value: types::Descriptor) -> Self {
        Self {
            id: value.id,
            name: value.name,
            kind: value.kind,
            capabilities: value.capabilities,
        }
    }
}

impl From<api::SearchQuery> for types::SearchQuery {
    fn from(value: api::SearchQuery) -> Self {
        Self {
            keyword: value.keyword,
            page: value.page,
            categories: value.categories,
        }
    }
}

impl From<types::TorrentRaw> for api::TorrentRaw {
    fn from(value: types::TorrentRaw) -> Self {
        Self {
            provider_local_id: value.provider_local_id,
            title: value.title,
            size: value.size,
            download_ref: value.download_ref,
            seeders: value.seeders,
            leechers: value.leechers,
            published_at: value.published_at,
            category: value.category,
            free: value.free,
            hr: value.hr,
        }
    }
}

impl From<api::TorrentRaw> for types::TorrentRaw {
    fn from(value: api::TorrentRaw) -> Self {
        Self {
            provider_local_id: value.provider_local_id,
            title: value.title,
            size: value.size,
            download_ref: value.download_ref,
            seeders: value.seeders,
            leechers: value.leechers,
            published_at: value.published_at,
            category: value.category,
            free: value.free,
            hr: value.hr,
        }
    }
}

impl From<types::TorrentBatch> for api::TorrentBatch {
    fn from(value: types::TorrentBatch) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
            has_more: value.has_more,
        }
    }
}

impl From<api::TorrentBatch> for types::TorrentBatch {
    fn from(value: api::TorrentBatch) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
            has_more: value.has_more,
        }
    }
}

impl From<types::MaintenanceState> for api::MaintenanceState {
    fn from(value: types::MaintenanceState) -> Self {
        match value {
            types::MaintenanceState::Completed => Self::Completed,
            types::MaintenanceState::AlreadyCompleted => Self::AlreadyCompleted,
            types::MaintenanceState::AuthExpired => Self::AuthExpired,
            types::MaintenanceState::Challenge => Self::Challenge,
            types::MaintenanceState::Unknown => Self::Unknown,
        }
    }
}

impl From<types::MaintenanceOutcome> for api::MaintenanceOutcome {
    fn from(value: types::MaintenanceOutcome) -> Self {
        Self {
            state: value.state.into(),
            evidence: value.evidence,
            session_capture: value.session_capture,
        }
    }
}

impl From<api::MaintenanceAction> for types::MaintenanceAction {
    fn from(value: api::MaintenanceAction) -> Self {
        match value {
            api::MaintenanceAction::VerifySession => Self::VerifySession,
            api::MaintenanceAction::CheckIn => Self::CheckIn,
        }
    }
}

impl From<indexer_world::crawler::plugin::browser::Purpose> for api::BrowserPurpose {
    fn from(value: indexer_world::crawler::plugin::browser::Purpose) -> Self {
        match value {
            indexer_world::crawler::plugin::browser::Purpose::Search => Self::Search,
            indexer_world::crawler::plugin::browser::Purpose::Maintenance => Self::Maintenance,
        }
    }
}

impl From<api::BrowserElement> for indexer_world::crawler::plugin::browser::Element {
    fn from(value: api::BrowserElement) -> Self {
        Self {
            handle: value.handle,
            visible_text: value.visible_text,
        }
    }
}

impl From<indexer_world::crawler::plugin::browser::FillValue> for api::BrowserFillValue {
    fn from(value: indexer_world::crawler::plugin::browser::FillValue) -> Self {
        match value {
            indexer_world::crawler::plugin::browser::FillValue::Text(s) => Self::Text(s),
            indexer_world::crawler::plugin::browser::FillValue::CredentialSlot(s) => {
                Self::CredentialSlot(s)
            }
        }
    }
}

impl From<indexer_world::exports::crawler::plugin::lifecycle::HealthState> for api::HealthState {
    fn from(value: indexer_world::exports::crawler::plugin::lifecycle::HealthState) -> Self {
        match value {
            indexer_world::exports::crawler::plugin::lifecycle::HealthState::Healthy => {
                Self::Healthy
            }
            indexer_world::exports::crawler::plugin::lifecycle::HealthState::Degraded(msg) => {
                Self::Degraded(msg)
            }
        }
    }
}

impl From<maintenance_world::exports::crawler::plugin::lifecycle::HealthState>
    for api::HealthState
{
    fn from(value: maintenance_world::exports::crawler::plugin::lifecycle::HealthState) -> Self {
        match value {
            maintenance_world::exports::crawler::plugin::lifecycle::HealthState::Healthy => {
                Self::Healthy
            }
            maintenance_world::exports::crawler::plugin::lifecycle::HealthState::Degraded(msg) => {
                Self::Degraded(msg)
            }
        }
    }
}
