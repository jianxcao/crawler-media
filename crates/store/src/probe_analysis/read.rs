use super::*;
use crate::{FingerprintCacheEntry, StoredFingerprintSample};

/// Owned raw facts from one Library snapshot. Decoding and semantic identity
/// preparation must happen after the enclosing snapshot has been released.
pub struct ProbeAnalysisRead {
    request: ProbeAnalysisRequest,
    guards: Vec<evidence::EvidenceGuard>,
    samples: Vec<crate::fingerprint_samples::FingerprintSampleRead>,
    active_context: Option<String>,
}

pub struct ProbeAnalysisFile {
    pub participant: ProbeAnalysisParticipant,
    pub samples: Vec<StoredFingerprintSample>,
    pub cache: Option<FingerprintCacheEntry>,
    pub active_context: Option<ProbeAnalysisContext>,
    pub duration_ms: Option<i64>,
    pub audio: Vec<library::AudioTrack>,
    pub chapters: Vec<library::ChapterMarker>,
}

impl ProbeAnalysisRead {
    pub fn prepare(mut self) -> Result<ProbeAnalysisFile, StoreError> {
        let details = self
            .guards
            .iter()
            .find(|guard| guard.id.starts_with("details:"));
        let detail = |index| {
            details
                .and_then(|guard| guard.values.get(index))
                .and_then(Option::as_deref)
        };
        let current_metadata = detail(1)
            == Some(self.request.target.file.version.source_version.as_str())
            && detail(2)
                .and_then(|raw| raw.parse::<i64>().ok())
                .is_some_and(|version| version >= crate::library::CURRENT_TRACKS_VERSION);
        let duration_ms = if current_metadata {
            detail(5)
                .map(|raw| {
                    raw.parse::<i64>()
                        .map_err(|e| StoreError::Invalid(e.to_string()))
                })
                .transpose()?
                .filter(|value| *value > 0)
        } else {
            None
        };
        let audio = if current_metadata {
            detail(3)
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_default()
        } else {
            vec![]
        };
        let chapters = detail(4)
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or_default();
        let cache = self
            .guards
            .iter()
            .find(|guard| guard.id.starts_with("cache:"))
            .map(|guard| {
                let value = |index| {
                    guard
                        .values
                        .get(index)
                        .and_then(Option::as_deref)
                        .ok_or_else(|| StoreError::Invalid("analysis cache field missing".into()))
                };
                Ok::<_, StoreError>(FingerprintCacheEntry {
                    cache_key: value(0)?.to_owned(),
                    algorithm_version: value(1)?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| StoreError::Invalid(e.to_string()))?,
                    sample_duration_secs: value(2)?
                        .parse()
                        .map_err(|e: std::num::ParseIntError| StoreError::Invalid(e.to_string()))?,
                    media_duration_ms: guard.values[3]
                        .as_deref()
                        .map(|v| {
                            v.parse().map_err(|e: std::num::ParseIntError| {
                                StoreError::Invalid(e.to_string())
                            })
                        })
                        .transpose()?,
                    intro: serde_json::from_str(value(4)?)?,
                    outro: guard.values[5]
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                })
            })
            .transpose()?;
        let active_context = self
            .active_context
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        if self.request.required_kinds.is_empty()
            && let Some(cache) = &cache
        {
            self.request.capture_context_key = cache.cache_key.clone();
        }
        let samples = self
            .samples
            .into_iter()
            .map(|sample| sample.decode())
            .collect::<Result<Vec<_>, _>>()?;
        let participant = prepare_participant(&self.request, self.guards)?;
        Ok(ProbeAnalysisFile {
            participant,
            samples,
            cache,
            active_context,
            duration_ms,
            audio,
            chapters,
        })
    }
}

impl Store {
    pub fn read_probe_analysis(
        &self,
        request: &ProbeAnalysisRequest,
    ) -> Result<ProbeAnalysisRead, StoreError> {
        use rusqlite::OptionalExtension;
        self.with_library_read(|db| {
            let _snapshot = crate::read_tx(db.library)?;
            let guards = db.analysis_evidence(request)?;
            let samples = guards
                .iter()
                .filter_map(|guard| guard.id.strip_prefix("sample:"))
                .map(|id| {
                    db.fingerprint_sample_read(id)?
                        .ok_or_else(|| StoreError::Missing("analysis sample disappeared".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let active_context = db
                .library
                .query_row(
                    "SELECT analysis_context_json FROM probe_job_units
                WHERE file_id=?1 AND stage='analysis' AND status IN ('queued','running')",
                    [request.target.file.file_id.to_string()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            Ok(ProbeAnalysisRead {
                request: request.clone(),
                guards,
                samples,
                active_context,
            })
        })
    }
}

pub(super) fn prepare_participant(
    request: &ProbeAnalysisRequest,
    guards: Vec<evidence::EvidenceGuard>,
) -> Result<ProbeAnalysisParticipant, StoreError> {
    let mut context = ProbeAnalysisContext {
        context_key: String::new(),
        media_id: request.target.file.media_id,
        target: request.target.clone(),
        season: request.season,
        sampling_mode: request.sampling_mode.clone(),
        analysis_policy_key: request.analysis_policy_key.clone(),
        analysis_policy_json: request.analysis_policy_json.clone(),
        capture_context_key: request.capture_context_key.clone(),
        required_kinds: request.required_kinds.clone(),
        evidence: guards
            .iter()
            .map(|guard| Ok((guard.id.clone(), guard.identity()?)))
            .collect::<Result<_, StoreError>>()?,
    };
    context.context_key = digest(&context)?;
    let encoded = serde_json::to_string(&context)?;
    Ok(ProbeAnalysisParticipant {
        dispatch: request.dispatch.clone(),
        context,
        encoded,
        guards,
    })
}
