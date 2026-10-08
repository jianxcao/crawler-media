use std::fs;
use std::path::PathBuf;

use domain::{Confidence, Media, MediaKind, Release};

use crate::scrape::scrape_beside;
use crate::{LibraryError, TransferMode, resolve_mode, transfer_file};

pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "wmv", "webm", "flv", "ts", "m2ts", "mts", "mpg", "mpeg",
    "vob", "iso", "strm",
];

pub fn is_video_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchKind {
    Intake,
    InPlace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchJob {
    pub kind: WatchKind,
    pub path: PathBuf,
    pub library_root: PathBuf,
    /// B11: 可选的电视剧媒体库根路径。若设置，intake 解析出的剧集视频文件转移到该路径。
    pub tv_library_root: Option<PathBuf>,
    /// 写 NFO 等侧车文件（总闸 + mirror_nfo 的合取）。
    pub scrape: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unidentified {
    pub path: PathBuf,
    pub confidence: Confidence,
}

/// 一个已转移（或已确认存在）的文件，连同 scan 阶段识别的 Release。
/// `identified_release` 携带 NFO/父目录 fallback 后的解析结果；
/// `None` 表示 intake 路径中文件名本身置信度足够，调用方可按 basename 解析。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferredFile {
    pub path: PathBuf,
    /// scan 阶段识别结果；None = 文件名即可信，调用方自行解析 basename。
    pub identified_release: Option<Release>,
}

impl PartialEq<PathBuf> for TransferredFile {
    fn eq(&self, other: &PathBuf) -> bool {
        &self.path == other
    }
}

impl PartialEq<TransferredFile> for PathBuf {
    fn eq(&self, other: &TransferredFile) -> bool {
        self == &other.path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct WatchOutcome {
    pub transferred: Vec<TransferredFile>,
    pub unidentified: Vec<Unidentified>,
    pub errors: Vec<FileError>,
}

pub fn scan_watch(job: &WatchJob, media: &Media) -> Result<WatchOutcome, LibraryError> {
    match job.kind {
        WatchKind::InPlace => in_place(job),
        WatchKind::Intake => intake(job, media),
    }
}

fn in_place(job: &WatchJob) -> Result<WatchOutcome, LibraryError> {
    let mut outcome = WatchOutcome::default();
    walk_in_place(&job.path, job.scrape, &mut outcome)?;
    Ok(outcome)
}

fn walk_in_place(
    dir: &std::path::Path,
    scrape: bool,
    outcome: &mut WatchOutcome,
) -> Result<(), LibraryError> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_in_place(&path, scrape, outcome)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if !is_video_file(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let mut parsed = release::parse(name);
        if parsed.confidence == Confidence::Low {
            if let Some(parent) = path.parent() {
                let nfo_cand = path.with_extension("nfo");
                let nfo_file = if nfo_cand.is_file() {
                    Some(nfo_cand)
                } else {
                    fs::read_dir(parent).ok().and_then(|entries| {
                        entries.filter_map(Result::ok).find_map(|e| {
                            let p = e.path();
                            (p.extension().and_then(|s| s.to_str()) == Some("nfo")).then_some(p)
                        })
                    })
                };
                if let Some(nfo_p) = nfo_file {
                    if let Ok(content) = fs::read_to_string(&nfo_p) {
                        if let Some(meta) = crate::nfo::parse_nfo(&content) {
                            if let Some(title) = meta.title {
                                if !title.trim().is_empty() {
                                    parsed.title = title;
                                    parsed.year = meta.year.and_then(|y| y.parse::<u16>().ok());
                                    parsed.confidence = Confidence::High;
                                }
                            }
                        }
                    }
                }
                if parsed.confidence == Confidence::Low {
                    if let Some(parent_name) = parent.file_name().and_then(|n| n.to_str()) {
                        let parent_parsed = release::parse(parent_name);
                        if parent_parsed.confidence != Confidence::Low {
                            parsed.title = parent_parsed.title;
                            parsed.year = parent_parsed.year;
                            parsed.confidence = parent_parsed.confidence;
                        }
                    }
                }
            }
        }
        if parsed.confidence == Confidence::Low {
            continue;
        }
        let media = Media {
            id: domain::MediaId::new(),
            kind: if parsed.season.is_some() {
                MediaKind::Tv
            } else {
                MediaKind::Movie
            },
            title: parsed.title.clone(),
            year: parsed.year,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        // 仅在已有 TMDB ID 等成熟元数据时才允许落盘 NFO；
        // 未联网识别阶段绝不抢先生成无剧情的空壳 NFO 占位文件。
        if scrape && media.tmdb_id.is_some() {
            scrape_beside(&path, &media, true, None)?;
        }
        // B14: 把 scan 阶段经过 NFO/父目录 fallback 的 parsed 结果一并传出，
        // 避免调用方按 basename 重解（movie.mkv + movie.nfo 因此能正确识别）。
        outcome.transferred.push(TransferredFile {
            path,
            identified_release: Some(parsed),
        });
    }
    Ok(())
}

fn intake(job: &WatchJob, media: &Media) -> Result<WatchOutcome, LibraryError> {
    let mut outcome = WatchOutcome::default();
    walk_intake(&job.path, job, media, &mut outcome)?;
    Ok(outcome)
}

fn walk_intake(
    current_dir: &std::path::Path,
    job: &WatchJob,
    media: &Media,
    outcome: &mut WatchOutcome,
) -> Result<(), LibraryError> {
    for entry in fs::read_dir(current_dir)? {
        let path = entry?.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_intake(&path, job, media, outcome)?;
            continue;
        }
        if !meta.is_file() || !is_video_file(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let mut parsed = release::parse(name);
        if parsed.confidence == Confidence::Low {
            if let Some(parent) = path.parent() {
                let nfo_cand = path.with_extension("nfo");
                let nfo_file = if nfo_cand.is_file() {
                    Some(nfo_cand)
                } else {
                    fs::read_dir(parent).ok().and_then(|entries| {
                        entries.filter_map(Result::ok).find_map(|e| {
                            let p = e.path();
                            (p.extension().and_then(|s| s.to_str()) == Some("nfo")).then_some(p)
                        })
                    })
                };
                if let Some(nfo_p) = nfo_file {
                    if let Ok(content) = fs::read_to_string(&nfo_p) {
                        if let Some(meta) = crate::nfo::parse_nfo(&content) {
                            if let Some(title) = meta.title {
                                if !title.trim().is_empty() {
                                    parsed.title = title;
                                    parsed.year = meta.year.and_then(|y| y.parse::<u16>().ok());
                                    parsed.confidence = Confidence::High;
                                }
                            }
                        }
                    }
                }
                if parsed.confidence == Confidence::Low {
                    if let Some(parent_name) = parent.file_name().and_then(|n| n.to_str()) {
                        let parent_parsed = release::parse(parent_name);
                        if parent_parsed.confidence != Confidence::Low {
                            parsed.title = parent_parsed.title;
                            parsed.year = parent_parsed.year;
                            parsed.confidence = parent_parsed.confidence;
                        }
                    }
                }
            }
        }
        if parsed.confidence == Confidence::Low {
            outcome.unidentified.push(Unidentified {
                path,
                confidence: Confidence::Low,
            });
            continue;
        }
        let is_tv = parsed.season.is_some() || parsed.episode.is_some();
        let target_root = if is_tv {
            job.tv_library_root.as_ref().unwrap_or(&job.library_root)
        } else {
            &job.library_root
        };

        // 如果文件位于子目录中（例如 watch/Show Title/S01E01.mp4 或 watch/Show Title/Season 1/S01E01.mp4），
        // 在目标库中保持相同的相对层级结构，避免把各集直接平铺到电视剧库根目录下
        let rel_path = path.strip_prefix(&job.path).unwrap_or(&path);
        let dest = target_root.join(rel_path);

        if dest.exists() {
            // B07: hardlink 模式源文件仍留存，下次轮询会再次遇到同一个目标。
            // 若源和目标是同一个实体（dev+ino 相同，已成功 hardlink），把它视为幂等成功；
            // 若不是同一实体，则真实碰撞——拒绝覆盖已有 Library 文件。
            let same_entity = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    std::fs::metadata(&path)
                        .ok()
                        .zip(std::fs::metadata(&dest).ok())
                        .is_some_and(|(src_meta, dst_meta)| {
                            src_meta.dev() == dst_meta.dev() && src_meta.ino() == dst_meta.ino()
                        })
                }
                #[cfg(not(unix))]
                {
                    false
                }
            };
            if same_entity {
                tracing::debug!(
                    src = %path.display(),
                    dest = %dest.display(),
                    "Watch intake 目标已存在且与源同实体，视为幂等重试"
                );
                outcome.transferred.push(TransferredFile {
                    path: dest,
                    identified_release: Some(parsed),
                });
                continue;
            } else {
                tracing::error!(
                    src = %path.display(),
                    dest = %dest.display(),
                    "Watch intake 目标已存在且与源不同，拒绝覆盖已有 Library 文件"
                );
                outcome.errors.push(FileError {
                    path: path.clone(),
                    error: format!(
                        "watch intake destination already exists: {}",
                        dest.display()
                    ),
                });
                continue;
            }
        }
        let mode = resolve_mode(&path, target_root, Some(TransferMode::Hardlink));
        if let Err(err) = transfer_file(&path, &dest, mode) {
            tracing::error!(
                src = %path.display(),
                dest = %dest.display(),
                error = %err,
                "Watch intake transfer file failed"
            );
            outcome.errors.push(FileError {
                path: path.clone(),
                error: err.to_string(),
            });
            continue;
        }
        if job.scrape {
            if let Err(err) = scrape_beside(&dest, media, true, None) {
                tracing::warn!(
                    dest = %dest.display(),
                    error = %err,
                    "Watch intake scrape beside failed"
                );
            }
        }
        outcome.transferred.push(TransferredFile {
            path: dest,
            identified_release: Some(parsed),
        });
    }
    Ok(())
}
