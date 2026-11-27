use crate::bindings::{
    crawler::plugin::{boost, log, types},
    endpoint_world, exports, task_world,
};
use plugin_api as api;

impl From<api::ConfigView> for types::ConfigView {
    fn from(value: api::ConfigView) -> Self {
        Self {
            revision: value.revision,
            validated_json: value.validated_json,
        }
    }
}

impl From<log::Level> for api::LogLevel {
    fn from(value: log::Level) -> Self {
        match value {
            log::Level::Debug => Self::Debug,
            log::Level::Info => Self::Info,
            log::Level::Warn => Self::Warn,
            log::Level::Error => Self::Error,
        }
    }
}

impl From<types::PageRequest> for api::PageRequest {
    fn from(value: types::PageRequest) -> Self {
        Self {
            cursor: value.cursor,
            limit: value.limit,
        }
    }
}

impl From<api::StateEntry> for types::Entry {
    fn from(value: api::StateEntry) -> Self {
        Self {
            key: value.key,
            value: value.value,
            etag: value.etag,
        }
    }
}

impl From<api::EntryPage> for types::EntryPage {
    fn from(value: api::EntryPage) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<api::OperationState> for types::OperationState {
    fn from(value: api::OperationState) -> Self {
        match value {
            api::OperationState::Queued => Self::Queued,
            api::OperationState::Running => Self::Running,
            api::OperationState::Waiting => Self::Waiting,
            api::OperationState::Blocked => Self::Blocked,
            api::OperationState::Succeeded => Self::Succeeded,
            api::OperationState::Rejected => Self::Rejected,
            api::OperationState::Deferred => Self::Deferred,
            api::OperationState::Failed => Self::Failed,
            api::OperationState::Cancelled => Self::Cancelled,
            api::OperationState::Unknown => Self::Unknown,
        }
    }
}

impl From<api::OperationView> for types::OperationView {
    fn from(value: api::OperationView) -> Self {
        Self {
            id: value.id,
            state: value.state.into(),
            revision: value.revision,
            next_run_at: value.next_run_at,
        }
    }
}

impl From<api::OperationPage> for types::OperationPage {
    fn from(value: api::OperationPage) -> Self {
        Self {
            items: value.items.into_iter().map(Into::into).collect(),
            next_cursor: value.next_cursor,
        }
    }
}

impl From<types::MaintenanceAction> for api::MaintenanceAction {
    fn from(value: types::MaintenanceAction) -> Self {
        match value {
            types::MaintenanceAction::VerifySession => Self::VerifySession,
            types::MaintenanceAction::CheckIn => Self::CheckIn,
        }
    }
}

impl From<types::OwnedRevision> for api::OwnedRevision {
    fn from(value: types::OwnedRevision) -> Self {
        Self {
            handle: value.handle,
            revision: value.revision,
        }
    }
}

impl From<types::DownloadOptions> for api::DownloadOptions {
    fn from(value: types::DownloadOptions) -> Self {
        Self {
            relative_directory: value.relative_directory,
            category: value.category,
            tags: value.tags,
            paused: value.paused,
            upload_limit: value.upload_limit,
            download_limit: value.download_limit,
            sequential: value.sequential,
            first_last_piece: value.first_last_piece,
        }
    }
}

impl From<types::TaskControl> for api::TaskControl {
    fn from(value: types::TaskControl) -> Self {
        match value {
            types::TaskControl::Pause => Self::Pause,
            types::TaskControl::Resume => Self::Resume,
            types::TaskControl::Tags(tags) => Self::Tags(tags),
            types::TaskControl::Category(cat) => Self::Category(cat),
            types::TaskControl::Limits((upload, download)) => Self::Limits { upload, download },
        }
    }
}

impl From<api::BoostDisk> for boost::Disk {
    fn from(value: api::BoostDisk) -> Self {
        Self {
            available_bytes: value.available_bytes,
            observed_at: value.observed_at,
            status: value.status,
        }
    }
}

impl From<api::BoostReservationView> for boost::ReservationView {
    fn from(value: api::BoostReservationView) -> Self {
        Self {
            handle: value.handle,
            operation_id: value.operation_id,
            quota_key: value.quota_key,
            downloader_id: value.downloader_id,
            site_id: value.site_id,
            info_hash: value.info_hash,
            reserved_bytes: value.reserved_bytes,
            stage: value.stage,
            expires_at: value.expires_at,
            revision: value.revision,
        }
    }
}

impl From<api::BoostTaskView> for boost::TaskFactView {
    fn from(value: api::BoostTaskView) -> Self {
        Self {
            handle: value.handle,
            quota_key: value.quota_key,
            downloader_id: value.downloader_id,
            site_id: value.site_id,
            info_hash: value.info_hash,
            state: value.state,
            title: value.title,
            progress_permille: value.progress_permille,
            downloaded_bytes: value.downloaded_bytes,
            upload_speed: value.upload_speed,
            download_speed: value.download_speed,
            seeding_secs: value.seeding_secs,
            added_at: value.added_at,
            completed_at: value.completed_at,
            last_activity_at: value.last_activity_at,
            category: value.category,
            tags: value.tags,
            task_bytes: value.task_bytes,
            retained_bytes: value.retained_bytes,
            uploaded_bytes: value.uploaded_bytes,
            hr_protected: value.hr_protected,
            hold_until: value.hold_until,
            free_until: value.free_until,
            protection_state: value.protection_state,
            transfer_active: value.transfer_active,
            library_referenced: value.library_referenced,
            observed_at: value.observed_at,
            revision: value.revision,
        }
    }
}

impl From<api::BoostSnapshotView> for boost::SnapshotView {
    fn from(value: api::BoostSnapshotView) -> Self {
        Self {
            revision: value.revision,
            used_bytes: value.used_bytes,
            reserved_bytes: value.reserved_bytes,
            retained_file_bytes: value.retained_file_bytes,
            reservations: value.reservations.into_iter().map(Into::into).collect(),
            tasks: value.tasks.into_iter().map(Into::into).collect(),
            disk: value.disk.into(),
        }
    }
}

impl From<types::Command> for api::CommandAction {
    fn from(value: types::Command) -> Self {
        match value {
            types::Command::SiteMaintenance((site_handle, action)) => Self::SiteMaintenance {
                site_handle,
                action: action.into(),
            },
            types::Command::ScheduleSet((task_id, interval_secs, enabled, expected_revision)) => {
                Self::ScheduleSet {
                    task_id,
                    interval_secs,
                    enabled,
                    expected_revision,
                }
            }
            types::Command::ScheduleDisable(owned) => Self::ScheduleDisable {
                handle: owned.handle,
                revision: owned.revision,
            },
            types::Command::DownloaderAdd((downloader_id, download_ref, options)) => {
                Self::DownloaderAdd {
                    downloader_id,
                    download_ref,
                    options: options.into(),
                }
            }
            types::Command::DownloaderControl((target, control)) => Self::DownloaderControl {
                target: target.into(),
                control: control.into(),
            },
            types::Command::DownloaderDeleteTask((downloader_id, revision, task_handle)) => {
                Self::DownloaderDeleteTask {
                    downloader_id,
                    revision,
                    task_handle,
                }
            }
            types::Command::DownloaderPrepareCleanup((targets, reason)) => {
                Self::DownloaderPrepareCleanup {
                    targets: targets.into_iter().map(Into::into).collect(),
                    reason,
                }
            }
            types::Command::DownloaderDeleteFiles((target, plan_handle)) => {
                Self::DownloaderDeleteFiles {
                    target: target.into(),
                    plan_handle,
                }
            }
            other => {
                let kind = match &other {
                    types::Command::IndexerSearch(_) => "indexer.search",
                    types::Command::IndexerRss(_) => "indexer.rss",
                    types::Command::SiteCreate(_) => "site.create",
                    types::Command::SiteUpdate(_) => "site.update",
                    types::Command::SiteDelete(_) => "site.delete",
                    types::Command::SubscribeCreate(_) => "subscribe.create",
                    types::Command::SubscribeUpdate(_) => "subscribe.update",
                    types::Command::SubscribeDelete(_) => "subscribe.delete",
                    types::Command::SubscribeComplete(_) => "subscribe.complete",
                    types::Command::LibraryRefresh(_) => "library.refresh",
                    types::Command::TransferRequest(_) => "transfer.request",
                    types::Command::ScrapeRequest(_) => "scrape.request",
                    types::Command::NotifySend(_) => "notify.send",
                    types::Command::SiteMaintenance(_)
                    | types::Command::ScheduleSet(_)
                    | types::Command::ScheduleDisable(_)
                    | types::Command::DownloaderAdd(_)
                    | types::Command::DownloaderControl(_)
                    | types::Command::DownloaderDeleteTask(_)
                    | types::Command::DownloaderPrepareCleanup(_)
                    | types::Command::DownloaderDeleteFiles(_) => unreachable!(),
                };
                Self::Other {
                    kind: kind.into(),
                    summary: format!("{other:?}"),
                }
            }
        }
    }
}

impl From<types::CommandRequest> for api::CommandRequest {
    fn from(value: types::CommandRequest) -> Self {
        Self {
            action: value.action.into(),
            idempotency_key: value.idempotency_key,
            expected_revision: value.expected_revision,
        }
    }
}

impl From<api::TaskInput> for task_world::exports::crawler::plugin::tasks::TaskInput {
    fn from(value: api::TaskInput) -> Self {
        Self {
            task_id: value.task_id,
            validated_input: value.validated_input,
            job_id: value.job_id,
            attempt: value.attempt,
        }
    }
}

impl From<task_world::exports::crawler::plugin::tasks::TaskOutcome> for api::TaskOutcome {
    fn from(value: task_world::exports::crawler::plugin::tasks::TaskOutcome) -> Self {
        Self {
            partial: value.partial,
            progress: value.progress,
            operation_ids: value.operation_ids,
            summary: value.summary,
        }
    }
}

impl From<api::Header> for types::Header {
    fn from(value: api::Header) -> Self {
        Self {
            name: value.name,
            value: value.value,
        }
    }
}

impl From<types::Header> for api::Header {
    fn from(value: types::Header) -> Self {
        Self {
            name: value.name,
            value: value.value,
        }
    }
}

impl From<api::EndpointRequest>
    for endpoint_world::exports::crawler::plugin::endpoints::EndpointRequest
{
    fn from(value: api::EndpointRequest) -> Self {
        Self {
            method: value.method,
            path: value.path,
            query: value.query.into_iter().map(Into::into).collect(),
            body: value.body,
        }
    }
}

impl From<endpoint_world::exports::crawler::plugin::endpoints::EndpointResponse>
    for api::EndpointResponse
{
    fn from(value: endpoint_world::exports::crawler::plugin::endpoints::EndpointResponse) -> Self {
        Self {
            status: value.status,
            headers: value.headers.into_iter().map(Into::into).collect(),
            body: value.body,
        }
    }
}

impl From<exports::crawler::plugin::lifecycle::HealthState> for api::HealthState {
    fn from(value: exports::crawler::plugin::lifecycle::HealthState) -> Self {
        match value {
            exports::crawler::plugin::lifecycle::HealthState::Healthy => Self::Healthy,
            exports::crawler::plugin::lifecycle::HealthState::Degraded(msg) => Self::Degraded(msg),
        }
    }
}

impl From<task_world::exports::crawler::plugin::lifecycle::HealthState> for api::HealthState {
    fn from(value: task_world::exports::crawler::plugin::lifecycle::HealthState) -> Self {
        match value {
            task_world::exports::crawler::plugin::lifecycle::HealthState::Healthy => Self::Healthy,
            task_world::exports::crawler::plugin::lifecycle::HealthState::Degraded(msg) => {
                Self::Degraded(msg)
            }
        }
    }
}

impl From<endpoint_world::exports::crawler::plugin::lifecycle::HealthState> for api::HealthState {
    fn from(value: endpoint_world::exports::crawler::plugin::lifecycle::HealthState) -> Self {
        match value {
            endpoint_world::exports::crawler::plugin::lifecycle::HealthState::Healthy => {
                Self::Healthy
            }
            endpoint_world::exports::crawler::plugin::lifecycle::HealthState::Degraded(msg) => {
                Self::Degraded(msg)
            }
        }
    }
}
