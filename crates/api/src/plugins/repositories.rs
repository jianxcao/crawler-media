use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use domain::UserId;
use net_guard::{
    GuardedMethod, GuardedRequest, NetGuardClient, NetGuardError, Scheme, TargetPolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use store::Store;
use store::plugins::{CanonicalPluginRequest, PluginLifecycle, PluginSubject};
use uuid::Uuid;

use super::config_ops::run_app_write_tx;
use super::idempotency::{
    check_idempotent_replay, check_idempotent_replay_after_write, validate_idempotency_input,
};
use super::service::{PluginService, run_app_read, run_blocking};
use super::staging::{build_package_policy_from_store, stage_upload_sync, uploads_root};
use super::types::PluginServiceError;
use crate::management::ApiState;

pub const OFFICIAL_REPO_ID: &str = "official";
pub const OFFICIAL_REPO_NAME: &str = "crawler-media Official Plugins";
pub const OFFICIAL_REPO_INDEX_URL: &str =
    "https://raw.githubusercontent.com/jianxcao/crawler-media-plugins/main/registry/index.json";

const CUSTOM_REPOS_KEY: &str = "plugin.repositories";
const OFFICIAL_ENABLED_KEY: &str = "plugin.official_repository_enabled";
const REPOS_SAVE_SCOPE: &str = "plugin-repositories-save";
const MAX_INDEX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRepositoryView {
    pub id: String,
    pub name: String,
    pub index_url: String,
    pub enabled: bool,
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRepositoryListView {
    pub repositories: Vec<PluginRepositoryView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRepositoryInput {
    pub id: String,
    pub name: String,
    pub index_url: String,
    pub enabled: bool,
    pub builtin: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCatalogItemView {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub package_url: String,
    pub signature_url: Option<String>,
    pub sha256: String,
    pub capabilities: Vec<String>,
    pub homepage: Option<String>,
    pub installed_version: Option<String>,
    pub lifecycle: Option<PluginLifecycle>,
    pub update_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRepositoryCatalogView {
    pub repo_id: String,
    pub repo_name: String,
    pub plugins: Vec<PluginCatalogItemView>,
}

#[derive(Clone, Debug)]
pub struct SavePluginRepositoriesInput {
    pub actor_user_id: UserId,
    pub repositories: Vec<PluginRepositoryInput>,
    pub idempotency_key: Option<String>,
    pub request: Option<CanonicalPluginRequest>,
}

pub(super) struct RepoStagedPackage {
    pub upload_id: Uuid,
    pub version: String,
}

#[derive(Deserialize)]
struct RawCatalogDocument {
    schema_version: Option<u32>,
    #[serde(default)]
    plugins: Vec<RawCatalogPlugin>,
}

#[derive(Deserialize)]
struct RawCatalogPlugin {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    license: String,
    package_url: String,
    #[serde(default)]
    signature_url: Option<String>,
    sha256: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    homepage: Option<String>,
}

struct ParsedCatalogEntry {
    semver: semver::Version,
    item: PluginCatalogItemView,
}

struct DownloadedArtifacts {
    version: String,
    sha256: String,
    package_bytes: Vec<u8>,
    signature_text: Option<String>,
}

#[derive(Clone, Copy)]
enum UrlErrMode {
    Input,
    Catalog,
}

impl PluginService {
    pub async fn list_repositories(
        &self,
        state: &ApiState,
    ) -> Result<PluginRepositoryListView, PluginServiceError> {
        run_app_read(state, |store| {
            let (official_enabled, custom) = load_repositories_from_store(store)?;
            Ok(PluginRepositoryListView {
                repositories: build_full_repo_list(official_enabled, custom),
            })
        })
        .await
    }

    pub async fn save_repositories(
        &self,
        state: &ApiState,
        input: SavePluginRepositoriesInput,
    ) -> Result<PluginRepositoryListView, PluginServiceError> {
        if let (Some(key), Some(req)) = (&input.idempotency_key, &input.request) {
            validate_idempotency_input(key, req)?;
            if let Some(replayed) = check_idempotent_replay::<PluginRepositoryListView>(
                state,
                input.actor_user_id,
                REPOS_SAVE_SCOPE,
                key,
                req,
            )
            .await?
            {
                return Ok(replayed);
            }
        }
        super::service::require_foreground_write_admitted(state)?;

        let allow_loopback = state.plugin_http_policy_override.is_some();
        let (official_override, custom) =
            validate_save_repositories_input(input.repositories, allow_loopback)?;
        persist_repositories_tx(
            state,
            input.actor_user_id,
            input.idempotency_key,
            input.request,
            official_override,
            custom,
        )
        .await
    }

    pub async fn get_repository_catalog(
        &self,
        state: &ApiState,
        repo_id: &str,
    ) -> Result<PluginRepositoryCatalogView, PluginServiceError> {
        let repo = load_enabled_repository(state, repo_id).await?;
        let policy = build_repo_http_policy(state, MAX_INDEX_BYTES);
        let allow_loopback = state.plugin_http_policy_override.is_some();
        let index_url = repo.index_url.clone();

        let entries = run_blocking(move || {
            let body = fetch_url_bounded_sync(
                &policy,
                &index_url,
                "application/json",
                MAX_INDEX_BYTES,
                "plugin.catalog_too_large",
            )?;
            parse_and_validate_catalog(&body, allow_loopback)
        })
        .await?;

        let installed = run_app_read(state, load_installed_plugin_versions).await?;
        let plugins = entries
            .into_iter()
            .map(|entry| {
                let inst = installed.get(&entry.item.id);
                merge_catalog_item(entry, inst)
            })
            .collect();

        Ok(PluginRepositoryCatalogView {
            repo_id: repo.id,
            repo_name: repo.name,
            plugins,
        })
    }
}

async fn persist_repositories_tx(
    state: &ApiState,
    actor: UserId,
    idempotency_key: Option<String>,
    request: Option<CanonicalPluginRequest>,
    official_override: Option<bool>,
    custom: Vec<PluginRepositoryView>,
) -> Result<PluginRepositoryListView, PluginServiceError> {
    let custom_json =
        serde_json::to_string(&custom).map_err(|e| PluginServiceError::Io(e.to_string()))?;
    let idem_tx = idempotency_key.clone();
    let req_tx = request.clone();
    let tx_res = run_app_write_tx(state, move |app| {
        if let (Some(key), Some(req)) = (&idem_tx, &req_tx)
            && let Some(existing) = app.get_plugin_idempotency_envelope(
                &PluginSubject::User(actor),
                REPOS_SAVE_SCOPE,
                key,
                req,
            )?
        {
            return serde_json::from_value(existing)
                .map_err(|e| PluginServiceError::Io(e.to_string()));
        }
        app.put_setting(CUSTOM_REPOS_KEY, &custom_json)?;
        let official_enabled = match official_override {
            Some(flag) => {
                app.put_setting(OFFICIAL_ENABLED_KEY, if flag { "true" } else { "false" })?;
                flag
            }
            None => parse_official_enabled(app.get_setting(OFFICIAL_ENABLED_KEY)?),
        };
        let view = PluginRepositoryListView {
            repositories: build_full_repo_list(official_enabled, custom),
        };
        if let (Some(key), Some(req)) = (&idem_tx, &req_tx) {
            let json =
                serde_json::to_value(&view).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            app.put_plugin_idempotency_envelope(
                &PluginSubject::User(actor),
                REPOS_SAVE_SCOPE,
                key,
                req,
                &json,
            )?;
        }
        Ok(view)
    })
    .await;
    if let Err(err) = &tx_res
        && matches!(err, PluginServiceError::Conflict { .. })
        && let (Some(key), Some(req)) = (&idempotency_key, &request)
        && let Some(replayed) =
            check_idempotent_replay_after_write(state, actor, REPOS_SAVE_SCOPE, key, req).await?
    {
        return Ok(replayed);
    }
    tx_res
}

pub(super) async fn download_and_stage_from_repo(
    state: &ApiState,
    actor_user_id: UserId,
    repo_id: &str,
    plugin_id: &str,
    requested_version: Option<&str>,
) -> Result<RepoStagedPackage, PluginServiceError> {
    let pid = plugin_id.trim().to_string();
    if pid.is_empty() {
        return Err(invalid_arg(
            "plugin_id is required when installing from a repository",
        ));
    }
    let req_ver = requested_version
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let repo = load_enabled_repository(state, repo_id).await?;
    let allow_loopback = state.plugin_http_policy_override.is_some();
    let policy = build_repo_http_policy(state, MAX_PACKAGE_BYTES);
    let index_url = repo.index_url.clone();

    let downloaded = run_blocking(move || {
        download_repo_artifacts_sync(
            &policy,
            &index_url,
            &pid,
            req_ver.as_deref(),
            allow_loopback,
        )
    })
    .await?;

    let sig_for_policy = downloaded.signature_text.clone();
    let package_policy = run_app_read(state, move |store| {
        build_package_policy_from_store(store, sig_for_policy)
    })
    .await?;

    let data_dir = state.store.data_dir().to_path_buf();
    let upload_id = Uuid::new_v4();
    let version = downloaded.version;
    let expected_sha256 = downloaded.sha256;
    run_blocking(move || {
        stage_upload_sync(
            &data_dir,
            actor_user_id,
            upload_id,
            &downloaded.package_bytes,
            downloaded.signature_text.as_deref(),
            Some(&expected_sha256),
            &package_policy,
        )
    })
    .await?;

    Ok(RepoStagedPackage { upload_id, version })
}

pub(super) async fn cleanup_staged_upload(data_dir: PathBuf, upload_id: Option<Uuid>) {
    let Some(uid) = upload_id else {
        return;
    };
    let dir = uploads_root(&data_dir).join(uid.to_string());
    let _ = tokio::task::spawn_blocking(move || {
        let _ = std::fs::remove_dir_all(dir);
    })
    .await;
}

fn download_repo_artifacts_sync(
    policy: &TargetPolicy,
    index_url: &str,
    plugin_id: &str,
    requested_version: Option<&str>,
    allow_loopback: bool,
) -> Result<DownloadedArtifacts, PluginServiceError> {
    let index_bytes = fetch_url_bounded_sync(
        policy,
        index_url,
        "application/json",
        MAX_INDEX_BYTES,
        "plugin.catalog_too_large",
    )?;
    let entries = parse_and_validate_catalog(&index_bytes, allow_loopback)?;
    let entry = select_catalog_entry(entries, plugin_id, requested_version)?;

    let package_bytes = fetch_url_bounded_sync(
        policy,
        &entry.item.package_url,
        "application/octet-stream",
        MAX_PACKAGE_BYTES,
        "plugin.package_too_large",
    )?;
    if sha256_hex(&package_bytes) != entry.item.sha256 {
        return Err(PluginServiceError::invalid(
            "plugin.invalid_package",
            "downloaded package sha256 mismatch",
        ));
    }

    let signature_text = match &entry.item.signature_url {
        Some(sig_url) => {
            let sig_bytes = fetch_url_bounded_sync(
                policy,
                sig_url,
                "text/plain",
                MAX_SIGNATURE_BYTES,
                "plugin.signature_too_large",
            )?;
            let text = String::from_utf8(sig_bytes).map_err(|_| {
                PluginServiceError::invalid("plugin.invalid_package", "signature must be UTF-8")
            })?;
            Some(text)
        }
        None => None,
    };

    Ok(DownloadedArtifacts {
        version: entry.item.version,
        sha256: entry.item.sha256,
        package_bytes,
        signature_text,
    })
}

fn select_catalog_entry(
    entries: Vec<ParsedCatalogEntry>,
    plugin_id: &str,
    requested_version: Option<&str>,
) -> Result<ParsedCatalogEntry, PluginServiceError> {
    let matches: Vec<_> = entries
        .into_iter()
        .filter(|e| e.item.id == plugin_id)
        .collect();
    if matches.is_empty() {
        return Err(PluginServiceError::not_found(
            "plugin.missing",
            format!("plugin `{plugin_id}` not found in repository catalog"),
        ));
    }
    if let Some(ver) = requested_version {
        let parsed_req = semver::Version::parse(ver).ok();
        return matches
            .into_iter()
            .find(|e| e.item.version == ver || parsed_req.as_ref().is_some_and(|r| &e.semver == r))
            .ok_or_else(|| {
                PluginServiceError::not_found(
                    "plugin.version_missing",
                    format!("version `{ver}` of `{plugin_id}` not found"),
                )
            });
    }
    matches
        .into_iter()
        .max_by(|a, b| a.semver.cmp(&b.semver))
        .ok_or_else(|| PluginServiceError::not_found("plugin.missing", "no matching entry"))
}

async fn load_enabled_repository(
    state: &ApiState,
    repo_id: &str,
) -> Result<PluginRepositoryView, PluginServiceError> {
    let needle = repo_id.to_string();
    let repo = run_app_read(state, move |store| {
        let (official_enabled, custom) = load_repositories_from_store(store)?;
        Ok(build_full_repo_list(official_enabled, custom)
            .into_iter()
            .find(|r| r.id == needle))
    })
    .await?
    .ok_or_else(|| {
        PluginServiceError::not_found(
            "plugin.repository_missing",
            format!("plugin repository `{repo_id}` not found"),
        )
    })?;
    if !repo.enabled {
        return Err(PluginServiceError::conflict(
            "plugin.repository_disabled",
            format!("plugin repository `{repo_id}` is disabled"),
        ));
    }
    Ok(repo)
}

fn parse_official_enabled(raw: Option<String>) -> bool {
    raw.as_deref().map(str::trim) != Some("false")
}

fn load_repositories_from_store(
    store: &Store,
) -> Result<(bool, Vec<PluginRepositoryView>), PluginServiceError> {
    let official_enabled = parse_official_enabled(store.get_setting(OFFICIAL_ENABLED_KEY)?);
    let custom = match store.get_setting(CUSTOM_REPOS_KEY)? {
        Some(raw) if !raw.trim().is_empty() => {
            let mut parsed: Vec<PluginRepositoryView> =
                serde_json::from_str(&raw).map_err(|e| PluginServiceError::Io(e.to_string()))?;
            parsed.retain(|r| r.id != OFFICIAL_REPO_ID);
            parsed.iter_mut().for_each(|r| r.builtin = false);
            parsed
        }
        _ => Vec::new(),
    };
    Ok((official_enabled, custom))
}

fn build_full_repo_list(
    official_enabled: bool,
    mut custom: Vec<PluginRepositoryView>,
) -> Vec<PluginRepositoryView> {
    let mut out = Vec::with_capacity(1 + custom.len());
    out.push(PluginRepositoryView {
        id: OFFICIAL_REPO_ID.to_string(),
        name: OFFICIAL_REPO_NAME.to_string(),
        index_url: OFFICIAL_REPO_INDEX_URL.to_string(),
        enabled: official_enabled,
        builtin: true,
    });
    out.append(&mut custom);
    out
}

fn validate_save_repositories_input(
    items: Vec<PluginRepositoryInput>,
    allow_loopback: bool,
) -> Result<(Option<bool>, Vec<PluginRepositoryView>), PluginServiceError> {
    let mut seen = HashSet::with_capacity(items.len());
    let mut official_enabled = None;
    let mut custom = Vec::new();
    for item in items {
        let id = validate_repo_id(&item.id)?;
        if !seen.insert(id.clone()) {
            return Err(invalid_arg(format!("duplicate repository id `{id}`")));
        }
        let name = item.name.trim();
        if name.is_empty() || name.len() > 128 {
            return Err(invalid_arg("repository name must be 1..=128 characters"));
        }
        if id == OFFICIAL_REPO_ID {
            if name != OFFICIAL_REPO_NAME
                || item.index_url.trim() != OFFICIAL_REPO_INDEX_URL
                || item.builtin == Some(false)
            {
                return Err(invalid_arg(
                    "built-in official repository metadata cannot be altered",
                ));
            }
            official_enabled = Some(item.enabled);
            continue;
        }
        if item.builtin == Some(true) {
            return Err(invalid_arg("custom repositories cannot set builtin=true"));
        }
        let index_url = validate_remote_url(&item.index_url, allow_loopback, UrlErrMode::Input)?;
        custom.push(PluginRepositoryView {
            id,
            name: name.to_string(),
            index_url,
            enabled: item.enabled,
            builtin: false,
        });
    }
    Ok((official_enabled, custom))
}

fn validate_repo_id(raw: &str) -> Result<String, PluginServiceError> {
    if raw.is_empty() || raw.len() > 64 || raw != raw.trim() {
        return Err(invalid_arg("repository id must be 1..=64 chars"));
    }
    let mut bytes = raw.bytes();
    let first_ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let rest_ok =
        bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !first_ok || !rest_ok {
        return Err(invalid_arg(
            "repository id must match ^[a-z0-9][a-z0-9_-]*$",
        ));
    }
    Ok(raw.to_string())
}

fn validate_remote_url(
    raw: &str,
    allow_test_loopback_http: bool,
    mode: UrlErrMode,
) -> Result<String, PluginServiceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(url_err(mode, "URL must not be empty"));
    }
    let target = net_guard::normalize_url(trimmed, false)
        .map_err(|e| url_err(mode, format!("invalid URL `{trimmed}`: {e}")))?;
    match target.origin.scheme {
        Scheme::Https => {
            if !allow_test_loopback_http
                && target
                    .host_is_ip
                    .is_some_and(|ip| !net_guard::is_global_unicast(net_guard::canonicalize_ip(ip)))
            {
                return Err(url_err(mode, "non-public IP literal is not permitted"));
            }
        }
        Scheme::Http => {
            let ok = allow_test_loopback_http
                && target.host_is_ip.is_some_and(|ip| {
                    matches!(
                        net_guard::canonicalize_ip(ip),
                        IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
                    )
                });
            if !ok {
                return Err(url_err(mode, "repository URLs must use https://"));
            }
        }
    }
    Ok(target.url.to_string())
}

fn invalid_arg(msg: impl Into<String>) -> PluginServiceError {
    PluginServiceError::invalid("plugin.invalid_input", msg)
}

fn invalid_cat(msg: impl Into<String>) -> PluginServiceError {
    PluginServiceError::Unprocessable {
        code: "plugin.invalid_catalog",
        message: msg.into(),
    }
}

fn url_err(mode: UrlErrMode, msg: impl Into<String>) -> PluginServiceError {
    match mode {
        UrlErrMode::Input => invalid_arg(msg),
        UrlErrMode::Catalog => invalid_cat(msg),
    }
}

pub(crate) fn build_repo_http_policy(state: &ApiState, max_bytes: u64) -> TargetPolicy {
    let mut policy = match &state.plugin_http_policy_override {
        Some(p) => p.clone(),
        None => {
            let mut p = TargetPolicy::production().with_benchmark_fake_ip_domains(true);
            if let Some(self_addr) = state.self_listen {
                p = p.with_blocked_self(self_addr);
            }
            p
        }
    };
    policy.limits.max_response_bytes = policy.limits.max_response_bytes.max(max_bytes);
    policy
}

fn fetch_url_bounded_sync(
    policy: &TargetPolicy,
    url: &str,
    accept: &str,
    max_bytes: u64,
    oversized_code: &'static str,
) -> Result<Vec<u8>, PluginServiceError> {
    let req = GuardedRequest {
        method: GuardedMethod::Get,
        url: url.to_string(),
        headers: vec![
            ("accept".into(), accept.into()),
            ("user-agent".into(), "crawler-media/1.0".into()),
        ],
        body: None,
        proxy: None,
        max_response_bytes: Some(max_bytes),
    };
    let resp = NetGuardClient::new()
        .execute(req, policy, None)
        .map_err(|e| map_net_guard_err(&e, oversized_code))?;
    if !resp.is_success() {
        return Err(PluginServiceError::Unprocessable {
            code: "plugin.repository_fetch_failed",
            message: format!("remote repository returned HTTP {}", resp.status),
        });
    }
    Ok(resp.body)
}

fn map_net_guard_err(err: &NetGuardError, oversized_code: &'static str) -> PluginServiceError {
    match err.cause() {
        NetGuardError::BodyTooLarge { limit } => PluginServiceError::PayloadTooLarge {
            code: oversized_code,
            message: format!("remote response exceeds {limit} byte limit"),
        },
        NetGuardError::InvalidUrl(msg) | NetGuardError::BlockedAddress(msg) => {
            invalid_arg(msg.clone())
        }
        NetGuardError::HttpsDowngradeBlocked => invalid_arg("https to http downgrade blocked"),
        other => PluginServiceError::Unprocessable {
            code: "plugin.repository_fetch_failed",
            message: other.to_string(),
        },
    }
}

fn parse_and_validate_catalog(
    bytes: &[u8],
    allow_loopback: bool,
) -> Result<Vec<ParsedCatalogEntry>, PluginServiceError> {
    let doc: RawCatalogDocument =
        serde_json::from_slice(bytes).map_err(|e| invalid_cat(format!("invalid catalog: {e}")))?;
    if doc.schema_version != Some(1) {
        return Err(invalid_cat("catalog schema_version must be 1"));
    }
    doc.plugins
        .into_iter()
        .map(|raw| validate_catalog_entry(raw, allow_loopback))
        .collect()
}

fn validate_catalog_entry(
    raw: RawCatalogPlugin,
    allow_loopback: bool,
) -> Result<ParsedCatalogEntry, PluginServiceError> {
    let (id, name, version) = (
        raw.id.trim().to_string(),
        raw.name.trim().to_string(),
        raw.version.trim().to_string(),
    );
    if id.is_empty() || name.is_empty() {
        return Err(invalid_cat("catalog entry missing id or name"));
    }
    let semver = semver::Version::parse(&version)
        .map_err(|e| invalid_cat(format!("invalid version `{version}`: {e}")))?;
    let sha256 = raw.sha256.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid_cat(format!("invalid sha256 digest for `{id}`")));
    }
    let package_url = validate_remote_url(&raw.package_url, allow_loopback, UrlErrMode::Catalog)?;
    let signature_url = match raw.signature_url.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => {
            Some(validate_remote_url(s, allow_loopback, UrlErrMode::Catalog)?)
        }
        _ => None,
    };
    let homepage = raw
        .homepage
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());

    Ok(ParsedCatalogEntry {
        semver,
        item: PluginCatalogItemView {
            id,
            name,
            version,
            description: raw.description,
            authors: raw.authors,
            license: raw.license,
            package_url,
            signature_url,
            sha256,
            capabilities: raw.capabilities,
            homepage,
            installed_version: None,
            lifecycle: None,
            update_available: false,
        },
    })
}

fn load_installed_plugin_versions(
    store: &Store,
) -> Result<HashMap<String, (Option<String>, PluginLifecycle)>, PluginServiceError> {
    let records = PluginService::list_all_plugins(store, None)?;
    let mut map = HashMap::with_capacity(records.len());
    for rec in records {
        let ver = store
            .get_plugin_version(&rec.plugin_id, &rec.package_hash)?
            .map(|m| m.version);
        map.insert(rec.plugin_id, (ver, rec.lifecycle));
    }
    Ok(map)
}

fn merge_catalog_item(
    mut entry: ParsedCatalogEntry,
    installed: Option<&(Option<String>, PluginLifecycle)>,
) -> PluginCatalogItemView {
    if let Some((ver_opt, lc)) = installed {
        entry.item.installed_version = ver_opt.clone();
        entry.item.lifecycle = Some(*lc);
        entry.item.update_available = ver_opt
            .as_deref()
            .and_then(|v| semver::Version::parse(v).ok())
            .is_some_and(|cur| entry.semver > cur);
    }
    entry.item
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
#[path = "repositories_tests.rs"]
mod tests;
