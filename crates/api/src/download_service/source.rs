use domain::DownloaderId;
use downloader::{
    DownloadSubmitError, FrozenDownloadPlan, PreparedTorrent, SubmissionPurpose, SubmitPayload,
    TorrentHttpSource, TorrentSource,
};
use store::{DownloadOperationRecord, DownloadOperationState, TransitionDownloadOperationInput};
use uuid::Uuid;

use super::helpers::{SnapshotData, map_store_error, sha256_hex};
use crate::delivery;
use crate::management::ApiState;

pub(super) struct PreparedSubmissionPayload {
    pub(super) submit_payload: SubmitPayload,
    pub(super) info_hash: String,
    pub(super) bytes_digest: Option<String>,
    pub(super) verified_bytes: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct SubmissionFence {
    pub(crate) library_id: Option<String>,
    pub(crate) subscribe_id: Option<domain::SubscribeId>,
    pub(crate) expected_subscribe: Option<domain::Subscribe>,
    pub(crate) control: Option<library::WorkControl>,
}

impl From<&domain::Subscribe> for SubmissionFence {
    fn from(subscribe: &domain::Subscribe) -> Self {
        Self {
            library_id: None,
            subscribe_id: Some(subscribe.id),
            expected_subscribe: Some(subscribe.clone()),
            control: None,
        }
    }
}

pub(crate) fn is_operation_rolled_back(state: &ApiState, op: &DownloadOperationRecord) -> bool {
    if op.state != DownloadOperationState::Submitted {
        return false;
    }
    let Some(dl_id) = op.downloader_id else {
        return false;
    };
    state
        .store
        .get_download_ownership(dl_id, &op.ownership_tag)
        .ok()
        .flatten()
        .is_some_and(|own| {
            own.ownership_state == store::DownloadOwnershipState::Deleted
                && own.operation_id == op.operation_id
        })
}

fn verify_expected_subscribe(
    state: &ApiState,
    purpose: SubmissionPurpose,
    sub_id: domain::SubscribeId,
    expected: &domain::Subscribe,
) -> Result<(), DownloadSubmitError> {
    let guard = state.subscribe_guard(sub_id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(sub_id) {
        return Err(DownloadSubmitError::Cancelled);
    }
    let Some(current) = state.store.get_subscribe(sub_id).map_err(map_store_error)? else {
        return Err(DownloadSubmitError::Cancelled);
    };
    let invalid = if matches!(
        purpose,
        SubmissionPurpose::Admit | SubmissionPurpose::Manual
    ) {
        current.media_id != expected.media_id
            || current.filter_id != expected.filter_id
            || current.downloader_id != expected.downloader_id
            || current.coverage != expected.coverage
            || current.library_id != expected.library_id
    } else {
        crate::management::subscribes::ensure_current(state, expected).is_err()
    };
    if invalid {
        return Err(DownloadSubmitError::Cancelled);
    }
    Ok(())
}

pub(super) fn check_submission_fence(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    fence: Option<&SubmissionFence>,
) -> Result<(), DownloadSubmitError> {
    if fence
        .and_then(|f| f.control.as_ref())
        .is_some_and(|c| c.check().is_err())
    {
        return Err(DownloadSubmitError::Cancelled);
    }
    if let Some(sub_id) = fence.and_then(|f| f.subscribe_id).or(plan.subscribe_id) {
        if state.subscribe_is_deleting(sub_id) {
            return Err(DownloadSubmitError::Cancelled);
        }
        let Some(sub_row) = state.store.get_subscribe(sub_id).map_err(map_store_error)? else {
            return Err(DownloadSubmitError::Cancelled);
        };
        if let Some(expected) = fence.and_then(|f| f.expected_subscribe.as_ref()) {
            verify_expected_subscribe(state, plan.purpose, sub_id, expected)?;
        }
        let lib_id = match fence
            .and_then(|f| f.library_id.clone())
            .or_else(|| sub_row.library_id.map(|id| id.to_string()))
        {
            Some(id) => id,
            None => {
                let Some(media) = state
                    .store
                    .get_media(sub_row.media_id)
                    .map_err(map_store_error)?
                else {
                    return Err(DownloadSubmitError::Cancelled);
                };
                let Some(default_lib) = state
                    .store
                    .default_library(media.kind)
                    .map_err(map_store_error)?
                else {
                    return Err(DownloadSubmitError::Cancelled);
                };
                default_lib.id
            }
        };
        if state.library_work().is_cancelled(&lib_id)
            || !state
                .store
                .library_accepts_work(&lib_id)
                .map_err(map_store_error)?
        {
            return Err(DownloadSubmitError::Cancelled);
        }
    } else if let Some(lib_id) = fence.and_then(|f| f.library_id.as_deref())
        && (state.library_work().is_cancelled(lib_id)
            || !state
                .store
                .library_accepts_work(lib_id)
                .map_err(map_store_error)?)
    {
        return Err(DownloadSubmitError::Cancelled);
    }
    Ok(())
}

pub(super) fn snapshot_inputs(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
) -> Result<(SnapshotData, Option<store::SubscribePolicyBinding>), DownloadSubmitError> {
    let store = state.store.as_ref();
    let subscribe_binding = match plan.subscribe_id {
        Some(sub_id) => match plan.subscribe_generation.zip(plan.policy_revision) {
            Some((gen_id, policy_revision)) => Some(store::SubscribePolicyBinding {
                subscribe_id: sub_id,
                jobs_sync_generation: gen_id.to_string(),
                policy_revision,
            }),
            None => store
                .subscribe_policy_binding(sub_id)
                .map_err(map_store_error)?
                .map(|mut b| {
                    if let Some(rev) = plan.policy_revision {
                        b.policy_revision = rev;
                    }
                    b
                }),
        },
        None => None,
    };
    let site = store
        .get_site(plan.torrent.site_id)
        .map_err(map_store_error)?;
    let downloaders = store.list_downloaders().map_err(map_store_error)?;
    let mut endpoint_by_id = std::collections::HashMap::new();
    let mut all_enabled = Vec::with_capacity(downloaders.len() + 1);
    for d in &downloaders {
        endpoint_by_id.insert(d.id, format!("{:?}:{}", d.kind, d.url));
        if d.enabled {
            all_enabled.push(d.id.to_string());
        }
    }
    let sentinel = DownloaderId::from_uuid(Uuid::nil());
    let pinned_explicit = plan.downloader_id.filter(|explicit| {
        if matches!(
            plan.purpose,
            SubmissionPurpose::Subscribe | SubmissionPurpose::Admit
        ) && plan.subscribe_id.is_some()
        {
            downloaders.iter().any(|d| d.id == *explicit && d.enabled)
        } else {
            true
        }
    });
    let resolved_downloader_id = if let Some(explicit) = pinned_explicit {
        explicit
    } else if let Some(sub_id) = plan.subscribe_id {
        let sub = store.get_subscribe(sub_id).map_err(map_store_error)?;
        delivery::target_id_from_store(store, sub.as_ref(), &plan.torrent)
            .map_err(DownloadSubmitError::Downloader)?
            .unwrap_or(sentinel)
    } else {
        delivery::target_id_from_store(store, None, &plan.torrent)
            .map_err(DownloadSubmitError::Downloader)?
            .unwrap_or(sentinel)
    };
    let downloader_frozen = pinned_explicit.is_some()
        || plan.frozen_endpoint.is_some()
        || plan.purpose == SubmissionPurpose::Plugin;
    let allowed_downloaders = if downloader_frozen {
        vec![resolved_downloader_id.to_string()]
    } else {
        if !all_enabled.contains(&resolved_downloader_id.to_string()) {
            all_enabled.push(resolved_downloader_id.to_string());
        }
        all_enabled
    };
    Ok((
        SnapshotData {
            site_provider: site.as_ref().map(|s| s.profile_id.clone()),
            site,
            allowed_downloaders,
            endpoint_by_id,
            resolved_downloader_id,
        },
        subscribe_binding,
    ))
}

pub(super) fn torrent_fetch_policy(state: &ApiState) -> net_guard::TargetPolicy {
    if let Some(policy) = state.plugin_http_policy_override.clone() {
        return downloader::with_torrent_limits(policy);
    }
    let mut policy = downloader::torrent_target_policy().with_benchmark_fake_ip_domains(true);
    if let Some(self_addr) = state.self_listen {
        policy = policy.with_blocked_self(self_addr);
    }
    policy
}

fn apply_site_torrent_policy(
    state: &ApiState,
    mut policy: net_guard::TargetPolicy,
    site: Option<&domain::Site>,
    target_url: &str,
) -> net_guard::TargetPolicy {
    let Ok(target_norm) = net_guard::normalize_url(target_url, true) else {
        return policy;
    };
    if let Some(s) = site.filter(|s| s.enabled) {
        let (next_policy, matched) = crate::http_fetcher::attach_site_private_exception_for_origin(
            policy,
            &s.url,
            &target_norm.origin,
        );
        policy = next_policy;
        if matched {
            return policy;
        }
    }
    let sites = match state.store.list_enabled_sites() {
        Ok(sites) => sites,
        Err(err) => {
            tracing::warn!(%err, "failed to load enabled sites for torrent fetch policy");
            return policy;
        }
    };
    for enabled_site in &sites {
        let (next_policy, matched) = crate::http_fetcher::attach_site_private_exception_for_origin(
            policy,
            &enabled_site.url,
            &target_norm.origin,
        );
        policy = next_policy;
        if matched {
            break;
        }
    }
    policy
}

pub(super) fn build_site_creds(
    site: &domain::Site,
    origin_url: &str,
    req_cookie: Option<&str>,
    req_api_key: Option<&str>,
    allow_site_fallback: bool,
) -> Option<net_guard::CredentialSlot> {
    let cookie_src = if allow_site_fallback {
        req_cookie.or(site.cookie.as_deref())
    } else {
        req_cookie
    };
    let api_key_src = if allow_site_fallback {
        req_api_key.or(site.api_key.as_deref())
    } else {
        req_api_key
    };
    let mut headers = Vec::new();
    if let Some(cookie) = cookie_src.map(str::trim).filter(|s| !s.is_empty()) {
        headers.push(("Cookie".to_string(), cookie.to_string()));
    }
    if let Some(api_key) = api_key_src.map(str::trim).filter(|s| !s.is_empty()) {
        headers.push(("X-Api-Key".to_string(), api_key.to_string()));
    }
    if headers.is_empty() {
        return None;
    }
    let target = net_guard::normalize_url(origin_url, true).ok()?;
    let epoch_input = headers
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join("|");
    Some(net_guard::CredentialSlot::new(
        site.id.to_string(),
        target.origin,
        sha256_hex(epoch_input.as_bytes()),
        headers,
    ))
}

fn attach_site_transport(src: &mut TorrentSource, site: &domain::Site) {
    if let TorrentSource::Http(http_src) = src {
        if http_src.proxy.is_none() {
            http_src.proxy = site.proxy.clone();
        }
        if http_src.creds.is_none() {
            http_src.creds = build_site_creds(site, &site.url, None, None, true);
        }
        if !http_src
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        {
            http_src.headers.push((
                "User-Agent".to_string(),
                crate::http_fetcher::effective_site_user_agent(),
            ));
        }
    }
}

fn request_headers_for_method(
    method: indexer::FetchMethod,
    mut headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let has_content_type = headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    if !has_content_type {
        match method {
            indexer::FetchMethod::Get => {}
            indexer::FetchMethod::PostJson => {
                headers.push(("Content-Type".to_string(), "application/json".to_string()));
            }
            indexer::FetchMethod::PostForm => {
                headers.push((
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ));
            }
        }
    }
    headers
}

#[cfg(test)]
pub(super) fn resolve_source(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    site: Option<&domain::Site>,
) -> Result<TorrentSource, DownloadSubmitError> {
    resolve_source_at(state, plan, site, None)
}

pub(super) fn resolve_source_at(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    site: Option<&domain::Site>,
    expected_generation: Option<u64>,
) -> Result<TorrentSource, DownloadSubmitError> {
    if matches!(plan.source, TorrentSource::Magnet { .. }) {
        return Ok(plan.source.clone());
    }
    let Some(site) = site else {
        if matches!(plan.source, TorrentSource::PluginRef(_)) {
            return Err(DownloadSubmitError::InvalidPlan(
                "unresolved plugin download_ref without site".into(),
            ));
        }
        return Ok(plan.source.clone());
    };
    let resolved_opt = match expected_generation {
        Some(generation) => {
            state
                .indexer
                .resolve_torrent_download_at(site, &plan.torrent, generation)
        }
        None => state.indexer.resolve_torrent_download(site, &plan.torrent),
    }
    .map_err(|e| match &e {
        _ if e.is_definitive_source_rejection() => DownloadSubmitError::Downloader(
            downloader::DownloaderError::SourceRejected(e.to_string()),
        ),
        indexer::IndexerError::InvalidInput(_)
        | indexer::IndexerError::UnknownProfile(_)
        | indexer::IndexerError::UnsupportedCapability { .. }
        | indexer::IndexerError::Registry(_)
        | indexer::IndexerError::Yaml(_) => DownloadSubmitError::InvalidPlan(e.to_string()),
        _ => DownloadSubmitError::Fetch(downloader::TorrentFetchError::Transient(e.to_string())),
    })?;
    if let Some(resolved) = resolved_opt {
        match resolved {
            indexer::ResolvedDownload::Url(url) => {
                let mut src = TorrentSource::from_url(&url)?;
                attach_site_transport(&mut src, site);
                return Ok(src);
            }
            indexer::ResolvedDownload::Request(req) => {
                let creds = build_site_creds(
                    site,
                    &req.url,
                    req.cookie.as_deref(),
                    req.api_key.as_deref(),
                    false,
                );
                let headers = request_headers_for_method(req.method, Vec::new());
                return Ok(TorrentSource::Http(TorrentHttpSource {
                    url: req.url,
                    method: match req.method {
                        indexer::FetchMethod::Get => net_guard::GuardedMethod::Get,
                        indexer::FetchMethod::PostJson | indexer::FetchMethod::PostForm => {
                            net_guard::GuardedMethod::Post
                        }
                    },
                    headers,
                    body: req.body.map(String::into_bytes),
                    proxy: req.proxy.or_else(|| site.proxy.clone()),
                    creds,
                }));
            }
        }
    }
    if matches!(plan.source, TorrentSource::PluginRef(_)) {
        return Err(DownloadSubmitError::InvalidPlan(
            "unresolved plugin download_ref".into(),
        ));
    }
    let mut src = plan.source.clone();
    attach_site_transport(&mut src, site);
    Ok(src)
}

pub(super) fn fail_intent_operation(
    state: &ApiState,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    outcome_code: &str,
    error_message: String,
    now: i64,
) {
    let _ = state
        .store
        .transition_download_operation(&TransitionDownloadOperationInput {
            operation_id: current_op.operation_id.clone(),
            expected_revision: current_op.revision,
            next_state: DownloadOperationState::Failed,
            downloader_id: Some(frozen_dl_id),
            endpoint_fingerprint,
            info_hash: None,
            bytes_digest: None,
            verified_bytes: None,
            outcome_code: Some(outcome_code.to_string()),
            error_message: Some(error_message),
            submitted_at: None,
            now_utc: now,
        });
}

pub(super) fn validate_and_connect_pre_send_client(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: &Option<String>,
    now: i64,
) -> Result<std::sync::Arc<dyn downloader::Downloader>, DownloadSubmitError> {
    let fail = |code: &str, msg: String| -> DownloadSubmitError {
        fail_intent_operation(
            state,
            current_op,
            frozen_dl_id,
            endpoint_fingerprint.clone(),
            code,
            msg.clone(),
            now,
        );
        DownloadSubmitError::Downloader(downloader::DownloaderError::Message(msg))
    };
    if !frozen_dl_id.as_uuid().is_nil() {
        match state
            .store
            .get_downloader(frozen_dl_id)
            .map_err(map_store_error)?
        {
            Some(row) => {
                if !row.enabled && plan.purpose != SubmissionPurpose::Manual {
                    return Err(fail(
                        "downloader_disabled",
                        "target downloader is disabled".into(),
                    ));
                }
                if plan.frozen_endpoint.is_none()
                    && endpoint_fingerprint
                        .as_deref()
                        .is_some_and(|fp| format!("{:?}:{}", row.kind, row.url) != fp)
                {
                    return Err(fail(
                        "downloader_retargeted",
                        "target downloader endpoint changed during fetch".into(),
                    ));
                }
            }
            None if endpoint_fingerprint.is_some() => {
                return Err(fail(
                    "downloader_deleted",
                    "target downloader was deleted during fetch".into(),
                ));
            }
            None => {}
        }
    } else if plan.purpose != SubmissionPurpose::Manual
        && state
            .store
            .list_downloaders()
            .map_err(map_store_error)?
            .iter()
            .any(|d| d.is_default && !d.enabled)
    {
        return Err(fail(
            "downloader_disabled",
            "default downloader is disabled".into(),
        ));
    }
    delivery::frozen_client_for_id(state, Some(frozen_dl_id))
        .map_err(|err| fail("downloader_connect_failed", err.to_string()))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_and_prepare_payload(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    site: Option<&domain::Site>,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    now: i64,
) -> Result<PreparedSubmissionPayload, DownloadSubmitError> {
    fetch_and_prepare_payload_at(
        state,
        plan,
        site,
        current_op,
        frozen_dl_id,
        endpoint_fingerprint,
        None,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn fetch_and_prepare_payload_at(
    state: &ApiState,
    plan: &FrozenDownloadPlan,
    site: Option<&domain::Site>,
    current_op: &DownloadOperationRecord,
    frozen_dl_id: DownloaderId,
    endpoint_fingerprint: Option<String>,
    expected_generation: Option<u64>,
    now: i64,
) -> Result<PreparedSubmissionPayload, DownloadSubmitError> {
    let source = match resolve_source_at(state, plan, site, expected_generation) {
        Ok(s) => s,
        Err(err) => {
            fail_intent_operation(
                state,
                current_op,
                frozen_dl_id,
                endpoint_fingerprint,
                "resolve_source_failed",
                err.to_string(),
                now,
            );
            return Err(err);
        }
    };
    let mut policy = torrent_fetch_policy(state);
    if state.plugin_http_policy_override.is_none()
        && let TorrentSource::Http(http_src) = &source
    {
        policy = apply_site_torrent_policy(state, policy, site, &http_src.url);
    }
    let prepared = match state
        .torrent_payload_fetcher
        .prepare(&source, plan.purpose, &policy)
    {
        Ok(p) => p,
        Err(err) => {
            fail_intent_operation(
                state,
                current_op,
                frozen_dl_id,
                endpoint_fingerprint,
                "fetch_failed",
                err.to_string(),
                now,
            );
            return Err(DownloadSubmitError::Fetch(err));
        }
    };
    let (submit_payload, info_hash, bytes_digest, verified_bytes) = match prepared {
        PreparedTorrent::TorrentFile(v) => (
            SubmitPayload::TorrentBytes {
                bytes: v.bytes,
                bytes_digest: v.bytes_digest.clone(),
                info_hash: v.info_hash.clone(),
            },
            v.info_hash,
            Some(v.bytes_digest),
            Some(v.verified_total_bytes),
        ),
        PreparedTorrent::Magnet { uri, info_hash } => (
            SubmitPayload::Magnet {
                uri,
                info_hash: info_hash.clone(),
            },
            info_hash,
            None,
            None,
        ),
    };
    Ok(PreparedSubmissionPayload {
        submit_payload,
        info_hash,
        bytes_digest,
        verified_bytes,
    })
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
