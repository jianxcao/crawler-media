//! FFmpeg renders and publishes immutable candidates outside all locks.
use std::path::{Path, PathBuf};
use std::process::Command;

use library::{ProbeTarget, WorkControl};
use store::ChapterFrameTargetResult;

use super::{
    ChapterFrameRenderResult, ChapterFrameRequest, ChapterFrameRunner, ChapterFrameStaged,
    chapter_frame_artifact_path,
};

pub struct FfmpegChapterFrameRunner {
    ffmpeg: String,
}

impl Default for FfmpegChapterFrameRunner {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
        }
    }
}

impl FfmpegChapterFrameRunner {
    pub fn new(ffmpeg: impl Into<String>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
        }
    }
}

impl ChapterFrameRunner for FfmpegChapterFrameRunner {
    fn render(
        &self,
        request: &ChapterFrameRequest,
        control: &WorkControl,
    ) -> Result<Vec<ChapterFrameRenderResult>, String> {
        let mut results = Vec::new();
        for &(index, ref artifact) in &request.candidates {
            if control.is_cancelled() {
                cleanup_staged(&results);
                return Err("cancelled".into());
            }
            let staged = request
                .targets
                .get(index)
                .ok_or_else(|| "invalid_frame_target".to_string())
                .and_then(|&(start, end)| {
                    let midpoint = end
                        .checked_sub(start)
                        .filter(|duration| start >= 0 && *duration > 0)
                        .and_then(|duration| start.checked_add(duration / 2))
                        .ok_or_else(|| "invalid_frame_timestamp".to_string())?;
                    let midpoint = match request.duration_ms {
                        Some(duration) if duration > 0 => midpoint.min(duration - 1),
                        Some(_) => return Err("invalid_frame_duration".into()),
                        None => midpoint,
                    };
                    let target = chapter_frame_artifact_path(
                        &request.video,
                        &request.generation,
                        index,
                        artifact,
                    );
                    extract_frame(&self.ffmpeg, &request.video, midpoint, &target, control)
                });
            results.push(ChapterFrameRenderResult { index, staged });
        }
        Ok(results)
    }
}

fn extract_frame(
    ffmpeg: &str,
    video: &Path,
    midpoint: i64,
    target: &Path,
    control: &WorkControl,
) -> Result<ChapterFrameStaged, String> {
    let _permit = marker::media_read::acquire_media_read(video, control.signal())?;
    let staged = target.with_file_name(format!(".chapter-{}.jpg", uuid::Uuid::new_v4()));
    let mut command = Command::new(ffmpeg);
    command.args(["-y", "-ss", &format!("{:.3}", midpoint as f64 / 1000.0)]);
    ProbeTarget::from_path(video).apply_ffmpeg_input(&mut command);
    command.args([
        "-frames:v",
        "1",
        "-vf",
        "scale=960:-2",
        "-q:v",
        "4",
        "-f",
        "image2",
        "-vcodec",
        "mjpeg",
    ]);
    command.arg(&staged);
    let output = match control.output(&mut command) {
        Ok(output) => output,
        Err(error) => {
            remove_file(&staged);
            return Err(error.to_string());
        }
    };
    if !output.status.success() {
        remove_file(&staged);
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(ChapterFrameStaged {
        staged,
        target: target.to_path_buf(),
    })
}

/// File IO precedes the short Store completion CAS. Each destination is a
/// pre-registered unique candidate, so a stale worker cannot replace new bytes.
pub(super) fn publish_staged(
    rendered: &[ChapterFrameRenderResult],
    request: &ChapterFrameRequest,
    library_control: Option<&WorkControl>,
    control: &WorkControl,
) -> Result<Vec<ChapterFrameTargetResult>, String> {
    let mut seen = std::collections::HashSet::new();
    if rendered.iter().any(|result| {
        !request.pending_indices.contains(&result.index) || !seen.insert(result.index)
    }) {
        cleanup_staged(rendered);
        return Err("invalid_chapter_frame_results".into());
    }
    let mut results = Vec::new();
    for result in rendered {
        if control.is_cancelled() || library_control.is_some_and(WorkControl::is_cancelled) {
            cleanup_staged(rendered);
            return Err("cancelled".into());
        }
        let outcome = match &result.staged {
            Err(error) => Err(error.clone()),
            Ok(item) => publish_one(item, request, result.index),
        };
        if let Err(error) = &outcome {
            tracing::warn!(%error, file=%request.file_id, generation=%request.generation,
                attempt=request.attempt, index=result.index, "章节场景帧生成失败");
        }
        results.push(ChapterFrameTargetResult {
            index: result.index,
            ready: outcome.is_ok(),
            error: outcome.err(),
        });
    }
    cleanup_staged(rendered);
    Ok(results)
}

fn publish_one(
    item: &ChapterFrameStaged,
    request: &ChapterFrameRequest,
    index: usize,
) -> Result<(), String> {
    let artifact = request
        .candidates
        .iter()
        .find(|(candidate_index, _)| *candidate_index == index)
        .map(|(_, artifact)| artifact)
        .ok_or_else(|| "unregistered_chapter_frame_artifact".to_string())?;
    let expected =
        chapter_frame_artifact_path(&request.video, &request.generation, index, artifact);
    if item.target != expected {
        return Err("unregistered_chapter_frame_path".into());
    }
    if crate::fingerprint_job::current_source_version(&request.video) != request.source_version {
        return Err("chapter_frame_source_changed".into());
    }
    if !valid_frame(&item.staged) {
        return Err("invalid_chapter_frame_image".into());
    }
    // Link atomically without replacing an existing immutable destination.
    std::fs::hard_link(&item.staged, &expected).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn valid_frame(path: &Path) -> bool {
    std::fs::read(path).ok().is_some_and(|bytes| {
        image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).is_ok()
    })
}

fn cleanup_staged(results: &[ChapterFrameRenderResult]) {
    for result in results {
        if let Ok(item) = &result.staged {
            remove_file(&item.staged);
        }
    }
}

pub(super) fn cleanup_candidates(
    request: &ChapterFrameRequest,
    retired: &[(usize, store::ChapterFrameArtifact)],
) {
    for (index, artifact) in retired {
        remove_file(&chapter_frame_artifact_path(
            &request.video,
            &request.generation,
            *index,
            artifact,
        ));
    }
}

fn remove_file(path: &PathBuf) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, path=%path.display(), "无法删除章节帧文件"),
    }
}
