use domain::{FileCoverage, LedgerId, Media, Subscribe, SubscribeId};
use filter::ScoredTorrent;
use store::StoreError;
use store::library_write::{FilePublication as PublicationRequest, WriteScope};
use subscribe::{Added, RunInput, RunOutcome, SubscribeFacts, collect_completed_recorded};

use crate::management::ApiState;

use super::plan::{TransferPlan, resolve_transfer_plan, validate_committed_transfer_plan};
use super::recorder::{PlannedPublicationRecorder, PublicationRecorder};

fn attach_metadata_and_chapters(state: &ApiState, media: &Media, files: &[FileCoverage]) {
    for coverage in files {
        let path = coverage.file.path.as_path();
        crate::poster_fetch::attach_poster(state, media, path);
        crate::poster_fetch::attach_backdrop(state, media, path);
        crate::http::library_chapters::auto_generate_chapters(state, path);
    }
}

/// The transfer mode a placed file actually ended up with: a file that shares an
/// inode with its download source came in as a hardlink, everything else kept the
/// configured mode. `library_files.transfer_mode` only accepts the three real
/// transfer modes, so the observation never invents a fourth one.
pub(crate) fn observe_transfer_modes(files: &[FileCoverage], default_mode: &str) -> Vec<String> {
    files
        .iter()
        .map(|coverage| {
            let is_hardlink = std::fs::metadata(&coverage.file.path)
                .map(|meta| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        meta.nlink() > 1
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                })
                .unwrap_or(false);
            if is_hardlink {
                "hardlink".to_string()
            } else {
                default_mode.to_string()
            }
        })
        .collect()
}

/// The download source this round recorded for a published file, if any. The
/// outcome owns the mapping, keyed by the file identity the reservation minted.
pub(crate) fn recorded_file_source(
    outcome: &RunOutcome,
    file_id: LedgerId,
) -> Option<&std::path::Path> {
    outcome
        .file_sources
        .iter()
        .find(|source| source.file_id == file_id)
        .map(|source| source.source.as_path())
}

/// The Subscribe slots a published file covers, as `mark_wanted_imported`
/// spells them: a whole-work unit is `(None, None)` (its `wanted_key` maps both
/// to `(-1, -1)`), an episode unit is its own `(season, episode)`.
fn wanted_slots(units: &[domain::UnitKey]) -> Vec<(Option<u32>, Option<u32>)> {
    units
        .iter()
        .map(|unit| {
            if unit.is_whole() {
                (None, None)
            } else {
                (Some(unit.season as u32), Some(unit.episode as u32))
            }
        })
        .collect()
}

/// Publish one reserved Transfer file: the reservation owns the file identity,
/// so this is a version-checked publish of the confirmed coverage of that same
/// file, never a second identity creation. The transfer mode a placed file
/// actually ended up with (a hardlink is only visible here), its source path and
/// the sidecar artwork beside it are written with the file.
pub(crate) fn publish_transfer_file(
    store: &store::Store,
    library_id: &str,
    config_revision: i64,
    coverage: &FileCoverage,
    source_path: Option<&std::path::Path>,
    mode: &str,
    observed_source: Option<&library::SourceObservation>,
) -> Result<domain::LibraryFile, StoreError> {
    let file_id = coverage.file.file_id;
    let observed = &coverage.file.version;
    let committed = store.get_file_version(file_id)?.ok_or_else(|| {
        StoreError::Protected("Transfer reservation changed or was detached".into())
    })?;
    if committed.revision != observed.revision
        || committed.source_version != observed.source_version
    {
        return Err(StoreError::Protected(
            "Transfer reservation changed or was detached".into(),
        ));
    }
    // The reservation already wrote this file's units; the collected coverage is
    // the fallback so a span is never narrowed back to its first episode.
    let stored_units = store.get_file_units(file_id)?;
    let units: Vec<domain::UnitKey> = if !stored_units.is_empty() {
        stored_units
    } else if !coverage.units.is_empty() {
        coverage.units.clone()
    } else {
        vec![domain::UnitKey::WHOLE]
    };
    let artwork = store::files::DiscoveredArtwork::discover(&coverage.file.path);
    let mut file = coverage.file.clone();
    file.presence = domain::FilePresence::Present;
    file.publication = domain::FilePublication::Published;
    file.source_path = source_path.map(std::path::PathBuf::from);
    file.transfer_mode = Some(mode.to_string());
    file.poster_path = artwork.poster;
    file.backdrop_path = artwork.backdrop;
    file.still_path = artwork.still;
    store.publish_files(
        &WriteScope {
            library_id: library_id.parse::<domain::LibraryId>()?,
            config_revision,
        },
        &[PublicationRequest {
            file: &file,
            units: &units,
            expected_version: Some(observed),
            nfo_observation: None,
            probe_required: false,
        }],
        store::unix_now() * 1000,
    )?;
    // The reservation version is a placeholder for bytes that did not exist yet;
    // the source observed (outside the Store lock) after placement advances it to
    // the real production encoding by CAS, so Probe/fingerprints bind real bytes.
    if let Some(observation) = observed_source
        && observation.source_version != observed.source_version
    {
        file.version = store.bind_placed_file_source(
            &WriteScope {
                library_id: library_id.parse::<domain::LibraryId>()?,
                config_revision,
            },
            file_id,
            observed,
            observation,
            store::unix_now() * 1000,
        )?;
    }
    tracing::info!(file_id = %file_id, path = %coverage.file.path.display(), "Published Transfer file");
    Ok(file)
}

/// Observe each placed Transfer file's source outside the Store lock.
pub(crate) fn observe_transfer_sources(
    files: &[FileCoverage],
) -> Vec<Option<library::SourceObservation>> {
    files
        .iter()
        .map(|coverage| crate::fingerprint_job::observe_source(&coverage.file.path))
        .collect()
}

fn commit_transfer_outcome(
    state: &ApiState,
    subscribe: &Subscribe,
    outcome: &RunOutcome,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    plan: &TransferPlan,
) -> Result<Vec<domain::LibraryFile>, String> {
    let default_transfer_mode = {
        let store = state.store.lock();
        store.transfer_mode().unwrap_or_else(|_| "copy".into())
    };

    // Observe transfer modes outside Store lock
    let observed_modes = observe_transfer_modes(&outcome.files, &default_transfer_mode);
    let observed_sources = observe_transfer_sources(&outcome.files);

    // Commit replacement file/source mapping and facts in a short Store section
    let mut published = Vec::with_capacity(outcome.files.len());
    {
        let store = state.store.lock();
        // Publication already validated the pre-publication fingerprint before
        // each reservation; the scoped fingerprint now reflects this round's
        // own published rows, so only the untouched plan parts are re-checked.
        validate_committed_transfer_plan(&store, subscribe, lease, plan)?;
        for ((coverage, mode), observed_source) in outcome
            .files
            .iter()
            .zip(observed_modes.iter())
            .zip(observed_sources.iter())
        {
            let path = coverage.file.path.to_string_lossy().to_string();
            let source_path = recorded_file_source(outcome, coverage.file.file_id)
                .map(|source| source.display().to_string());
            let existing = store
                .file_coverage_by_path(&path)
                .map_err(|e| e.to_string())?;
            if existing.as_ref().map(|existing| {
                (
                    existing.file.file_id,
                    existing.file.media_id,
                    existing.file.path.to_string_lossy().into_owned(),
                )
            }) != Some((coverage.file.file_id, coverage.file.media_id, path.clone()))
                || !store
                    .ledger_source_path_matches(&path, source_path.as_deref())
                    .map_err(|e| e.to_string())?
            {
                return Err(format!("Transfer reservation missing or replaced: {path}"));
            }
            published.push(
                publish_transfer_file(
                    &store,
                    &plan.library_id,
                    plan.config_revision,
                    coverage,
                    source_path.as_deref().map(std::path::Path::new),
                    mode,
                    observed_source.as_ref(),
                )
                .map_err(|e| e.to_string())?,
            );
        }
        for path in &outcome.removed_paths {
            if store
                .file_coverage_by_path(path)
                .map_err(|e| e.to_string())?
                .is_some_and(|existing| existing.file.media_id != subscribe.media_id)
            {
                return Err("待替换路径已被其他媒体占用，放弃本轮提交".into());
            }
        }

        store
            .save_subscribe_facts(subscribe.id, &outcome.facts)
            .map_err(|e| e.to_string())?;

        let now = crate::job_loop::unix_now();
        for coverage in &outcome.files {
            for (season, episode) in wanted_slots(&coverage.units) {
                store
                    .mark_wanted_imported(subscribe.id, season, episode, now)
                    .map_err(|e| e.to_string())?;
            }
        }
        store
            .mark_pending_imported(subscribe.id, &outcome.transferred_enclosures)
            .map_err(|e| e.to_string())?;
        for coverage in &outcome.files {
            let path = coverage.file.path.to_string_lossy().to_string();
            store.clear_ledger_missing(&path).map_err(|error| {
                tracing::error!(path = %path, %error, "清理 Library 缺失标记失败");
                error.to_string()
            })?;
        }
    }

    // Review N-1: capped wait for a source probe; exhausted waits are rejected.
    let exhausted = crate::probe_wait::exhausted_probe_waits(
        state,
        subscribe.id,
        &outcome.awaiting_probe_enclosures,
    );
    // M6: queue replaced files instead of deleting inline; a failed delete
    // must not abort the commit and is retried later (outside the Store lock).
    {
        let store = state.store.lock();
        for path in &outcome.removed_paths {
            store
                .enqueue_file_deletion(path, "wash_replaced")
                .map_err(|e| e.to_string())?;
        }
        store
            .mark_pending_rejected(subscribe.id, &outcome.rejected_enclosures)
            .map_err(|e| e.to_string())?;
        store
            .mark_pending_rejected(subscribe.id, &exhausted)
            .map_err(|e| e.to_string())?;
    }
    crate::http::file_delete::retry_queued_deletions(&state.store).map_err(|e| e.to_string())?;
    Ok(published)
}

fn load_transfer_context(
    state: &ApiState,
    subscribe: &Subscribe,
) -> Result<
    (
        Media,
        domain::Filter,
        SubscribeFacts,
        Option<domain::Filter>,
        store::ReadFingerprint,
    ),
    String,
> {
    let (media, filter, wash_filter) = {
        let store = state.store.lock();
        let media = store
            .get_media(subscribe.media_id)
            .map_err(|e| e.to_string())?
            .ok_or("Media not found")?;
        let filter = store
            .get_filter(subscribe.filter_id)
            .map_err(|e| e.to_string())?
            .ok_or("Filter not found")?;
        let wash_filter = subscribe
            .wash_cut_filter_id
            .map(|fid| store.get_filter(fid))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten()
            .or_else(|| store.get_filter(subscribe.filter_id).ok().flatten());
        (media, filter, wash_filter)
    };
    let obs = crate::subscribe_library_read::observe_subscribe_library_facts(
        state, subscribe, media.kind,
    )?;
    Ok((media, filter, obs.facts, wash_filter, obs.fingerprint))
}

/// D-M3: hand persisted info hashes back to the (possibly fresh) clients so
/// label-less Transmission tasks stay collectable across restarts.
fn restore_identities(
    state: &ApiState,
    subscribe: &Subscribe,
    routed: &crate::delivery::RoutedDownloader<'_>,
    pending: &[(i32, crate::store::PendingDownload)],
) {
    let hashes = match state.store.lock().load_pending_info_hashes(subscribe.id) {
        Ok(hashes) => hashes,
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "读取 pending info hash 失败");
            return;
        }
    };
    for (_, item) in pending {
        if let Some(hash) = hashes.get(&item.torrent.enclosure) {
            use downloader::Downloader as _;
            if let Err(error) = routed.remember_identity(&item.torrent, hash) {
                tracing::warn!(torrent = %item.torrent.title, %error, "恢复下载器任务身份失败");
            }
        }
    }
}

fn filter_matched_pending(
    subscribe: &Subscribe,
    media: &Media,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    absolute: Option<&subscribe::AbsoluteEpisodes>,
) -> Vec<ScoredTorrent> {
    pending
        .into_iter()
        .filter_map(|(score, pending)| {
            let release = pending
                .release_override
                .unwrap_or_else(|| release::parse(&pending.torrent.title));
            // G-H2b: absolute episode numbers → catalog season + episode.
            let release = absolute.map_or_else(|| release.clone(), |map| map.apply(&release));
            if !subscribe::candidate_matches_subscribe(subscribe, media, &release) {
                tracing::error!(subscribe_id = %subscribe.id, media = %media.title,
                    torrent = %pending.torrent.title, "拒绝转存与订阅不匹配的待处理种子");
                return None;
            }
            Some(ScoredTorrent {
                release,
                torrent: pending.torrent,
                score,
            })
        })
        .collect()
}

fn build_added_input<'a>(
    input: RunInput<'a, crate::delivery::RoutedDownloader<'a>>,
    chosen: Vec<ScoredTorrent>,
) -> Added<'a, crate::delivery::RoutedDownloader<'a>> {
    Added {
        input,
        chosen,
        add_errors: Vec::new(),
        rejected: Vec::new(),
        outcome: subscribe::RunOutcome {
            facts: Default::default(),
            files: Vec::new(),
            file_sources: Vec::new(),
            removed_paths: Vec::new(),
            completed: false,
            transferred_enclosures: Vec::new(),
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: Vec::new(),
            submission_errors: Vec::new(),
            torrents_added: Vec::new(),
        },
        broken: Vec::new(),
    }
}

/// Join existing Library work before observing facts or acquiring our own lease.
/// In particular, a first scan must not claim a just-published Transfer file while
/// probing is still running and before its authoritative ledger row is committed.
fn wait_for_transfer_library(state: &ApiState, library_id: &str) -> Result<bool, String> {
    let started = std::time::Instant::now();
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
    // Match the Library lifecycle drain budget; never hold Store while waiting.
    let idle = runtime.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            state.wait_library_idle(library_id),
        )
        .await
        .is_ok()
    });
    if !idle {
        tracing::warn!(
            library_id,
            elapsed_ms = started.elapsed().as_millis(),
            "Library 工作尚未结束，保留待处理下载供下一轮 Transfer 重试"
        );
    }
    Ok(idle)
}

/// Load this subscription's pending rows, releasing tasks that provably
/// vanished from the client. `None` means there is nothing to transfer.
fn load_pending_transfer(
    state: &ApiState,
    subscribe_id: SubscribeId,
) -> Result<Option<(Subscribe, Vec<(i32, crate::store::PendingDownload)>)>, String> {
    let subscribe = match state.store.lock().get_subscribe(subscribe_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::info!(subscribe_id = %subscribe_id, "订阅已不存在，跳过 Transfer");
            return Ok(None);
        }
        Err(e) => return Err(e.to_string()),
    };

    let pending = state
        .store
        .lock()
        .load_pending(subscribe.id)
        .map_err(|e| e.to_string())?;
    // H3: release rows whose task provably vanished from the client, so a
    // slot never stays blocked just because nobody opened the task center.
    let pending = crate::pending_reconcile::reconcile_subscribe(state, subscribe.id, pending);
    crate::selective_files::retry_deferred(state, subscribe.id, &pending);

    if pending.is_empty() {
        return Ok(None);
    }
    Ok(Some((subscribe, pending)))
}

pub fn transfer_one(state: &ApiState, subscribe_id: SubscribeId) -> Result<(), String> {
    let guard = state.subscribe_guard(subscribe_id);
    let _guard = guard.lock();

    if state.subscribe_is_deleting(subscribe_id) {
        tracing::info!(subscribe_id = %subscribe_id, "订阅正在删除中，跳过 Transfer");
        return Ok(());
    }
    crate::http::file_delete::retry_queued_deletions(&state.store).map_err(|e| e.to_string())?;

    let Some((subscribe, pending)) = load_pending_transfer(state, subscribe_id)? else {
        return Ok(());
    };

    let media = state
        .store
        .lock()
        .get_media(subscribe.media_id)
        .map_err(|error| error.to_string())?
        .ok_or("Media not found")?;
    let plan = resolve_transfer_plan(state, &media, &subscribe)?;
    if !wait_for_transfer_library(state, &plan.library_id)? {
        return Ok(());
    }
    let (media, filter, facts, wash_filter, observation_fingerprint) =
        load_transfer_context(state, &subscribe)?;
    let routed = crate::delivery::RoutedDownloader::with_pending(state, &subscribe, &pending);
    restore_identities(state, &subscribe, &routed, &pending);
    let absolute = crate::season_check::absolute_map(
        state,
        &media,
        pending.iter().map(|(_, p)| p.torrent.title.as_str()),
    );
    let chosen = filter_matched_pending(&subscribe, &media, pending, absolute.as_ref());

    let library_lease = state.acquire_library_work(&plan.library_id)?;
    library_lease.control().check().map_err(|e| e.to_string())?;
    tracing::info!(%subscribe_id, pending_count = chosen.len(), "Transfer 开始");
    let destination_library: domain::LibraryId = plan
        .library_id
        .parse()
        .map_err(|error| format!("Library id {} 无法作为目标身份: {error}", plan.library_id))?;

    let added = build_added_input(
        RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: wash_filter.as_ref(),
            torrents: Vec::new(),
            facts,
            search_keywords: Vec::new(),
            downloader: &routed,
            library_root: &plan.root,
            library_id: Some(destination_library),
            root_id: Some(plan.root_id.clone()),
            config_revision: Some(plan.config_revision),
            transfer_mode: plan.mode,
            scrape: plan.nfo,
            hooks: None,
            naming: Some(&plan.naming),
            preserve_removed: true,
            overlap: Default::default(),
            absolute: absolute.as_ref(),
        },
        chosen,
    );

    let destinations = routed.imported_destinations().map_err(|e| e.to_string())?;
    // M7: do not start writing library files for a subscription being deleted.
    if state.subscribe_is_deleting(subscribe_id) {
        tracing::warn!(subscribe_id = %subscribe_id, "订阅已被预留删除，跳过 Transfer");
        return Ok(());
    }
    let known: std::collections::HashSet<std::path::PathBuf> = destinations
        .iter()
        .map(|m| m.destination_path.clone())
        .collect();
    // D-M2: hardlink fallback policy and fallback counting for this round.
    let fallback_copy = state
        .store
        .lock()
        .get_setting(crate::settings_keys::LIBRARY_HARDLINK_FALLBACK)
        .ok()
        .flatten()
        .is_none_or(|value| value.trim() != "fail");
    library::set_hardlink_fallback_copy(fallback_copy);
    let fallbacks_before = library::hardlink_fallback_count();
    let recorder = PlannedPublicationRecorder::new(
        PublicationRecorder {
            state,
            subscribe: &subscribe,
            lease: &library_lease,
            fingerprint: observation_fingerprint.clone(),
        },
        &plan,
    );
    let outcome = collect_completed_recorded(
        added,
        &library::Ffprobe::default(),
        &destinations,
        library_lease.control(),
        Some(&recorder),
    )
    .map_err(|e| e.to_string())?;
    commit_transfer_round(
        state,
        subscribe_id,
        &subscribe,
        &media,
        &outcome,
        &library_lease,
        &plan,
        &known,
        fallbacks_before,
    )
}

/// Finish a collected round: abandon it if the subscription or its lease is
/// gone, otherwise commit it and report the result.
fn commit_transfer_round(
    state: &ApiState,
    subscribe_id: SubscribeId,
    subscribe: &Subscribe,
    media: &Media,
    outcome: &RunOutcome,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    plan: &TransferPlan,
    known: &std::collections::HashSet<std::path::PathBuf>,
    fallbacks_before: u64,
) -> Result<(), String> {
    if state.subscribe_is_deleting(subscribe_id) {
        tracing::warn!(subscribe_id = %subscribe_id, "Transfer 完成但订阅已被预留删除，取消提交并回滚本轮新文件");
        rollback_new_files(state, outcome, plan.mode, known);
        return Ok(());
    }

    if let Err(error) = lease.control().check() {
        rollback_new_files(state, outcome, plan.mode, known);
        return Err(error.to_string());
    }
    if plan.scrape {
        attach_metadata_and_chapters(state, media, &outcome.files);
    }

    lease.control().check().map_err(|e| e.to_string())?;
    let ledger_count = outcome.files.len();
    let published = commit_transfer_outcome(state, subscribe, outcome, lease, plan)?;

    crate::http::library::enqueue_probes_for_files(state, &published);
    let fallbacks = library::hardlink_fallback_count().saturating_sub(fallbacks_before);
    if fallbacks > 0 {
        tracing::warn!(%subscribe_id, fallbacks, "硬链接失败，已回退为复制");
        crate::worker::notify(
            state,
            "硬链接回退为复制",
            &format!(
                "「{}」有 {fallbacks} 个文件无法硬链接（下载目录与媒体库可能不在同一设备），已改为复制",
                media.title
            ),
        );
    }
    if ledger_count > 0 {
        crate::worker::notify(
            state,
            "入库完成",
            &format!("「{}」{} 个文件已整理进媒体库", media.title, ledger_count),
        );
    }
    crate::worker::notify_subscription_complete(state, subscribe, media)?;
    let transfer_errors: Vec<String> = outcome
        .collection_errors
        .iter()
        .chain(outcome.submission_errors.iter())
        .cloned()
        .collect();
    tracing::info!(%subscribe_id, ledger_count, imported_count = outcome.transferred_enclosures.len(),
        error_count = transfer_errors.len(), "Transfer 完成");
    if !transfer_errors.is_empty() {
        return Err(format!(
            "{} file collection failure(s): {}",
            transfer_errors.len(),
            transfer_errors.join("; ")
        ));
    }
    Ok(())
}

/// M7: undo library files this abandoned round created. Only paths that were
/// not already known destinations before the round are touched. Hardlink /
/// Copy: delete the new file (the download keeps its data). Move: move the
/// file back to its download source; if that fails it is left in place.
fn rollback_new_files(
    state: &ApiState,
    outcome: &subscribe::RunOutcome,
    mode: Option<library::TransferMode>,
    known: &std::collections::HashSet<std::path::PathBuf>,
) {
    for coverage in &outcome.files {
        let path = coverage.file.path.clone();
        if known.contains(&path) || !path.exists() {
            continue;
        }
        let source =
            recorded_file_source(outcome, coverage.file.file_id).map(std::path::Path::to_path_buf);
        let result = match (mode, source) {
            (Some(library::TransferMode::Move), Some(source)) => {
                if source.exists() {
                    tracing::warn!(path = %path.display(), "回滚：Move 源路径已存在，保留库文件");
                    continue;
                }
                std::fs::rename(&path, &source)
            }
            (Some(library::TransferMode::Move), None) => {
                tracing::warn!(path = %path.display(), "回滚：Move 模式缺少源路径，保留库文件");
                continue;
            }
            _ => std::fs::remove_file(&path),
        };
        match result {
            Ok(()) => {
                tracing::info!(path = %path.display(), "回滚放弃提交的 Transfer 新文件");
                // The ledger row was reserved before publishing: drop it too.
                let ledger_path = path.to_string_lossy().to_string();
                if let Err(error) = state.store.lock().delete_ledger_path(&ledger_path) {
                    tracing::warn!(path = %ledger_path, %error, "回滚时删除预留台账行失败");
                }
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "回滚 Transfer 新文件失败")
            }
        }
    }
}
