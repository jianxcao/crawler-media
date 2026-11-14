//! Per-file season results and publication uncertainty checks.
use super::*;

pub(super) fn build_marker_replacement(
    pipeline: &SeasonPipeline<'_>,
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
) -> Result<
    (
        Vec<StoredFileMarker>,
        Vec<(String, Vec<marker::ChapterMarker>)>,
    ),
    String,
> {
    let mut marker_map: std::collections::BTreeMap<String, StoredFileMarker> =
        std::collections::BTreeMap::new();
    let mut chapter_updates = Vec::new();
    let updated_at = now_secs();
    let store = pipeline.mgr.store.as_ref();

    for unit in units {
        let ledger_id = unit.row.file.file_id.to_string();
        let episode = crate::probe_manager::require_episode_coordinates(&unit.row)
            .map_err(|error| error.to_string())?
            .1;
        let detection = find_detection(detections, &ledger_id, episode);
        let previous = store
            .get_file_marker(unit.row.file.file_id, unit.row.units[0])
            .ok()
            .flatten();

        versions::validate_previous_absence(previous.as_ref(), detection)?;
        let (intro_start_ms, intro_end_ms) =
            detection_segment_range(detection, SegmentKind::Intro, previous.as_ref());
        let (outro_start_ms, outro_end_ms) =
            detection_segment_range(detection, SegmentKind::Outro, previous.as_ref());
        let source = marker_source(detection, previous.as_ref());

        if intro_start_ms.is_some() || outro_start_ms.is_some() {
            let candidate_marker = StoredFileMarker {
                file_id: unit.row.file.file_id,
                version: unit.row.file.version.clone(),
                unit: unit.row.units[0],
                intro_start_ms,
                intro_end_ms,
                outro_start_ms,
                outro_end_ms,
                source,
                locked: false,
                updated_at,
            };

            marker_map.insert(ledger_id.clone(), candidate_marker);
        }

        let existing = store
            .get_cached_chapters(&ledger_id)
            .ok()
            .flatten()
            .unwrap_or_default();
        let duration_ms = known_media_duration(&store, &ledger_id);
        let complete = marker::build_complete_timeline_chapters(
            &existing,
            intro_start_ms.zip(intro_end_ms),
            display_outro_range(outro_start_ms, outro_end_ms),
            duration_ms,
        );
        chapter_updates.push((ledger_id, complete));
    }

    let markers = marker_map.into_values().collect();
    Ok((markers, chapter_updates))
}

fn detection_segment_range(
    detection: Option<&EpisodeDetection>,
    kind: SegmentKind,
    previous: Option<&StoredFileMarker>,
) -> (Option<i64>, Option<i64>) {
    let (matched, sampled, outcome, previous) = match kind {
        SegmentKind::Intro => (
            detection.and_then(|d| d.intro_match.as_ref()),
            detection.is_some_and(|d| d.intro_evidence.is_some()),
            detection.and_then(|d| d.intro_outcome.as_ref()),
            previous.map(|m| (m.intro_start_ms, m.intro_end_ms)),
        ),
        SegmentKind::Outro => (
            detection.and_then(|d| d.outro_match.as_ref()),
            detection.is_some_and(|d| d.outro_evidence.is_some()),
            detection.and_then(|d| d.outro_outcome.as_ref()),
            previous.map(|m| (m.outro_start_ms, m.outro_end_ms)),
        ),
    };
    resolve_segment_range(
        matched.map(|v| (v.start_ms, v.end_ms)),
        sampled,
        outcome,
        previous,
    )
}

fn marker_source(
    detection: Option<&EpisodeDetection>,
    previous: Option<&StoredFileMarker>,
) -> String {
    if detection.is_some_and(|d| d.intro_match.is_some() || d.outro_match.is_some()) {
        "fingerprint_adaptive".to_string()
    } else {
        previous
            .map(|m| m.source.clone())
            .unwrap_or_else(|| "fingerprint_adaptive".to_string())
    }
}

/// 结合标准验证结果决定某个 kind 最终应当写入标记的时间区间。
///
/// - 标准验证返回 `Verified`：采用本轮识别到的新区间。
/// - 标准验证在完整窗口上明确返回 `NoMatch`：确认该剧集没有此片段，允许清空旧值。
/// - 其余情况（未采样、需要完整窗口、采样受限）都属于"无法判定"：
///   必须保留数据库中已有的旧值（可能只有起始时间），
///   绝不能把"无法判定"当成"确认无此片段"发布，从而误删旧标记。
pub(super) fn resolve_segment_range(
    matched: Option<(i64, i64)>,
    sampled: bool,
    outcome: Option<&marker::adaptive::VerificationOutcome>,
    previous: Option<(Option<i64>, Option<i64>)>,
) -> (Option<i64>, Option<i64>) {
    if let Some((start_ms, end_ms)) = matched {
        return (Some(start_ms), Some(end_ms));
    }
    let confirmed_absent = sampled
        && matches!(
            outcome,
            Some(marker::adaptive::VerificationOutcome::NoMatch { .. })
        );
    if confirmed_absent {
        return (None, None);
    }
    previous.unwrap_or((None, None))
}

/// 章节展示需要一个结束时间；标记本身允许只有片尾起始时间，
/// 因此这里为章节补一个仅供展示的兜底结束时间，不写回标记事实。
fn display_outro_range(start_ms: Option<i64>, end_ms: Option<i64>) -> Option<(i64, i64)> {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if end > start => Some((start, end)),
        (Some(start), None) => Some((start, start + 60_000)),
        _ => None,
    }
}

pub(super) fn ensure_no_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
) -> Result<(), String> {
    for kind in [SegmentKind::Intro, SegmentKind::Outro] {
        if has_truncated_unmatched(units, detections, collected_evidences, kind) {
            let kind_name = match kind {
                SegmentKind::Intro => "片头",
                SegmentKind::Outro => "片尾",
            };
            return Err(format!(
                "verification_unresolved: {kind_name}采集音频数据不完整或被截断，未能为全部剧集形成有效标记，保留现有标记"
            ));
        }
    }
    Ok(())
}

/// 某集该 kind 没有标记，但该集同 kind 的采样证据被截断或时长未知时为 true。
fn has_truncated_unmatched(
    units: &[ProbeUnit],
    detections: &[EpisodeDetection],
    collected_evidences: &[EpisodeEvidence],
    kind: SegmentKind,
) -> bool {
    units.iter().any(|unit| {
        let Some(episode) = single_episode(&unit.row) else {
            return true;
        };
        let ledger_id = unit.row.file.file_id.to_string();
        let detection = find_detection(detections, &ledger_id, episode);
        let has_detection = match kind {
            SegmentKind::Intro => detection.and_then(|d| d.intro_match.as_ref()).is_some(),
            SegmentKind::Outro => detection.and_then(|d| d.outro_match.as_ref()).is_some(),
        };
        if has_detection {
            return false;
        }
        // Episode has no detection for this kind. Check if it had a truncated or unknown PCM evidence.
        collected_evidences.iter().any(|evidence| {
            if evidence.ledger_id != ledger_id || evidence.kind != kind {
                return false;
            }
            !marker::boundary::evidence::complete_pcm(&evidence.capture)
        })
    })
}

fn find_detection<'a>(
    detections: &'a [EpisodeDetection],
    ledger_id: &str,
    _episode: u32,
) -> Option<&'a EpisodeDetection> {
    detections
        .iter()
        .find(|detection| detection.ledger_id == ledger_id)
}

pub(super) fn ensure_no_unresolved_verification(
    detections: &[EpisodeDetection],
) -> Result<(), String> {
    for detection in detections {
        for (kind, outcome) in [
            ("intro", &detection.intro_outcome),
            ("outro", &detection.outro_outcome),
        ] {
            match outcome {
                Some(
                    marker::adaptive::VerificationOutcome::Verified(_)
                    | marker::adaptive::VerificationOutcome::NoMatch { .. },
                ) => {}
                other => {
                    return Err(format!(
                        "verification_unresolved: episode={} kind={kind} outcome={other:?}; keeping existing markers",
                        detection.episode
                    ));
                }
            }
        }
    }
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
