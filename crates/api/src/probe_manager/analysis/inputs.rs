use super::*;
#[path = "inputs_read.rs"]
mod read;

pub(super) fn load_inputs(
    manager: &ProbeManager,
    key: SeasonKey,
    anchor: &ProbeAnalysisParticipant,
) -> Result<Inputs, store::StoreError> {
    let rows = observed_rows(manager, key)?;
    let (files, templates) = read::load(manager, key, anchor, &rows)?;
    let mut evidence = Vec::new();
    let mut references = Vec::new();
    let mut observed = Vec::new();
    let mut chapters = HashMap::new();
    for file in files {
        let id = file.participant.dispatch().ledger_id.clone();
        observed.push(file.participant.clone());
        let selected = if anchor.context().sampling_mode == "adaptive" {
            adaptive_evidence(manager, &file)?
        } else {
            full_evidence(manager, &file)?
        };
        if selected.is_empty() {
            continue;
        }
        let reference = if anchor.context().sampling_mode == "adaptive" {
            let ids = selected
                .iter()
                .chain(templates.references.values().filter(|e| e.ledger_id == id))
                .map(|e| e.sample_id.clone())
                .collect::<Vec<_>>();
            file.participant.select_samples(&ids)?
        } else {
            file.participant
        };
        evidence.extend(selected);
        references.push(reference);
        chapters.insert(id, file.chapters);
    }
    use sha2::{Digest, Sha256};
    let identity = references
        .iter()
        .map(|r| (&r.context().target, &r.context().evidence))
        .collect::<Vec<_>>();
    let reference_key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&(
            identity,
            &anchor.context().analysis_policy_key
        ))?)
    );
    Ok(Inputs {
        evidence,
        references,
        observed,
        chapters,
        reference_key,
        templates,
    })
}

fn observed_rows(
    manager: &ProbeManager,
    key: SeasonKey,
) -> Result<Vec<domain::FileCoverage>, store::StoreError> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let page =
            manager
                .store
                .analysis_season_files_page(key.media_id, key.season, cursor, 16)?;
        let Some(last) = page.last() else {
            break;
        };
        cursor = Some((
            crate::probe_manager::require_episode_coordinates(last)?.1 as i32,
            last.file.library_id.to_string(),
            last.file.file_id.to_string(),
        ));
        for row in page {
            match crate::fingerprint_job::source::observe_source(&row) {
                Ok(()) => rows.push(row),
                Err(reason) if crate::fingerprint_job::source::is_file_deleted(&reason) => {}
                Err(reason) => return Err(store::StoreError::Invalid(reason)),
            }
        }
    }
    Ok(rows)
}

fn source(
    manager: &ProbeManager,
    file: &store::ProbeAnalysisFile,
) -> Option<marker::CaptureSourceFacts> {
    let selected = manager
        .audio_selections
        .lock()
        .get(&file.participant.context().target.file.path)
        .copied();
    library::capture_source(
        &library::Tracks {
            audio: file.audio.clone(),
            ..Default::default()
        },
        selected,
    )
    .ok()
}

fn adaptive_evidence(
    manager: &ProbeManager,
    file: &store::ProbeAnalysisFile,
) -> Result<Vec<marker::EpisodeEvidence>, store::StoreError> {
    let source = source(manager, file);
    let profile = crate::fingerprint_job::capture_profile_key(
        &crate::fingerprint_job::profile_for_source(source.as_ref()),
    );
    let mut selected = Vec::new();
    for sample in &file.samples {
        if file.active_context.as_ref().is_some_and(|context| {
            !context
                .evidence
                .iter()
                .any(|(id, _)| id.strip_prefix("sample:") == Some(sample.sample_id.as_str()))
        }) || sample.capture_profile_key != profile
        {
            continue;
        }
        let capture = crate::fingerprint_job::adaptive::stored_sample_to_evidence(
            sample,
            0,
            marker::SegmentKind::Intro,
            file.duration_ms,
        )
        .capture;
        if source.is_none() || capture.metrics.source_facts.as_ref() != source.as_ref() {
            continue;
        }
        selected.push(sample);
    }
    selected.sort_by(|a, b| a.sample_id.cmp(&b.sample_id));

    let episode =
        crate::probe_manager::require_episode_coordinates(&file.participant.context().target)?.1;
    Ok(selected
        .into_iter()
        .map(|sample| {
            crate::fingerprint_job::adaptive::stored_sample_to_evidence(
                sample,
                episode,
                if sample.kind == "intro" {
                    marker::SegmentKind::Intro
                } else {
                    marker::SegmentKind::Outro
                },
                file.duration_ms,
            )
        })
        .collect())
}

fn full_evidence(
    manager: &ProbeManager,
    file: &store::ProbeAnalysisFile,
) -> Result<Vec<marker::EpisodeEvidence>, store::StoreError> {
    let Some(cache) = &file.cache else {
        return Ok(vec![]);
    };
    let row = &file.participant.context().target;
    let source = source(manager, file);
    let expected = crate::fingerprint_job::fingerprint_cache_key_with_profile(
        &row.file.version.source_version,
        cache.sample_duration_secs,
        cache.media_duration_ms,
        &crate::fingerprint_job::profile_for_source(source.as_ref()),
    );
    if cache.cache_key != expected {
        return Ok(vec![]);
    }
    let id = row.file.file_id.to_string();
    let episode = crate::probe_manager::require_episode_coordinates(row)?.1;
    Ok([
        (marker::SegmentKind::Intro, Some(&cache.intro)),
        (marker::SegmentKind::Outro, cache.outro.as_ref()),
    ]
    .into_iter()
    .filter_map(|(kind, capture)| {
        capture
            .filter(|capture| {
                source.is_some() && capture.metrics.source_facts.as_ref() == source.as_ref()
            })
            .map(|capture| marker::EpisodeEvidence {
                sample_id: format!("{id}:{kind:?}"),
                ledger_id: id.clone(),
                episode,
                source_version: row.file.version.source_version.clone(),
                capture_profile_key: cache.cache_key.clone(),
                kind,
                capture: capture.clone(),
                duration_ms: file.duration_ms,
            })
    })
    .collect())
}
