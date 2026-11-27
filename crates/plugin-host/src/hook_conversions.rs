use crate::bindings::crawler::plugin::types;
use plugin_api as api;

impl From<&api::HookContext> for types::Context {
    fn from(c: &api::HookContext) -> Self {
        Self {
            invocation_id: c.invocation_id.clone(),
            cause_id: c.cause_id.clone(),
            plugin_id: c.plugin_id.clone(),
            generation: c.generation,
            config_revision: c.config_revision,
            subject: c.subject.clone(),
            site_id: c.site_id.clone(),
            subscribe_id: c.subscribe_id.clone(),
            library_id: c.library_id.clone(),
            deadline_ms: c.deadline_ms,
            cancelled: c.cancelled,
        }
    }
}

pub(crate) fn candidate_to_wit(c: &api::Candidate) -> types::TorrentView {
    types::TorrentView {
        handle: c.handle.clone(),
        site_id: c.site_id.clone(),
        title: c.title.clone(),
        size: c.size,
        seeders: c.seeders,
        leechers: c.leechers,
        published_at: c.published_at,
        free: c.free,
        hr: c.hr,
        release: c.release.clone(),
        score: c.score,
        labels: c.labels.clone(),
        revision: c.revision,
    }
}

fn search_before_to_wit(v: &api::SearchBefore) -> types::SearchBefore {
    types::SearchBefore {
        query: v.query.clone().into(),
        site_ids: v.site_ids.clone(),
    }
}

fn subscribe_select_to_wit(v: &api::SubscribeSelect) -> types::SubscribeSelect {
    types::SubscribeSelect {
        subscribe_id: v.subscribe_id.clone(),
        media_id: v.media_id.clone(),
        candidates: v.candidates.iter().map(candidate_to_wit).collect(),
        admitted_handles: v.admitted_handles.clone(),
    }
}

fn download_before_to_wit(v: &api::DownloadBefore) -> types::DownloadBefore {
    types::DownloadBefore {
        candidate: candidate_to_wit(&v.candidate),
        purpose: v.purpose.clone(),
        plan: types::DownloadPlanView {
            downloader_id: v.plan.downloader_id.clone(),
            relative_directory: v.plan.relative_directory.clone(),
            category: v.plan.category.clone(),
            tags: v.plan.tags.clone(),
            paused: v.plan.paused,
            upload_limit: v.plan.upload_limit,
            download_limit: v.plan.download_limit,
            sequential: v.plan.sequential,
            first_last_piece: v.plan.first_last_piece,
        },
    }
}

impl From<&api::FilterInput> for types::FilterInput {
    fn from(value: &api::FilterInput) -> Self {
        let payload = match &value.payload {
            api::FilterPayload::SearchBefore(v) => {
                types::FilterPayload::SearchBefore(search_before_to_wit(v))
            }
            api::FilterPayload::SearchSiteAfter(v) => {
                types::FilterPayload::SearchSiteAfter(types::SearchSiteAfter {
                    site_id: v.site_id.clone(),
                    query: v.query.clone().into(),
                    candidates: v.candidates.iter().map(candidate_to_wit).collect(),
                })
            }
            api::FilterPayload::SearchAfter(v) => {
                types::FilterPayload::SearchAfter(types::SearchAfter {
                    candidates: v.candidates.iter().map(candidate_to_wit).collect(),
                    site_failures: v
                        .site_failures
                        .iter()
                        .map(|f| types::SiteFailure {
                            site_id: f.site_id.clone(),
                            error: f.error.clone().into(),
                        })
                        .collect(),
                })
            }
            api::FilterPayload::FilterAfter(v) => {
                types::FilterPayload::FilterAfter(types::FilterAfter {
                    candidates: v.candidates.iter().map(candidate_to_wit).collect(),
                    admitted_handles: v.admitted_handles.clone(),
                })
            }
            api::FilterPayload::SubscribeScore(v) => {
                types::FilterPayload::SubscribeScore(types::SubscribeScore {
                    subscribe_id: v.subscribe_id.clone(),
                    media_id: v.media_id.clone(),
                    candidates: v.candidates.iter().map(candidate_to_wit).collect(),
                })
            }
            api::FilterPayload::SubscribeSelect(v) => {
                types::FilterPayload::SubscribeSelect(subscribe_select_to_wit(v))
            }
            api::FilterPayload::DownloadBefore(v) => {
                types::FilterPayload::DownloadBefore(download_before_to_wit(v))
            }
            api::FilterPayload::TransferRename(v) => {
                types::FilterPayload::TransferRename(types::TransferRename {
                    media_id: v.media_id.clone(),
                    library_id: v.library_id.clone(),
                    source_path: v.source_path.clone(),
                    proposed_relative_path: v.proposed_relative_path.clone(),
                    is_movie: v.is_movie,
                })
            }
        };
        Self {
            schema_version: value.schema_version,
            context: (&value.context).into(),
            payload,
        }
    }
}

impl From<types::Patch> for api::Patch {
    fn from(value: types::Patch) -> Self {
        match value {
            types::Patch::Drop(h) => Self::Drop(h),
            types::Patch::SetOrder(hs) => Self::SetOrder(hs),
            types::Patch::Rescore(r) => Self::Rescore(api::RescorePatch {
                handle: r.handle,
                delta: r.delta,
                reason: r.reason,
            }),
            types::Patch::SearchQuery(q) => Self::SearchQuery(api::SearchQueryPatch {
                keyword: q.keyword,
                site_ids: q.site_ids,
                categories: q.categories,
            }),
            types::Patch::SelectHandles(hs) => Self::SelectHandles(hs),
            types::Patch::DownloadPlan(p) => Self::DownloadPlan(api::DownloadPlanPatch {
                downloader_id: p.downloader_id,
                relative_directory: p.relative_directory,
                category: p.category,
                add_tags: p.add_tags,
                remove_tags: p.remove_tags,
                paused: p.paused,
                upload_limit: p.upload_limit,
                download_limit: p.download_limit,
                sequential: p.sequential,
                first_last_piece: p.first_last_piece,
            }),
            types::Patch::RenamePath(p) => Self::RenamePath(p),
        }
    }
}

impl From<&api::GateInput> for types::GateInput {
    fn from(value: &api::GateInput) -> Self {
        let payload = match &value.payload {
            api::GatePayload::None => types::GatePayload::None,
            api::GatePayload::SearchBefore(v) => {
                types::GatePayload::SearchBefore(search_before_to_wit(v))
            }
            api::GatePayload::SubscribeSelect(v) => {
                types::GatePayload::SubscribeSelect(subscribe_select_to_wit(v))
            }
            api::GatePayload::DownloadBefore(v) => {
                types::GatePayload::DownloadBefore(download_before_to_wit(v))
            }
        };
        Self {
            schema_version: value.schema_version,
            context: (&value.context).into(),
            hook_id: value.hook_id.clone(),
            handles: value.handles.clone(),
            revision: value.revision,
            payload,
        }
    }
}

impl From<types::GateDecision> for hooks::Gate {
    fn from(value: types::GateDecision) -> Self {
        match value {
            types::GateDecision::Allow => Self::Allow,
            types::GateDecision::Abstain => Self::Abstain,
            types::GateDecision::Reject(reason) => Self::Reject(reason),
            types::GateDecision::Defer((until_ms, reason)) => Self::Defer { until_ms, reason },
        }
    }
}

impl From<&api::Event> for types::Event {
    fn from(value: &api::Event) -> Self {
        let payload = match &value.payload {
            api::EventPayload::None => types::EventPayload::None,
            api::EventPayload::DownloadAfter(v) => {
                types::EventPayload::DownloadAfter(types::DownloadAfterEvent {
                    operation_id: v.operation_id.clone(),
                    downloader_id: v.downloader_id.clone(),
                    info_hash: v.info_hash.clone(),
                    outcome: v.outcome.clone(),
                    verified_total_bytes: v.verified_total_bytes,
                })
            }
            api::EventPayload::TransferCompleted(v) => {
                types::EventPayload::TransferCompleted(types::TransferCompletedEvent {
                    media_id: v.media_id.clone(),
                    library_id: v.library_id.clone(),
                    relative_path: v.relative_path.clone(),
                    file_count: v.file_count,
                    total_bytes: v.total_bytes,
                })
            }
            api::EventPayload::SiteCheckinAfter(v) => {
                types::EventPayload::SiteCheckinAfter(types::SiteCheckinAfterEvent {
                    site_id: v.site_id.clone(),
                    action: v.action.clone(),
                    state: v.state.clone(),
                    window_key: v.window_key.clone(),
                })
            }
        };
        Self {
            schema_version: value.schema_version,
            context: (&value.context).into(),
            id: value.id.clone(),
            kind: value.kind.clone(),
            occurred_at: value.occurred_at,
            handles: value.handles.clone(),
            revision: value.revision,
            payload,
        }
    }
}
