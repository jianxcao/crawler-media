use std::path::PathBuf;

use domain::{Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, scan_watch};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

pub fn run(state: &ApiState) -> Result<(), String> {
    let (dirs, nfo) = {
        let store = state.store.lock();
        let mut dirs = Vec::new();
        if let Some(path) = store.watch_inplace().map_err(|e| e.to_string())? {
            dirs.push(PathBuf::from(path));
        }
        // Extra roots attached to default Libraries are still distinct watched
        // roots; only the primary/default root is excluded from this pass.
        for root in store.list_library_roots().map_err(|e| e.to_string())? {
            if !root.is_default {
                dirs.push(root.path);
            }
        }
        let nfo = store
            .get_scrape_config()
            .map_err(|e| e.to_string())?
            .effective
            .mirror_nfo;
        (dirs, nfo)
    };
    for path in dirs {
        scrape_dir(state, path, nfo)?;
    }
    Ok(())
}

fn scrape_dir(state: &ApiState, path: PathBuf, nfo: bool) -> Result<(), String> {
    if path.as_os_str().is_empty() || !path.is_dir() {
        return Ok(());
    }
    let snapshot = state
        .store
        .lock()
        .library_ownership()
        .map_err(|e| e.to_string())?;
    let scope = store::LibraryPathScope::resolve(snapshot).map_err(|e| e.to_string())?;
    let resolved = store::LibraryPathScope::resolve_path(&path).map_err(|e| e.to_string())?;
    let owner = watch_owner(&scope, &resolved)?;
    if owner.is_none()
        && state
            .store
            .lock()
            .path_key_in_removal_scope(&resolved)
            .map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let kind = owner
        .as_ref()
        .map(|owner| owner.kind)
        .unwrap_or(MediaKind::Movie);
    let lease = owner
        .map(|owner| state.acquire_library_work(&owner.id))
        .transpose()?;
    tracing::debug!(dir = %path.display(), "扫描目录，刮削并生成 NFO");
    let probe = Media {
        id: MediaId::new(),
        kind,
        title: "inplace".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = WatchJob {
        kind: WatchKind::InPlace,
        library_root: path.clone(),
        tv_library_root: None,
        path: path.clone(),
        scrape: nfo,
    };
    let mut outcome = scan_watch(&job, &probe).map_err(|e| {
        tracing::error!(dir = %path.display(), error = %e, "目录扫描失败");
        e.to_string()
    })?;
    if !outcome.transferred.is_empty() {
        tracing::info!(
            dir = %path.display(),
            items = outcome.transferred.len(),
            "已记录目录中新增刮削的媒体路径"
        );
    }
    let resolved_files = outcome
        .transferred
        .iter()
        .map(|file| store::LibraryPathScope::resolve_path(&file.path).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    // L1a: plan/observe/record in short batches so the shared Store lock is
    // released between them. Every batch re-validates scope and lease exactly
    // like the former single batch did; each record stays atomic.
    let mut kept = Vec::with_capacity(outcome.transferred.len());
    let mut inserted = Vec::new();
    let files = std::mem::take(&mut outcome.transferred);
    let mut pending = files.into_iter().zip(resolved_files).peekable();
    while pending.peek().is_some() {
        let batch: Vec<_> = pending.by_ref().take(SCRAPE_BATCH).collect();
        let batch_files = {
            let store = state.store.lock();
            validate_scope(&store, &scope)?;
            if !accepts(&store, lease.as_ref())? {
                return Ok(());
            }
            let batch_files: Vec<_> = batch
                .into_iter()
                .filter(|(_, resolved)| match &lease {
                    Some(lease) => matches!(
                        scope.owner(resolved, kind),
                        store::StrictOwner::Owned(owner) if owner.id == lease.library_id()
                    ),
                    None => !store.path_key_in_removal_scope(resolved).unwrap_or(true),
                })
                .map(|(file, _)| file)
                .collect();
            let plan = crate::watch_ledger::plan_paths(&store, batch_files.clone())?;
            (batch_files, plan)
        };
        let (batch_files, plan) = batch_files;
        let observed = plan.observe()?;
        let recorded = {
            let store = state.store.lock();
            validate_scope(&store, &scope)?;
            if !accepts(&store, lease.as_ref())? {
                return Ok(());
            }
            observed.record(&store)?
        };
        kept.extend(batch_files);
        inserted.extend(recorded);
    }
    outcome.transferred = kept;

    resolve_scanned_files(state, &outcome.transferred, &inserted, nfo)?;
    crate::http::library::enqueue_probes_for_paths(state, inserted);
    attach_missing_posters(state, &job.path);
    Ok(())
}

/// Files planned and recorded per Store-lock hold (L1a).
const SCRAPE_BATCH: usize = 64;

/// Lease still live and its Library still accepting work.
fn accepts(
    store: &crate::Store,
    lease: Option<&crate::library_lifecycle::LibraryWorkLease>,
) -> Result<bool, String> {
    let Some(lease) = lease else {
        return Ok(true);
    };
    lease.control().check().map_err(|e| e.to_string())?;
    store
        .library_accepts_work(lease.library_id())
        .map_err(|e| e.to_string())
}

fn validate_scope(store: &crate::Store, scope: &store::LibraryPathScope) -> Result<(), String> {
    let current = store.library_ownership().map_err(|e| e.to_string())?;
    if !scope.matches(&current) {
        return Err("Library Watch scope changed during filesystem observation".into());
    }
    Ok(())
}

fn watch_owner(
    scope: &store::LibraryPathScope,
    path: &std::path::Path,
) -> Result<Option<store::Library>, String> {
    match scope.owner_any(path) {
        store::StrictOwner::Owned(owner) => Ok(Some(owner.clone())),
        store::StrictOwner::Unmatched => Ok(None),
        store::StrictOwner::Ambiguous => Err(format!(
            "Ambiguous Library Watch scope for {}",
            path.display()
        )),
    }
}

fn resolve_scanned_files(
    state: &ApiState,
    transferred: &[library::TransferredFile],
    inserted: &[PathBuf],
    nfo: bool,
) -> Result<(), String> {
    // MovieClaw / MoviePilot 模式：扫描入库后，为没有 TMDB ID 的影视条目自动匹配 TMDB 元数据与海报
    // L1a: one short lock per file instead of one lock across the directory.
    let mut unresolved_media = std::collections::HashMap::new();
    for dest in transferred {
        let media = {
            let store = state.store.lock();
            store
                .file_coverage_by_path(&dest.path.display().to_string())
                .ok()
                .flatten()
                .and_then(|row| store.get_media(row.file.media_id).ok().flatten())
        };
        if let Some(m) = media
            && m.tmdb_id.is_none()
            && m.kind != domain::MediaKind::Video
        {
            unresolved_media
                .entry(m.id)
                .or_insert((m, dest.path.clone()));
        }
    }
    for (_, (media, dest)) in unresolved_media {
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, &dest);
        let active_media = if let Some(canonical) = resolved {
            crate::auto_resolve::rebind_scan_rows(state, media.id, canonical.id, inserted)?;
            canonical
        } else {
            media
        };
        if nfo && active_media.tmdb_id.is_some() {
            let _ = crate::poster_fetch::attach_poster(state, &active_media, &dest);
            let _ = crate::poster_fetch::attach_backdrop(state, &active_media, &dest);
            crate::auto_resolve::write_resolved_nfo(state, &active_media, &dest);
        }
    }
    Ok(())
}

fn attach_missing_posters(state: &ApiState, dir_to_scan: &std::path::Path) {
    // L1a: index range on this directory only; no unit coverage, no full scan.
    let targets = state
        .store
        .lock()
        .file_media_under(dir_to_scan)
        .unwrap_or_default();
    for (media_id, row_path) in targets {
        if row_path
            .parent()
            .is_none_or(|dir| dir.join("poster.jpg").is_file())
        {
            continue;
        }
        let Some(real) = state.store.lock().get_media(media_id).ok().flatten() else {
            continue;
        };
        let _ = crate::poster_fetch::attach_poster(state, &real, &row_path);
        let _ = crate::poster_fetch::attach_backdrop(state, &real, &row_path);
    }
}
