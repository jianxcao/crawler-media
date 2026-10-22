//! Filesystem publication of collected files: destination naming, reservation,
//! controlled transfer, scraping hooks and the fact/ledger commit of a video.

use std::fs;
use std::path::{Path, PathBuf};

use domain::{FileCoverage, LedgerId, QualitySource, Release, UnitKey};
use downloader::Downloader;
use filter::ScoredTorrent;
use hooks::{HookEvent, Step};
use library::{resolve_mode, scrape_beside, transfer_file_controlled};

use super::TransferRecorder;
use crate::SubscribeError;
use crate::add::Added;
use crate::facts::QualityFact;

pub(super) fn reserve_and_publish<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    scored: &ScoredTorrent,
    release: &Release,
    src: &Path,
    dest: &Path,
    pending: bool,
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) -> Result<FileCoverage, SubscribeError> {
    // Publishing writes the destination identity: the Library that owns the root
    // and the root itself. Inventing either would record a file nobody owns, so a
    // missing identity fails the file instead.
    let library_id = added
        .input
        .library_id
        .ok_or(SubscribeError::MissingPublicationTarget("library_id"))?;
    let root_id = added
        .input
        .root_id
        .clone()
        .ok_or(SubscribeError::MissingPublicationTarget("root_id"))?;
    let mode = resolve_mode(src, added.input.library_root, added.input.transfer_mode);
    let file_id = LedgerId::new();
    let units: Vec<UnitKey> = if added.input.media.kind == domain::MediaKind::Movie {
        vec![UnitKey::WHOLE]
    } else {
        let span = release.episode_span();
        match (release.season, span) {
            (Some(s), Some((from, to))) => (from..=to)
                .map(|e| UnitKey::new(s as i32, e as i32))
                .collect(),
            (Some(s), None) => vec![UnitKey::new(s as i32, release.episode.unwrap_or(1) as i32)],
            _ => vec![UnitKey::WHOLE],
        }
    };
    let file = domain::LibraryFile {
        file_id,
        library_id,
        root_id,
        media_id: added.input.media.id,
        kind: added.input.media.kind,
        path: dest.to_path_buf(),
        version: domain::FileVersion {
            revision: 1,
            source_version: "v1".into(),
        },
        presence: domain::FilePresence::Present,
        publication: domain::FilePublication::Pending,
        size_bytes: std::fs::metadata(src).ok().map(|m| m.len()),
        stat_known: true,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: Some(src.to_path_buf()),
        transfer_mode: Some(mode.as_str().to_string()),
        resolution: release.resolution.clone(),
        codec: release.codec.clone(),
        hdr: release.hdr.clone(),
        quality_source: QualitySource::Release,
        confidence: release.confidence,
        filter_score: Some(scored.score),
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: None,
        backdrop_path: None,
        still_path: None,
    };
    let mut candidate = FileCoverage { file, units };
    if let Some(recorder) = recorder {
        candidate = recorder.prepare(&candidate, src, mode)?;
    }
    if !pending || !dest.is_file() {
        publish_video(added, src, dest, control)?;
    }
    candidate.file.publication = domain::FilePublication::Published;
    if let Some(recorder) = recorder {
        recorder.published(&candidate)?;
    }
    Ok(candidate)
}

fn publish_video<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    src: &Path,
    dest: &Path,
    control: &library::WorkControl,
) -> Result<(), SubscribeError> {
    let mode = resolve_mode(src, added.input.library_root, added.input.transfer_mode);
    tracing::info!(src = %src.display(), dest = %dest.display(), ?mode, "将已完成的媒体文件转入媒体库");
    emit(added.input.hooks, Step::Rename)?;
    emit(added.input.hooks, Step::Transfer)?;
    transfer_file_controlled(src, dest, mode, control).map_err(|e| {
        tracing::error!(src = %src.display(), dest = %dest.display(), error = %e, "文件转移失败");
        SubscribeError::Library(e)
    })?;
    control.check()?;
    emit(added.input.hooks, Step::Scrape)?;
    control.check()?;
    if let Err(error) = control.publish(|| {
        scrape_beside(dest, added.input.media, added.input.scrape, None).map_err(|e| e.to_string())
    }) {
        control.check()?;
        tracing::warn!(dest = %dest.display(), %error, "刮削写入元数据失败，但媒体文件已就位，继续入账");
    }
    Ok(())
}

pub(super) fn commit_video_facts<D: Downloader + ?Sized>(
    added: &mut Added<'_, D>,
    slots: &[(Option<u32>, Option<u32>)],
    score: i32,
    dest: &Path,
    approved: bool,
    removed_paths: &mut Vec<String>,
) -> Result<(), SubscribeError> {
    let previous: Vec<_> = slots
        .iter()
        .filter_map(|(s, e)| added.input.facts.get(*s, *e).and_then(|f| f.path.clone()))
        .collect();
    for (s, e) in slots {
        let fact = QualityFact {
            score,
            path: Some(dest.display().to_string()),
        };
        if approved {
            added.input.facts.replace(*s, *e, fact);
        } else {
            added.input.facts.upsert(*s, *e, fact);
        }
    }
    if approved && added.input.subscribe.wash_cut && !added.input.subscribe.keep_old_versions {
        for path in previous {
            let still_referenced = added
                .input
                .facts
                .entries()
                .any(|(_, fact)| fact.path.as_deref() == Some(path.as_str()));
            if still_referenced {
                continue;
            }
            if Path::new(&path) != dest {
                if !added.input.preserve_removed {
                    // Best effort here, but never silent: a failure to unlink is
                    // logged and the path is still handed to the caller, whose
                    // commit step retries the delete together with the ledger row.
                    // Returning early would persist facts pointing at `dest` while
                    // no ledger row exists for it, leaving the task silently active.
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            tracing::debug!(path = %path, "被替换的 Library 文件已不存在，仍清理台账");
                        }
                        Err(error) => {
                            tracing::error!(path = %path, %error, "删除被替换 Library 文件失败，交由提交阶段重试");
                        }
                    }
                }
                removed_paths.push(path);
            }
        }
    }
    Ok(())
}

pub(super) fn dest_path(
    root: &Path,
    media: &domain::Media,
    release: &Release,
    src: &Path,
    naming: Option<&str>,
) -> Result<PathBuf, SubscribeError> {
    let pattern = naming.unwrap_or_else(|| library::default_pattern(media.kind));
    let dest = library::render_path(root, pattern, media, release, src)?;
    if !dest.exists() {
        return Ok(dest);
    }
    // M8: never hand back an occupied alternate name; walk `-2`, `-3`, … until
    // a free slot. An alternate that already is this very file (retry after a
    // hardlink import) is reused so retries stay idempotent.
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("media");
    let src_stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("alt");
    let ext = dest.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    for n in 1..=MAX_ALTERNATE_NAMES {
        let name = if n == 1 {
            format!("{stem}-{src_stem}.{ext}")
        } else {
            format!("{stem}-{src_stem}-{n}.{ext}")
        };
        let candidate = dest.with_file_name(name);
        if std::fs::symlink_metadata(&candidate).is_err() || same_file(src, &candidate) {
            return Ok(candidate);
        }
    }
    Err(SubscribeError::Library(library::LibraryError::Io(
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("no free alternate name for {}", dest.display()),
        ),
    )))
}

const MAX_ALTERNATE_NAMES: usize = 100;

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(_: &Path, _: &Path) -> bool {
    false
}

fn emit(bus: Option<&hooks::Bus>, step: Step) -> Result<(), SubscribeError> {
    if let Some(bus) = bus {
        bus.emit(&HookEvent { step })?;
    }
    Ok(())
}
