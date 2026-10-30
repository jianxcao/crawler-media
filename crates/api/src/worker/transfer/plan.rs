use domain::{Media, Subscribe};

use crate::management::ApiState;
use crate::scrape_config::{ScrapeConfigSetting, compose_config};

pub(crate) struct TransferPlan {
    pub(crate) library_id: String,
    library: store::Library,
    transfer_settings: store::TransferSettings,
    scrape_setting: Option<String>,
    pub(crate) fingerprint: store::ReadFingerprint,
    pub(crate) root: std::path::PathBuf,
    /// Root of `library_id` that owns `root`: the destination identity every
    /// publication of this plan must carry.
    pub(crate) root_id: String,
    /// `libraries.config_revision` when the plan was built.
    pub(crate) config_revision: i64,
    pub(crate) mode: Option<library::TransferMode>,
    pub(crate) naming: String,
    pub(crate) scrape: bool,
    pub(crate) nfo: bool,
    pub(super) published_removals: parking_lot::Mutex<
        std::collections::HashMap<std::path::PathBuf, super::removals::TransferRemovalPin>,
    >,
}

/// The enabled root of `library_id` that contains `destination`.
///
/// The deepest containing root wins so a nested root is never shadowed by its
/// parent. A destination no enabled root contains, or one two equally deep
/// roots contain, is an error: publishing a file under a root the Library does
/// not declare would record a file row nobody owns.
fn destination_root_id(
    store: &store::LibraryConnection<'_>,
    library_id: &str,
    destination: &std::path::Path,
) -> Result<String, String> {
    let mut best: Option<(usize, String)> = None;
    let mut ambiguous = false;
    for root in store
        .list_library_roots()
        .map_err(|error| error.to_string())?
    {
        if root.library_id.as_str() != library_id || !root.enabled {
            continue;
        }
        if destination.strip_prefix(&root.path).is_err() {
            continue;
        }
        let depth = root.path.components().count();
        match &best {
            Some((best_depth, _)) if *best_depth > depth => {}
            Some((best_depth, _)) if *best_depth == depth => ambiguous = true,
            _ => {
                ambiguous = false;
                best = Some((depth, root.id));
            }
        }
    }
    match (best, ambiguous) {
        (Some((_, id)), false) => Ok(id),
        (_, true) => {
            let error = format!(
                "Library {library_id} 有多个根目录包含 {}（无法确定目标根目录）",
                destination.display()
            );
            tracing::error!(library_id, destination = %destination.display(), "{error}");
            Err(error)
        }
        (None, _) => {
            let error = format!(
                "Library {library_id} 没有包含 {} 的启用根目录",
                destination.display()
            );
            tracing::error!(library_id, destination = %destination.display(), "{error}");
            Err(error)
        }
    }
}

pub(crate) fn resolve_transfer_plan(
    state: &ApiState,
    media: &Media,
    subscribe: &Subscribe,
) -> Result<TransferPlan, String> {
    let store = state.store.as_ref();
    let library = match subscribe.library_id {
        Some(id) => store.get_library(&id.to_string()),
        None => store.default_library(media.kind),
    }
    .map_err(|e| e.to_string())?
    .ok_or("目标 Library 不存在")?;
    if !store
        .library_accepts_work(&library.id)
        .map_err(|error| error.to_string())?
    {
        return Err("目标 Library 正在删除".into());
    }
    let root = library
        .root_paths
        .first()
        .cloned()
        .ok_or("目标 Library 没有根目录")?;
    let root_id = store
        .library_read_snapshot(|db| destination_root_id(db, &library.id, &root))
        .map_err(|error| error.to_string())??;
    let config_revision = store
        .library_config_revision(&library.id)
        .map_err(|error| error.to_string())?;
    let transfer_settings = store
        .transfer_settings()
        .map_err(|error| error.to_string())?;
    let mode = crate::directory::parse_transfer_mode(&transfer_settings.transfer_mode);
    let scrape = transfer_settings.scrape;
    let scrape_setting = store
        .get_setting(crate::scrape_config::KEY)
        .map_err(|error| error.to_string())?;
    let setting = match scrape_setting.as_deref() {
        Some(raw) => serde_json::from_str::<ScrapeConfigSetting>(raw).unwrap_or_else(|error| {
            tracing::error!(%error, "Transfer scrape configuration is corrupt; using defaults");
            ScrapeConfigSetting::default()
        }),
        None => ScrapeConfigSetting::default(),
    };
    let cfg = compose_config(setting);
    let naming = match media.kind {
        domain::MediaKind::Tv => cfg.effective.compose_tv_pattern(),
        _ => cfg.effective.compose_movie_pattern(),
    };
    let fingerprint = store
        .read_fingerprint(&library.id, media.id)
        .map_err(|e| e.to_string())?;
    Ok(TransferPlan {
        library_id: library.id.clone(),
        library,
        transfer_settings,
        scrape_setting,
        fingerprint,
        root,
        root_id,
        config_revision,
        mode,
        naming,
        scrape,
        nfo: scrape && cfg.effective.mirror_nfo,
        published_removals: parking_lot::Mutex::default(),
    })
}

pub(crate) fn validate_transfer_plan(
    store: &crate::Store,
    subscribe: &Subscribe,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    fingerprint: &store::ReadFingerprint,
    plan: &TransferPlan,
) -> Result<(), String> {
    validate_transfer_plan_at(
        store,
        subscribe,
        lease,
        Some(fingerprint),
        &std::collections::HashSet::new(),
        plan,
    )
}

/// Compare two scoped fingerprints, ignoring file rows this round itself has
/// already reserved/published. Those rows are the round's own effect, not an
/// external change.
fn fingerprint_matches(
    current: &store::ReadFingerprint,
    expected: &store::ReadFingerprint,
    ignored_files: &std::collections::HashSet<String>,
) -> bool {
    current.library_id == expected.library_id
        && current.media_id == expected.media_id
        && current.config_revision == expected.config_revision
        && current
            .files
            .iter()
            .filter(|file| !ignored_files.contains(&file.file_id))
            .eq(expected
                .files
                .iter()
                .filter(|file| !ignored_files.contains(&file.file_id)))
}

/// Commit-time validation.
///
/// The transfer's own `publish_files` writes legitimately advance the scoped
/// file fingerprint, so re-reading it after publication can never match the
/// pre-publication snapshot. The fingerprint was already compared before every
/// publication reservation; here only the parts of the plan that publication
/// does not touch (lease, Library, roots, transfer settings, Subscribe) are
/// re-checked.
pub(crate) fn validate_committed_transfer_plan(
    store: &crate::Store,
    subscribe: &Subscribe,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    plan: &TransferPlan,
) -> Result<(), String> {
    validate_transfer_plan_at(
        store,
        subscribe,
        lease,
        None,
        &std::collections::HashSet::new(),
        plan,
    )
}

pub(super) fn validate_transfer_plan_at(
    store: &crate::Store,
    subscribe: &Subscribe,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    fingerprint: Option<&store::ReadFingerprint>,
    ignored_files: &std::collections::HashSet<String>,
    plan: &TransferPlan,
) -> Result<(), String> {
    validate_app_plan(store, plan)?;
    if store
        .get_subscribe(subscribe.id)
        .map_err(|error| error.to_string())?
        .as_ref()
        != Some(subscribe)
    {
        return Err("Transfer Subscribe changed".into());
    }
    store
        .library_read_snapshot(|db| {
            validate_library_plan(db, lease, fingerprint, ignored_files, plan)
        })
        .map_err(|error| error.to_string())?
}

fn validate_app_plan(store: &store::Store, plan: &TransferPlan) -> Result<(), String> {
    if store
        .transfer_settings()
        .map_err(|error| error.to_string())?
        != plan.transfer_settings
        || store
            .get_setting(crate::scrape_config::KEY)
            .map_err(|error| error.to_string())?
            != plan.scrape_setting
    {
        return Err("Transfer configuration changed".into());
    }
    Ok(())
}

/// App configuration cannot change between admission and the Library write.
/// Both writers are held only for SQL and acquired in App → Library order.
pub(crate) fn with_plan_write<T>(
    store: &store::Store,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    fingerprint: Option<&store::ReadFingerprint>,
    ignored_files: &std::collections::HashSet<String>,
    plan: &TransferPlan,
    work: impl FnOnce(&store::LibraryConnection<'_>) -> Result<T, String>,
) -> Result<T, String> {
    store
        .writers()
        .with_app(|app| {
            if app.transfer_settings()? != plan.transfer_settings
                || app.get_setting(crate::scrape_config::KEY)? != plan.scrape_setting
            {
                return Err(store::StoreError::Protected(
                    "Transfer configuration changed".into(),
                ));
            }
            store.library_write_with_error(std::convert::identity, |db| {
                validate_library_plan(db, lease, fingerprint, ignored_files, plan)
                    .map_err(store::StoreError::Protected)?;
                work(db).map_err(store::StoreError::Protected)
            })
        })
        .map_err(|error| error.to_string())
}

fn validate_library_plan(
    store: &store::LibraryConnection<'_>,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    fingerprint: Option<&store::ReadFingerprint>,
    ignored_files: &std::collections::HashSet<String>,
    plan: &TransferPlan,
) -> Result<(), String> {
    lease.control().check().map_err(|e| e.to_string())?;
    if !store
        .library_accepts_work(lease.library_id())
        .map_err(|error| error.to_string())?
    {
        return Err("目标 Library 正在删除或已不存在".into());
    }
    let fingerprint_changed = match fingerprint {
        Some(fingerprint) => {
            let current = store
                .read_fingerprint(&plan.library_id, plan.fingerprint.media_id)
                .map_err(|error| error.to_string())?;
            !fingerprint_matches(&current, fingerprint, ignored_files)
                || !fingerprint_matches(&current, &plan.fingerprint, ignored_files)
        }
        None => false,
    };
    let library = store
        .get_library(&plan.library_id)
        .map_err(|error| error.to_string())?;
    let root_id = destination_root_id(store, &plan.library_id, &plan.root)?;
    let config_revision = store
        .library_config_revision(&plan.library_id)
        .map_err(|error| error.to_string())?;
    if fingerprint_changed
        || lease.library_id() != plan.library_id
        || library.as_ref() != Some(&plan.library)
        || root_id != plan.root_id
        || config_revision != plan.config_revision
    {
        tracing::warn!(library_id = %plan.library_id, "Transfer source or Library configuration changed");
        return Err("Transfer source or Library configuration changed".into());
    }
    Ok(())
}
