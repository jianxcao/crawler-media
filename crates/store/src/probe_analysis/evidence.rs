use super::*;
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug, Serialize)]
pub(super) struct EvidenceGuard {
    pub id: String,
    // Exact original SQLite payloads, not reserialized parsed captures.
    pub values: Vec<Option<String>>,
}

const CACHE_COLUMNS: &str = "cache_key,CAST(algorithm_version AS TEXT),CAST(sample_duration_secs AS TEXT),CAST(media_duration_ms AS TEXT),intro_json,outro_json";
const SAMPLE_COLUMNS: &str = "file_id,source_version,capture_profile_key,kind,CAST(window_start_ms AS TEXT),CAST(window_end_ms AS TEXT),CAST(pcm_duration_ms AS TEXT),fingerprint_json,metrics_json";
const DETAILS_COLUMNS: &str = "CAST(source_revision AS TEXT),source_version,CAST(tracks_version AS TEXT),audio_json,chapters_json,CAST(format_duration_ms AS TEXT)";
const LIBRARY_COLUMNS: &str =
    "CAST(detect_intros AS TEXT),CAST(enable_fingerprint AS TEXT),lifecycle_state";

impl EvidenceGuard {
    pub(super) fn identity(&self) -> Result<String, StoreError> {
        let mut values = self.values.clone();
        if self.id.starts_with("sample:") {
            if let Some(raw) = values.get_mut(8).and_then(Option::as_mut) {
                *raw = serde_json::to_string(&semantic_metrics(serde_json::from_str(raw)?))?;
            }
        } else if self.id.starts_with("cache:") {
            for index in [4, 5] {
                if let Some(raw) = values.get_mut(index).and_then(Option::as_mut) {
                    let mut capture: marker::CapturedFingerprint = serde_json::from_str(raw)?;
                    capture.metrics = semantic_metrics(capture.metrics);
                    *raw = serde_json::to_string(&capture)?;
                }
            }
        } else if self.id.starts_with("details:") && !values.is_empty() {
            if values[4].is_none() {
                values[4] = Some("[]".into());
            }
            if let Some(raw) = values.get_mut(4).and_then(Option::as_mut) {
                let chapters: Vec<library::ChapterMarker> = serde_json::from_str(raw)?;
                *raw = serde_json::to_string(
                    &chapters
                        .into_iter()
                        .filter(|chapter| !chapter.synthetic)
                        .collect::<Vec<_>>(),
                )?;
            }
        }
        digest(&(&self.id, values))
    }
}

fn semantic_metrics(metrics: marker::CaptureMetrics) -> marker::CaptureMetrics {
    marker::CaptureMetrics {
        quantized_mapping: metrics.quantized_mapping,
        sample_count: metrics.sample_count,
        pcm_bytes: metrics.pcm_bytes,
        pcm_frame_ms: metrics.pcm_frame_ms,
        pcm_rms_dbfs_milli: metrics.pcm_rms_dbfs_milli,
        audio_stream_index: metrics.audio_stream_index,
        source_start_pts_ms: metrics.source_start_pts_ms,
        contiguous_pcm_start_ms: metrics.contiguous_pcm_start_ms,
        pcm_timeline: metrics.pcm_timeline,
        first_decoded_source_pts: metrics.first_decoded_source_pts,
        decoded_time_base: metrics.decoded_time_base,
        source_facts: metrics.source_facts,
        time_mapping_version: metrics.time_mapping_version,
        ..Default::default()
    }
}

impl crate::LibraryConnection<'_> {
    pub(super) fn analysis_evidence(
        &self,
        request: &ProbeAnalysisRequest,
    ) -> Result<Vec<EvidenceGuard>, StoreError> {
        let file = request.target.file.file_id.to_string();
        let library = self.library.query_row(
            &format!("SELECT {LIBRARY_COLUMNS} FROM libraries WHERE library_id=?1"),
            [request.target.file.library_id.to_string()],
            |row| {
                Ok(EvidenceGuard {
                    id: format!("library:{}", request.target.file.library_id),
                    values: (0..3).map(|i| row.get(i)).collect::<Result<_, _>>()?,
                })
            },
        )?;
        let details = self
            .library
            .query_row(
                &format!("SELECT {DETAILS_COLUMNS} FROM file_details WHERE file_id=?1"),
                [&file],
                |row| {
                    Ok(EvidenceGuard {
                        id: format!("details:{file}"),
                        values: (0..6).map(|i| row.get(i)).collect::<Result<_, _>>()?,
                    })
                },
            )
            .optional()?
            .unwrap_or_else(|| EvidenceGuard {
                id: format!("details:{file}"),
                values: vec![],
            });
        if request.sampling_mode == "full_window" {
            let guard = self
                .library
                .query_row(
                    &format!("SELECT {CACHE_COLUMNS} FROM fingerprint_cache WHERE file_id=?1"),
                    [&file],
                    |row| {
                        Ok(EvidenceGuard {
                            id: format!("cache:{file}"),
                            values: (0..6).map(|i| row.get(i)).collect::<Result<_, _>>()?,
                        })
                    },
                )
                .optional()?;
            return guard
                .map(|guard| vec![guard, details, library])
                .ok_or_else(|| StoreError::Missing("analysis capture missing".into()));
        }
        let mut statement = self.library.prepare(&format!(
            "SELECT sample_id,{SAMPLE_COLUMNS}
            FROM fingerprint_samples WHERE file_id=?1 AND source_version=?2 ORDER BY rowid DESC"
        ))?;
        let mut rows = statement
            .query_map(
                params![file, request.target.file.version.source_version],
                |row| {
                    Ok(EvidenceGuard {
                        id: format!("sample:{}", row.get::<_, String>(0)?),
                        values: (1..10).map(|i| row.get(i)).collect::<Result<_, _>>()?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.is_empty() {
            return Err(StoreError::Missing("analysis samples missing".into()));
        }
        rows.push(details);
        rows.push(library);
        Ok(rows)
    }

    pub(super) fn analysis_evidence_matches(
        &self,
        guards: &[EvidenceGuard],
    ) -> Result<bool, StoreError> {
        for guard in guards {
            let (table, column, columns, id) = if let Some(id) = guard.id.strip_prefix("cache:") {
                ("fingerprint_cache", "file_id", CACHE_COLUMNS, id)
            } else if let Some(id) = guard.id.strip_prefix("sample:") {
                ("fingerprint_samples", "sample_id", SAMPLE_COLUMNS, id)
            } else if let Some(id) = guard.id.strip_prefix("details:") {
                ("file_details", "file_id", DETAILS_COLUMNS, id)
            } else if let Some(id) = guard.id.strip_prefix("library:") {
                ("libraries", "library_id", LIBRARY_COLUMNS, id)
            } else {
                return Ok(false);
            };
            let current = self
                .library
                .query_row(
                    &format!("SELECT {columns} FROM {table} WHERE {column}=?1"),
                    [id],
                    |row| {
                        (0..guard.values.len())
                            .map(|i| row.get::<_, Option<String>>(i))
                            .collect::<Result<Vec<_>, _>>()
                    },
                )
                .optional()?;
            if guard.values.is_empty() {
                if current.is_some() {
                    return Ok(false);
                }
            } else if current.as_ref() != Some(&guard.values) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
