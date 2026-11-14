use super::*;
type RawModels = Vec<(
    String,
    Result<Option<store::FingerprintModelRead>, store::StoreError>,
)>;

pub(super) fn load(
    manager: &ProbeManager,
    key: SeasonKey,
    anchor: &ProbeAnalysisParticipant,
    rows: &[domain::FileCoverage],
) -> Result<
    (
        Vec<store::ProbeAnalysisFile>,
        marker::adaptive::TemplateContext,
    ),
    store::StoreError,
> {
    let policy: marker::SamplingPolicy =
        serde_json::from_str(&anchor.context().analysis_policy_json)?;
    // Only owned raw facts/payloads leave this one consistent Library snapshot.
    let (raw_files, raw_models) = manager.store.with_read_snapshot(
        &[store::Db::Library],
        crate::http::store_wait::STORE_READ_WAIT,
        || {
            let mut files = Vec::new();
            for row in rows {
                let request = store::ProbeAnalysisRequest {
                    dispatch: store::ProbeDispatchId {
                        job_id: anchor.dispatch().job_id.clone(),
                        ledger_id: row.file.file_id.to_string(),
                        stage: store::ProbeDispatchStage::Analysis,
                    },
                    target: row.clone(),
                    season: key.season,
                    sampling_mode: anchor.context().sampling_mode.clone(),
                    analysis_policy_key: anchor.context().analysis_policy_key.clone(),
                    analysis_policy_json: anchor.context().analysis_policy_json.clone(),
                    capture_context_key: String::new(),
                    required_kinds: vec![],
                };
                match manager.store.read_probe_analysis(&request) {
                    Ok(raw) => files.push(raw),
                    Err(store::StoreError::Missing(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            let mut models = Vec::new();
            if anchor.context().sampling_mode == "adaptive" {
                for kind in ["intro", "outro"] {
                    for model in manager.store.fingerprint_models_page(
                        &key.media_id.to_string(),
                        key.season,
                        kind,
                        &anchor.context().analysis_policy_key,
                        None,
                        policy.max_templates_per_kind,
                    )? {
                        models.push((
                            model.model_id.clone(),
                            manager.store.read_fingerprint_model_bundle(&model.model_id),
                        ));
                    }
                }
            }
            Ok::<_, store::StoreError>((files, models))
        },
    )??;
    // No database lease remains during parsing, validation, identity hashing or serialization.
    let files = raw_files
        .into_iter()
        .map(|raw| raw.prepare())
        .collect::<Result<Vec<_>, _>>()?;
    let templates = prepare_models(manager, raw_models, &files)?;
    Ok((files, templates))
}

fn prepare_models(
    manager: &ProbeManager,
    raw_models: RawModels,
    files: &[store::ProbeAnalysisFile],
) -> Result<marker::adaptive::TemplateContext, store::StoreError> {
    let mut templates = marker::adaptive::TemplateContext::default();
    for (id, raw) in raw_models {
        let bundle = match raw.and_then(|raw| raw.map(|raw| raw.prepare()).transpose()) {
            Ok(Some(bundle)) => bundle,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(%error,model_id=%id,"模板引用失效，重建当前证据");
                continue;
            }
        };
        let template: marker::adaptive::TemplateModel =
            serde_json::from_str(&bundle.model.model_json)?;
        let mut references = Vec::new();
        for sample in &bundle.samples {
            let Some(file) = files
                .iter()
                .find(|file| file.participant.dispatch().ledger_id == sample.ledger_id)
            else {
                break;
            };
            let profile = crate::fingerprint_job::capture_profile_key(
                &crate::fingerprint_job::profile_for_source(source(manager, file).as_ref()),
            );
            if sample.source_version
                != file
                    .participant
                    .context()
                    .target
                    .file
                    .version
                    .source_version
                || sample.capture_profile_key != profile
            {
                break;
            }
            let reference = template
                .references
                .iter()
                .find(|r| r.sample_id == sample.sample_id)
                .ok_or_else(|| store::StoreError::Invalid("template member missing".into()))?;
            references.push(crate::fingerprint_job::adaptive::stored_sample_to_evidence(
                sample,
                reference.episode,
                template.kind,
                file.duration_ms,
            ));
        }
        if references.len() == bundle.samples.len() {
            templates
                .references
                .extend(references.into_iter().map(|e| (e.sample_id.clone(), e)));
            templates.models.push(template);
        }
    }
    Ok(templates)
}
