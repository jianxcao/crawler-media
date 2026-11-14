use super::bundle::ModelReferenceBinding;
use super::*;

/// Raw model, exact sample payloads and current bindings from one reader snapshot.
pub struct FingerprintModelRead {
    model: StoredFingerprintModel,
    members: Vec<StoredFingerprintModelMember>,
    samples: Vec<crate::fingerprint_samples::FingerprintSampleRead>,
    bindings: Vec<ModelReferenceBinding>,
}

impl FingerprintModelRead {
    pub fn prepare(self) -> Result<StoredFingerprintModelBundle, StoreError> {
        let payloads = self
            .samples
            .iter()
            .map(|sample| sample.fingerprint_json.clone())
            .collect::<Vec<_>>();
        let samples = self
            .samples
            .into_iter()
            .map(|sample| sample.decode())
            .collect::<Result<Vec<_>, _>>()?;
        let mut bundle = super::bundle::validate_bundle(&self.model, &self.members, samples)?;
        let template: marker::adaptive::TemplateModel =
            serde_json::from_str(&bundle.model.model_json)?;
        for reference in &template.references {
            let binding = self
                .bindings
                .iter()
                .find(|binding| {
                    binding.file == reference.ledger_id && binding.episode == reference.episode
                })
                .ok_or_else(|| {
                    rejected_model(
                        &bundle.model,
                        "reference episode coverage or metadata changed",
                    )
                })?
                .clone();
            validate_audio(&bundle, &binding, &reference.sample_id)?;
            bundle.bindings.push(binding);
        }
        bundle.sample_payloads = payloads;
        Ok(bundle)
    }
}

pub(super) fn validate_audio(
    bundle: &StoredFingerprintModelBundle,
    binding: &ModelReferenceBinding,
    sample_id: &str,
) -> Result<(), StoreError> {
    let sample = bundle
        .samples
        .iter()
        .find(|sample| sample.sample_id == sample_id && sample.ledger_id == binding.file)
        .ok_or_else(|| rejected_model(&bundle.model, "reference sample missing"))?;
    let metrics: marker::CaptureMetrics = serde_json::from_str(&sample.metrics_json)?;
    let tracks = library::Tracks {
        audio: serde_json::from_str(&binding.audio)?,
        ..Default::default()
    };
    let source = library::capture_source(&tracks, metrics.audio_stream_index)
        .map_err(StoreError::Invalid)?;
    if metrics.source_facts.as_ref() != Some(&source)
        || binding.source != sample.source_version
        || binding.tracks_version < crate::library::CURRENT_TRACKS_VERSION
    {
        return Err(rejected_model(
            &bundle.model,
            "reference audio selection or source facts changed",
        ));
    }
    Ok(())
}

impl Store {
    pub fn read_fingerprint_model_bundle(
        &self,
        id: &str,
    ) -> Result<Option<FingerprintModelRead>, StoreError> {
        self.with_library_read(|db| {
            let _snapshot = crate::read_tx(db.library)?;
            let Some((model, members, samples)) = db.fingerprint_model_bundle_rows(id)? else { return Ok(None); };
            let mut bindings = Vec::new();
            for member in &members {
                let mut statement = db.library.prepare("SELECT d.source_revision,d.source_version,d.tracks_version,d.audio_json,u.episode
                    FROM file_details d JOIN library_files f ON f.file_id=d.file_id
                    JOIN library_file_units u ON u.file_id=f.file_id WHERE d.file_id=?1 AND d.video_json IS NOT NULL
                    AND d.source_revision=f.file_revision AND d.source_version=f.source_version
                    AND u.media_id=?2 AND u.season=?3 AND u.episode>=0")?;
                bindings.extend(statement.query_map(params![member.ledger_id,model.media_id,model.season], |row| Ok(ModelReferenceBinding {
                    file: member.ledger_id.clone(), revision: row.get(0)?, source: row.get(1)?,
                    tracks_version: row.get(2)?, audio: row.get(3)?, episode: row.get(4)?,
                }))?.collect::<Result<Vec<_>, _>>()?);
            }
            Ok(Some(FingerprintModelRead { model, members, samples, bindings }))
        })
    }
}
