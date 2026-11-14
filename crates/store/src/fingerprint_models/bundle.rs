use super::*;
use crate::StoredFingerprintSample;
use marker::adaptive::{TEMPLATE_MODEL_VERSION, TemplateModel};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct StoredFingerprintModelBundle {
    data: FingerprintModelBundleData,
    pub(crate) sample_payloads: Vec<String>,
    pub(crate) bindings: Vec<ModelReferenceBinding>,
}

#[derive(Clone, Debug)]
pub struct FingerprintModelBundleData {
    pub model: StoredFingerprintModel,
    pub members: Vec<StoredFingerprintModelMember>,
    pub samples: Vec<StoredFingerprintSample>,
}

impl std::ops::Deref for StoredFingerprintModelBundle {
    type Target = FingerprintModelBundleData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ModelReferenceBinding {
    pub file: String,
    pub episode: u32,
    pub revision: i64,
    pub source: String,
    pub tracks_version: i64,
    pub audio: String,
}

impl StoredFingerprintModelBundle {
    /// Canonicalize actual training references, never the set of output targets.
    pub fn new(
        media_id: domain::MediaId,
        season: u32,
        policy_key: String,
        mut template: TemplateModel,
        mut samples: Vec<StoredFingerprintSample>,
        created_at_ms: i64,
    ) -> Result<Self, StoreError> {
        template
            .references
            .sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
        samples.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
        let sample_payloads = samples
            .iter()
            .map(|sample| serde_json::to_string(&sample.fingerprint).map_err(Into::into))
            .collect::<Result<Vec<_>, StoreError>>()?;
        let identities = samples
            .iter()
            .map(sample_identity)
            .collect::<Result<Vec<_>, _>>()?;
        let membership_key = hash(&serde_json::to_vec(&identities)?);
        template.model_id.clear();
        let model_id = hash(&serde_json::to_vec(&(
            media_id,
            season,
            &policy_key,
            &membership_key,
            &template,
        ))?);
        template.model_id = model_id.clone();
        let model = StoredFingerprintModel {
            model_id: model_id.clone(),
            media_id: media_id.to_string(),
            season,
            kind: match template.kind {
                marker::SegmentKind::Intro => "intro",
                marker::SegmentKind::Outro => "outro",
            }
            .into(),
            model_version: template.version,
            membership_key,
            policy_key,
            model_json: serde_json::to_string(&template)?,
            created_at_ms,
        };
        let members = samples
            .iter()
            .map(|sample| StoredFingerprintModelMember {
                model_id: model_id.clone(),
                sample_id: sample.sample_id.clone(),
                ledger_id: sample.ledger_id.clone(),
                source_version: sample.source_version.clone(),
            })
            .collect();
        let result = Self {
            data: FingerprintModelBundleData {
                model,
                members,
                samples,
            },
            sample_payloads,
            bindings: vec![],
        };
        result.validate(&template)?;
        Ok(result)
    }

    fn validate(&self, template: &TemplateModel) -> Result<(), StoreError> {
        if template.version != TEMPLATE_MODEL_VERSION
            || template.references.len() != self.samples.len()
            || template.expected_duration_ms <= 0
        {
            return Err(rejected_model(&self.model, "invalid template header"));
        }
        let mut episodes = std::collections::BTreeSet::new();
        let mut samples = std::collections::BTreeSet::new();
        for reference in &template.references {
            let sample = self
                .samples
                .iter()
                .find(|sample| sample.sample_id == reference.sample_id)
                .ok_or_else(|| rejected_model(&self.model, "missing exact reference sample"))?;
            let (start, end) = reference.match_interval_ms;
            if !samples.insert(&reference.sample_id)
                || sample.ledger_id != reference.ledger_id
                || sample.source_version != reference.source_version
                || sample.kind != self.model.kind
                || sample.capture_profile_key.is_empty()
                || sample.fingerprint.is_empty()
                || start < sample.window_start_ms
                || end > sample.window_end_ms
                || end <= start
            {
                return Err(rejected_model(
                    &self.model,
                    "invalid reference sample or interval",
                ));
            }
            let metrics: marker::CaptureMetrics = serde_json::from_str(&sample.metrics_json)?;
            let profile =
                marker::capture_profile::profile_for_source(metrics.source_facts.as_ref());
            let capture = marker::CapturedFingerprint {
                window: marker::SampleWindow::new(sample.window_start_ms, sample.window_end_ms),
                words: vec![],
                pcm_duration_ms: sample.pcm_duration_ms,
                metrics,
            };
            if capture.metrics.source_facts.is_none()
                || capture.metrics.audio_stream_index != profile.audio_stream_index
                || sample.capture_profile_key
                    != marker::capture_profile::capture_profile_key(&profile)
                || capture.metrics.time_mapping_version != profile.time_mapping_version
                || capture.metrics.pcm_timeline != marker::PcmTimeline::Contiguous
                || marker::boundary::evidence::candidate_pcm_coverage(&capture)
                    .is_none_or(|coverage| coverage.start_ms > start || coverage.end_ms < end)
            {
                return Err(rejected_model(
                    &self.model,
                    "invalid reference capture facts",
                ));
            }
            episodes.insert(reference.episode);
        }
        if episodes.len() < 2 || (template.is_stable && episodes.len() < 3) {
            return Err(rejected_model(
                &self.model,
                "insufficient independent episode references",
            ));
        }
        Ok(())
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sample_identity(sample: &StoredFingerprintSample) -> Result<String, StoreError> {
    let metrics: marker::CaptureMetrics = serde_json::from_str(&sample.metrics_json)?;
    Ok(hash(&serde_json::to_vec(&(
        &sample.sample_id,
        &sample.ledger_id,
        &sample.source_version,
        &sample.capture_profile_key,
        &sample.kind,
        sample.window_start_ms,
        sample.window_end_ms,
        sample.pcm_duration_ms,
        &sample.fingerprint,
        (
            metrics.audio_stream_index,
            metrics.time_mapping_version,
            metrics.pcm_frame_ms,
            metrics.pcm_rms_dbfs_milli,
            metrics.pcm_timeline,
            metrics.contiguous_pcm_start_ms,
            metrics.source_facts,
            metrics.source_start_pts_ms,
            metrics.first_decoded_source_pts,
            metrics.decoded_time_base,
            metrics.quantized_mapping,
            metrics.sample_count,
            metrics.pcm_bytes,
        ),
    ))?))
}

impl Store {
    pub fn prepare_fingerprint_model(
        &self,
        model: &StoredFingerprintModel,
        members: &[StoredFingerprintModelMember],
    ) -> Result<StoredFingerprintModelBundle, StoreError> {
        let samples = self.with_library_read(|db| {
            let _snapshot = crate::read_tx(db.library)?;
            members
                .iter()
                .map(|member| {
                    db.fingerprint_sample_read(&member.sample_id)?
                        .ok_or_else(|| rejected_model(model, "reference sample missing"))
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        let samples = samples
            .into_iter()
            .map(|sample| sample.decode())
            .collect::<Result<Vec<_>, _>>()?;
        self.bind_fingerprint_model(validate_bundle(model, members, samples)?)
    }

    pub fn load_fingerprint_model_bundle(
        &self,
        model_id: &str,
    ) -> Result<Option<StoredFingerprintModelBundle>, StoreError> {
        self.read_fingerprint_model_bundle(model_id)?
            .map(|raw| raw.prepare())
            .transpose()
    }

    pub fn bind_fingerprint_model(
        &self,
        mut bundle: StoredFingerprintModelBundle,
    ) -> Result<StoredFingerprintModelBundle, StoreError> {
        let template: TemplateModel = serde_json::from_str(&bundle.model.model_json)?;
        bundle.bindings = self.with_library_read(|db| {
            let _snapshot = crate::read_tx(db.library)?;
            template
                .references
                .iter()
                .map(|reference| {
                    let sample = bundle
                        .samples
                        .iter()
                        .find(|s| s.sample_id == reference.sample_id)
                        .ok_or_else(|| rejected_model(&bundle.model, "reference sample missing"))?;
                    let binding = db.library.query_row(
                        "SELECT source_revision,source_version,tracks_version,audio_json
                    FROM file_details WHERE file_id=?1 AND video_json IS NOT NULL",
                        [&sample.ledger_id],
                        |row| {
                            Ok(ModelReferenceBinding {
                                file: sample.ledger_id.clone(),
                                episode: reference.episode,
                                revision: row.get(0)?,
                                source: row.get(1)?,
                                tracks_version: row.get(2)?,
                                audio: row.get(3)?,
                            })
                        },
                    )?;
                    db.validate_model_reference_binding(&bundle.model, &binding)?;
                    Ok(binding)
                })
                .collect::<Result<Vec<_>, StoreError>>()
        })?;
        for (reference, binding) in template.references.iter().zip(&bundle.bindings) {
            super::read::validate_audio(&bundle, binding, &reference.sample_id)?;
        }
        Ok(bundle)
    }

    pub fn fingerprint_models_page(
        &self,
        media_id: &str,
        season: u32,
        kind: &str,
        policy_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredFingerprintModel>, StoreError> {
        self.with_library_read(|db| {
            db.fingerprint_models_page(media_id, season, kind, policy_key, after, limit)
        })
    }
}

pub(super) fn validate_bundle(
    model: &StoredFingerprintModel,
    members: &[StoredFingerprintModelMember],
    samples: Vec<StoredFingerprintSample>,
) -> Result<StoredFingerprintModelBundle, StoreError> {
    let template: TemplateModel = serde_json::from_str(&model.model_json)?;
    if template.model_id != model.model_id {
        return Err(rejected_model(model, "serialized identity mismatch"));
    }
    let expected = StoredFingerprintModelBundle::new(
        model.media_id.parse()?,
        model.season,
        model.policy_key.clone(),
        template,
        samples,
        model.created_at_ms,
    )?;
    let mut sorted = members.to_vec();
    sorted.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));
    if expected.model != *model || sorted != expected.members {
        return Err(rejected_model(model, "model/reference identity mismatch"));
    }
    Ok(expected)
}
