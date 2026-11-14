use super::*;

pub(super) fn load(
    manager: &ProbeManager,
    key: SeasonKey,
    policy: &marker::SamplingPolicy,
    observed: Option<&[domain::FileCoverage]>,
) -> Result<marker::adaptive::TemplateContext, store::StoreError> {
    let mut templates = marker::adaptive::TemplateContext::default();
    let policy_key = crate::fingerprint_job::analysis_policy_key(policy);
    for kind in ["intro", "outro"] {
        let candidates = manager.store.fingerprint_models_page(
            &key.media_id.to_string(),
            key.season,
            kind,
            &policy_key,
            None,
            policy.max_templates_per_kind,
        )?;
        for model in candidates {
            let bundle = match manager.store.load_fingerprint_model_bundle(&model.model_id) {
                Ok(Some(bundle)) => bundle,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error,model_id=%model.model_id,"模板引用失效，重建当前证据");
                    continue;
                }
            };
            let template: marker::adaptive::TemplateModel =
                serde_json::from_str(&bundle.model.model_json)?;
            let mut valid = true;
            let mut references = Vec::new();
            for sample in &bundle.samples {
                let Some(row) = manager
                    .store
                    .file_coverage_by_id(sample.ledger_id.parse()?)?
                else {
                    valid = false;
                    break;
                };
                let source_current = match observed {
                    Some(rows) => rows.iter().any(|known| {
                        known.file.file_id == row.file.file_id
                            && known.file.version == row.file.version
                    }),
                    None => crate::fingerprint_job::source::observe_source(&row).is_ok(),
                };
                if !source_current {
                    valid = false;
                    break;
                }
                let source = manager.capture_source(&row.file);
                let profile = crate::fingerprint_job::capture_profile_key(
                    &crate::fingerprint_job::profile_for_source(source.as_ref()),
                );
                if sample.capture_profile_key != profile {
                    valid = false;
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
                    metadata_duration(manager, &row)?,
                ));
            }
            if valid {
                for reference in references {
                    templates
                        .references
                        .insert(reference.sample_id.clone(), reference);
                }
                templates.models.push(template);
            }
        }
    }
    Ok(templates)
}
