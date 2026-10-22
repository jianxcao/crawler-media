//! Selection and scoring of completed Downloads: which arrivals become Library
//! files, how their slots map onto Subscribe facts, and the per-file publish loop.

use std::path::{Path, PathBuf};

use domain::{Confidence, FileCoverage, FileSource, QualitySource, Release};
use downloader::Downloader;
use filter::ScoredTorrent;
use library::{MediaProbe, resolve_mode};

use super::TransferRecorder;
use super::recover::{quality_for, replacement_survives_probe};
use super::transfer::{commit_video_facts, dest_path, reserve_and_publish};
use crate::SubscribeError;
use crate::add::Added;
use crate::choose::covered_slots;
use crate::facts::{QualityFact, SubscribeFacts};

pub(super) fn collect_scored_torrent<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    scored: &ScoredTorrent,
    files: &mut Vec<FileCoverage>,
    file_sources: &mut Vec<FileSource>,
    removed_paths: &mut Vec<String>,
    transferred_enclosures: &mut Vec<String>,
    rejected_enclosures: &mut Vec<String>,
    awaiting_probe_enclosures: &mut Vec<String>,
    collection_errors: &mut Vec<String>,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) {
    // Review H-3: a claimed user task's files are never moved away; Move
    // degrades to Hardlink (copy fallback) for this torrent.
    let saved_mode = added.input.transfer_mode;
    if matches!(saved_mode, Some(library::TransferMode::Move))
        && added
            .input
            .downloader
            .is_claimed(&scored.torrent)
            .unwrap_or(true)
    {
        tracing::info!(torrent = %scored.torrent.title, "认领的用户任务：Move 降级为硬链接，不移走用户文件");
        added.input.transfer_mode = Some(library::TransferMode::Hardlink);
    }
    collect_scored_torrent_inner(
        added,
        probe,
        existing,
        scored,
        files,
        file_sources,
        removed_paths,
        transferred_enclosures,
        rejected_enclosures,
        awaiting_probe_enclosures,
        collection_errors,
        control,
        recorder,
    );
    added.input.transfer_mode = saved_mode;
}

#[allow(clippy::too_many_arguments)]
fn collect_scored_torrent_inner<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    scored: &ScoredTorrent,
    files: &mut Vec<FileCoverage>,
    file_sources: &mut Vec<FileSource>,
    removed_paths: &mut Vec<String>,
    transferred_enclosures: &mut Vec<String>,
    rejected_enclosures: &mut Vec<String>,
    awaiting_probe_enclosures: &mut Vec<String>,
    collection_errors: &mut Vec<String>,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) {
    let mut vetoed = false;
    // Review M-4: a veto caused by an unfinished probe; the row stays active.
    let mut awaiting_probe = false;
    // M2: video files skipped because their slots were already owned.
    let mut duplicate = false;
    let errors_before_torrent = collection_errors.len();
    // D-L1: finished files are collected while the rest of the task is still
    // downloading; the task only counts as transferred once it is done.
    // Move would take files away from a running task, so it waits.
    let Some(torrent_files) = collectable_torrent_files(added, scored, collection_errors) else {
        return;
    };
    let mut torrent_destinations = Vec::new();
    for src in torrent_files.videos {
        if control.is_cancelled() {
            return;
        }
        let source_path = src.display().to_string();
        let mapping = existing.iter().find(|m| m.source_path == source_path);
        if let Some(mapping) = mapping.filter(|mapping| !mapping.transfer_pending) {
            if mapping.destination_path.is_file() {
                torrent_destinations.push(mapping.clone());
                reconcile_mapping_facts(
                    &mut added.input.facts,
                    added.input.subscribe,
                    mapping,
                    scored.score,
                );
            } else {
                tracing::error!(source = %source_path, destination = %mapping.destination_path.display(), "已拥有的视频目标缺失，不自动恢复用户删除的文件");
                collection_errors.push(format!(
                    "missing owned destination: {}",
                    mapping.destination_path.display()
                ));
            }
            continue;
        }
        match collect_one(
            added,
            probe,
            scored,
            src,
            torrent_files.single_file,
            files,
            file_sources,
            removed_paths,
            &mut vetoed,
            &mut awaiting_probe,
            &mut duplicate,
            control,
            recorder,
            mapping.filter(|mapping| mapping.transfer_pending),
        ) {
            Ok(Some(dest)) => torrent_destinations.push(
                crate::collection_destinations::DestinationMapping::new(source_path, dest),
            ),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(torrent = %scored.torrent.title, source = %source_path, error = %error, "收集视频文件失败");
                collection_errors.push(format!(
                    "{} ({}): {error}",
                    scored.torrent.title, source_path
                ));
            }
        }
    }
    // Place the sidecar subtitles beside what this torrent just produced.
    let Some(subs_count) = place_sidecar_subtitles(
        added,
        existing,
        &torrent_destinations,
        torrent_files.subs,
        duplicate,
        control,
        collection_errors,
    ) else {
        return;
    };
    // A download that only duplicated owned episodes (M2) is finished too.
    let has_work = !torrent_destinations.is_empty() || subs_count > 0 || duplicate;
    if torrent_files.task_done
        && torrent_files.reported_any
        && has_work
        && !awaiting_probe
        && collection_errors.len() == errors_before_torrent
    {
        transferred_enclosures.push(scored.torrent.enclosure.clone());
    }
    if vetoed && !rejected_enclosures.contains(&scored.torrent.enclosure) {
        rejected_enclosures.push(scored.torrent.enclosure.clone());
    }
    if awaiting_probe && !vetoed && !awaiting_probe_enclosures.contains(&scored.torrent.enclosure) {
        awaiting_probe_enclosures.push(scored.torrent.enclosure.clone());
    }
}

/// D-L1: the files a task has finished so far, split by kind.
struct TorrentFiles {
    videos: Vec<PathBuf>,
    subs: Vec<PathBuf>,
    task_done: bool,
    single_file: bool,
    reported_any: bool,
}

/// Reads the files a task has finished so far and splits them into videos and
/// subtitles. `None` means the downloader could not be read: skip the torrent.
fn collectable_torrent_files<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    scored: &ScoredTorrent,
    collection_errors: &mut Vec<String>,
) -> Option<TorrentFiles> {
    let (completed_files, task_done) = match added
        .input
        .downloader
        .collectable_files(&scored.torrent)
    {
        Ok((files, done))
            if done || !matches!(added.input.transfer_mode, Some(library::TransferMode::Move)) =>
        {
            (files, done)
        }
        Ok(_) => (Vec::new(), false),
        Err(error) => {
            tracing::error!(torrent = %scored.torrent.title, error = %error, "读取已完成下载文件失败");
            collection_errors.push(format!("{}: {error}", scored.torrent.title));
            return None;
        }
    };
    let (videos, others): (Vec<_>, Vec<_>) = completed_files
        .into_iter()
        .partition(|path| crate::sidecars::is_video(path));
    let (subs, _junk): (Vec<_>, Vec<_>) = others
        .into_iter()
        .partition(|path| crate::sidecars::is_subtitle(path));
    let single_file = videos.len() == 1;
    let reported_any = !videos.is_empty() || !subs.is_empty();
    Some(TorrentFiles {
        videos,
        subs,
        task_done,
        single_file,
        reported_any,
    })
}

/// Places the subtitles of a torrent beside the destinations just collected and
/// reports how many subtitles it saw; `None` means the task was cancelled.
fn place_sidecar_subtitles<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    existing: &[crate::collection_destinations::DestinationMapping],
    torrent_destinations: &[crate::collection_destinations::DestinationMapping],
    subs: Vec<PathBuf>,
    duplicate: bool,
    control: &library::WorkControl,
    collection_errors: &mut Vec<String>,
) -> Option<usize> {
    let mut sidecar_destinations = torrent_destinations.to_vec();
    for mapping in existing {
        if mapping.destination_path.is_file()
            && !sidecar_destinations
                .iter()
                .any(|known| known.source_path == mapping.source_path)
        {
            sidecar_destinations.push(mapping.clone());
        }
    }
    // M2: subtitles of a pure duplicate have no new video to sit beside.
    let subs = if duplicate && torrent_destinations.is_empty() {
        Vec::new()
    } else {
        subs
    };
    let subs_count = subs.len();
    for src in subs {
        if control.is_cancelled() {
            return None;
        }
        match crate::sidecars::place_mapped_subtitle_controlled(
            &src,
            &sidecar_destinations,
            added.input.transfer_mode,
            control,
        ) {
            Ok(()) => {}
            Err(error) => {
                tracing::error!(source = %src.display(), error = %error, "摆放字幕失败");
                collection_errors.push(format!("subtitle {}: {error}", src.display()));
            }
        }
    }
    Some(subs_count)
}

pub(super) fn reconcile_mapping_facts(
    facts: &mut SubscribeFacts,
    subscribe: &domain::Subscribe,
    mapping: &crate::collection_destinations::DestinationMapping,
    score: i32,
) {
    let dest_str = mapping.destination_path.display().to_string();
    let file_release = mapping.quality.clone().unwrap_or_else(|| {
        release::parse(
            mapping
                .destination_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        )
    });
    if facts.quality(&dest_str).is_none() {
        facts.set_quality(dest_str.clone(), file_release.clone());
    }
    let slots = if !mapping.slots.is_empty() {
        mapping.slots.clone()
    } else {
        covered_slots(subscribe, &file_release)
    };
    for (s, e) in slots {
        let existing_fact = facts.get(s, e);
        let is_stale = existing_fact
            .as_ref()
            .and_then(|f| f.path.as_deref())
            .map_or(false, |p| !std::path::Path::new(p).exists());
        if existing_fact.is_none() || is_stale {
            facts.replace(
                s,
                e,
                QualityFact {
                    score,
                    path: Some(dest_str.clone()),
                },
            );
        } else if existing_fact.as_ref().and_then(|f| f.path.as_deref()) == Some(dest_str.as_str())
        {
            facts.upsert(
                s,
                e,
                QualityFact {
                    score,
                    path: Some(dest_str.clone()),
                },
            );
        }
    }
}

fn collect_one<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    probe: &dyn MediaProbe,
    scored: &ScoredTorrent,
    src: PathBuf,
    single_file: bool,
    files: &mut Vec<FileCoverage>,
    file_sources: &mut Vec<FileSource>,
    removed_paths: &mut Vec<String>,
    vetoed: &mut bool,
    awaiting_probe: &mut bool,
    duplicate: &mut bool,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
    pending: Option<&crate::collection_destinations::DestinationMapping>,
) -> Result<Option<PathBuf>, SubscribeError> {
    // The files this round already published: the M2 ownership check reads them
    // as a read-only snapshot while the collected list keeps growing below.
    let published: &[FileCoverage] = &files[..];
    let admitted = match admit_arrival(
        added,
        probe,
        scored,
        &src,
        single_file,
        published,
        vetoed,
        awaiting_probe,
        duplicate,
        control,
    )? {
        Some(admitted) => admitted,
        None => return Ok(None),
    };
    let Admitted {
        release,
        slots,
        approved,
        has_existing,
        pre_probed,
    } = admitted;
    let (dest, mut candidate) =
        match reserve_destination(added, scored, &release, &src, pending, control, recorder)? {
            Some(found) => found,
            None => return Ok(None),
        };
    let verdict = match confirm_replacement(
        added,
        probe,
        scored,
        &src,
        &dest,
        pre_probed,
        &slots,
        approved,
        has_existing,
        vetoed,
        awaiting_probe,
        removed_paths,
        control,
    )? {
        Some(verdict) => verdict,
        None => return Ok(None),
    };
    let Verdict {
        quality,
        source,
        confidence,
        approved,
        actual_score,
    } = verdict;
    commit_arrival(
        added,
        &slots,
        &dest,
        actual_score,
        approved,
        published,
        removed_paths,
    )?;
    let ledger_path = dest.display().to_string();
    let mut owned_quality = scored.release.clone();
    owned_quality.resolution = quality.resolution.clone();
    owned_quality.codec = quality.codec.clone();
    owned_quality.hdr = quality.hdr.clone();
    added
        .input
        .facts
        .set_quality(ledger_path.clone(), owned_quality);
    file_sources.push(FileSource {
        file_id: candidate.file.file_id,
        destination: dest.clone(),
        source: src.clone(),
    });
    candidate.file.resolution = quality.resolution.clone();
    candidate.file.codec = quality.codec.clone();
    candidate.file.hdr = quality.hdr.clone();
    candidate.file.quality_source = source;
    candidate.file.confidence = confidence;
    candidate.file.filter_score = Some(actual_score);
    files.push(candidate);
    Ok(Some(dest))
}

/// What `admit_arrival` decided before the destination is reserved; the fields
/// feed the post-transfer verification so `collect_one` stays an outline.
struct Admitted {
    release: Release,
    slots: Vec<(Option<u32>, Option<u32>)>,
    approved: bool,
    has_existing: bool,
    pre_probed: Option<(library::FileQuality, QualitySource, Confidence)>,
}

/// The verdict of the final, probe-backed replacement approval.
struct Verdict {
    quality: library::FileQuality,
    source: QualitySource,
    confidence: Confidence,
    approved: bool,
    actual_score: i32,
}

#[allow(clippy::too_many_arguments)]
fn admit_arrival<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    probe: &dyn MediaProbe,
    scored: &ScoredTorrent,
    src: &Path,
    single_file: bool,
    published: &[FileCoverage],
    vetoed: &mut bool,
    awaiting_probe: &mut bool,
    duplicate: &mut bool,
    control: &library::WorkControl,
) -> Result<Option<Admitted>, SubscribeError> {
    let file_release = release::parse(
        src.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    );
    // G-H2b: an absolute-numbered file maps onto the catalog season too.
    // Only for torrents whose own title has no season marker.
    let (file_release, torrent_release) = match added.input.absolute {
        Some(map) if !release::has_explicit_season(&scored.torrent.title) => {
            (map.apply(&file_release), map.apply(&scored.release))
        }
        _ => (file_release, scored.release.clone()),
    };
    let release = match crate::file_identity::resolve_file_release(
        added.input.subscribe,
        added.input.media,
        src,
        &file_release,
        &torrent_release,
        single_file,
    ) {
        Some(release) => release,
        None => return Ok(None),
    };
    // Facts paths are canonical: retries must not create collision copies or
    // restore user-deleted files. Unapproved Wash-cut arrivals never replace owned slots.
    let slots = covered_slots(added.input.subscribe, &release);
    let approved = crate::slot_replacement::is_replacement_approved(
        added.input.subscribe,
        added.input.wash_filter,
        &added.input.facts,
        scored,
        &slots,
    );
    let has_existing = slots
        .iter()
        .any(|(season, episode)| added.input.facts.get(*season, *episode).is_some());
    // Only unapproved Wash-cut arrivals are blocked.
    if added.input.subscribe.wash_cut && has_existing && !approved {
        return Ok(None);
    }
    // M2: without wash-cut an arrival whose every slot already has a file
    // owned before this run is a duplicate (e.g. the owned episode inside a
    // range pack). Files published earlier in this same run (extras) coexist.
    let all_owned = !slots.is_empty()
        && slots.iter().all(|(season, episode)| {
            added
                .input
                .facts
                .get(*season, *episode)
                .and_then(|fact| fact.path.as_deref())
                .is_some_and(|path| {
                    Path::new(path).exists() && !published_this_round(published, path)
                })
        });
    if !added.input.subscribe.wash_cut && all_owned {
        tracing::info!(torrent = %scored.torrent.title, file = %src.display(), "非洗版：槽位已有在库文件，跳过转存");
        *duplicate = true;
        return Ok(None);
    }
    // Review N-1: an arrival that needs S1 approval is probed at the source
    // before anything is published, so an unprobeable or non-upgrade file
    // never enters the Library (no publish/retract churn, no copies).
    let pre_probed = if added.input.subscribe.wash_cut && has_existing {
        let (quality, source, confidence) = quality_for(probe, &src, &scored.release, control);
        control.check()?;
        if source != QualitySource::Probe {
            tracing::warn!(file = %src.display(), "源文件 probe 未完成，暂不转入媒体库，等待下一轮");
            *awaiting_probe = true;
            return Ok(None);
        }
        let (ok, _) = replacement_survives_probe(added, scored, &slots, approved, &quality, true);
        if !ok {
            tracing::warn!(file = %src.display(), "源文件 probe 后不构成升级，不转入媒体库");
            *vetoed = true;
            return Ok(None);
        }
        Some((quality, source, confidence))
    } else {
        None
    };
    Ok(Some(Admitted {
        release,
        slots,
        approved,
        has_existing,
        pre_probed,
    }))
}

fn reserve_destination<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    scored: &ScoredTorrent,
    release: &Release,
    src: &Path,
    pending: Option<&crate::collection_destinations::DestinationMapping>,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) -> Result<Option<(PathBuf, FileCoverage)>, SubscribeError> {
    // Main's reservation: a retried transfer reuses its reserved destination.
    // Otherwise a name can be taken between choosing it and publishing it
    // (another worker, the user): publication never overwrites (D-H3), so pick
    // the next free name and retry a few times (M8).
    let (dest, candidate) = match pending {
        Some(mapping) => {
            let dest = mapping.destination_path.clone();
            let candidate =
                reserve_and_publish(added, scored, release, src, &dest, true, control, recorder)?;
            (dest, candidate)
        }
        None => {
            let mut attempts = 0;
            loop {
                let dest = dest_path(
                    added.input.library_root,
                    added.input.media,
                    release,
                    src,
                    added.input.naming,
                )?;
                if added.input.facts.entries().any(|(_, f)| {
                    f.path
                        .as_deref()
                        .is_some_and(|p| p == dest.to_string_lossy())
                }) {
                    return Ok(None);
                }
                match reserve_and_publish(
                    added, scored, release, src, &dest, false, control, recorder,
                ) {
                    Err(SubscribeError::Library(library::LibraryError::Io(error)))
                        if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 3 =>
                    {
                        attempts += 1;
                        tracing::warn!(dest = %dest.display(), "目标文件在发布前出现，改用下一个可用文件名");
                    }
                    result => break (dest, result?),
                }
            }
        }
    };
    Ok(Some((dest, candidate)))
}

#[allow(clippy::too_many_arguments)]
fn confirm_replacement<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    probe: &dyn MediaProbe,
    scored: &ScoredTorrent,
    src: &Path,
    dest: &Path,
    pre_probed: Option<(library::FileQuality, QualitySource, Confidence)>,
    slots: &[(Option<u32>, Option<u32>)],
    approved: bool,
    has_existing: bool,
    vetoed: &mut bool,
    awaiting_probe: &mut bool,
    removed_paths: &mut Vec<String>,
    control: &library::WorkControl,
) -> Result<Option<Verdict>, SubscribeError> {
    let (quality, source, confidence) = match pre_probed {
        Some(probed) => probed,
        None => quality_for(probe, dest, &scored.release, control),
    };
    control.check()?;
    // Final deletion approval uses actual (probed) quality, never the download
    // title alone: an unprobed arrival cannot replace an owned slot (S1).
    let (approved, actual_score) = replacement_survives_probe(
        added,
        scored,
        slots,
        approved,
        &quality,
        source == QualitySource::Probe,
    );
    if added.input.subscribe.wash_cut && has_existing && !approved {
        // Review M-4: only a finished probe that disproves the upgrade is a
        // final rejection; an unfinished probe waits for the next round.
        let probed = source == QualitySource::Probe;
        if probed {
            *vetoed = true;
        } else {
            *awaiting_probe = true;
        }
        // Roll back the side-by-side publish (queued deletion of file and
        // ledger row). Move took the file out of the download: keep it.
        let mode = resolve_mode(src, added.input.library_root, added.input.transfer_mode);
        if !matches!(mode, library::TransferMode::Move) {
            tracing::warn!(dest = %dest.display(), probed, "替换未获批准，撤回已转入的并列文件");
            removed_paths.push(dest.display().to_string());
            return Ok(None);
        }
        tracing::warn!(
            dest = %dest.display(),
            resolution = ?quality.resolution,
            codec = ?quality.codec,
            "probe 后的实际质量不构成升级，保留已转入的新文件，不删除在位版本"
        );
    }
    Ok(Some(Verdict {
        quality,
        source,
        confidence,
        approved,
        actual_score,
    }))
}

/// Was `path` published by this very round? Facts carry canonical destination
/// paths, so the M2 ownership checks read the round's own publications instead
/// of keeping a second, parallel bookkeeping list of ledger rows.
fn published_this_round(published: &[FileCoverage], path: &str) -> bool {
    published
        .iter()
        .any(|coverage| coverage.file.path.to_string_lossy().as_ref() == path)
}

fn commit_arrival<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    slots: &[(Option<u32>, Option<u32>)],
    dest: &Path,
    actual_score: i32,
    approved: bool,
    published: &[FileCoverage],
    removed_paths: &mut Vec<String>,
) -> Result<(), SubscribeError> {
    // 拒绝的 Wash-cut 只能登记并列文件，不能借标题高分改写已有槽位。
    let committed_slots: Vec<_> = slots
        .iter()
        .copied()
        .filter(|(s, e)| {
            !added.input.subscribe.wash_cut || approved || added.input.facts.get(*s, *e).is_none()
        })
        // M2: non-wash never repoints a slot owned before this run.
        .filter(|(s, e)| {
            added.input.subscribe.wash_cut
                || !added
                    .input
                    .facts
                    .get(*s, *e)
                    .and_then(|fact| fact.path.as_deref())
                    .is_some_and(|path| !published_this_round(published, path))
        })
        .collect();
    commit_video_facts(
        added,
        &committed_slots,
        actual_score,
        dest,
        approved,
        removed_paths,
    )?;
    Ok(())
}
