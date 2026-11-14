//! Merge independent kind decisions using only accepted values for this source version.
use super::*;
use marker::SegmentKind;
use serde::Serialize;

#[derive(Serialize)]
struct KindOutcome {
    decision: &'static str,
    status: &'static str,
    reason: Option<String>,
    retained_previous: bool,
}

pub(super) fn output(
    manager: &ProbeManager,
    participant: &ProbeAnalysisParticipant,
    inputs: &Inputs,
    decisions: &[marker::matcher::EpisodeFingerprintDecision],
    references: &[ProbeAnalysisParticipant],
    model_reference_key: &[String],
) -> Result<ProbeAnalysisOutput, store::StoreError> {
    let row = &participant.context().target;
    let id = row.file.file_id.to_string();
    let old = manager
        .store
        .get_file_marker(row.file.file_id, row.units[0])?
        .filter(|old| old.version == row.file.version);
    let mut marker = store::StoredFileMarker {
        file_id: row.file.file_id,
        version: row.file.version.clone(),
        unit: row.units[0],
        intro_start_ms: old.as_ref().and_then(|m| m.intro_start_ms),
        intro_end_ms: old.as_ref().and_then(|m| m.intro_end_ms),
        outro_start_ms: old.as_ref().and_then(|m| m.outro_start_ms),
        outro_end_ms: old.as_ref().and_then(|m| m.outro_end_ms),
        source: "fingerprint".into(),
        locked: false,
        updated_at: super::super::super::now_ms() / 1000,
    };
    let mut outcomes = std::collections::BTreeMap::new();
    let mut resolved = 0;
    for (kind, name) in [(SegmentKind::Intro, "intro"), (SegmentKind::Outro, "outro")] {
        let decision = decisions
            .iter()
            .find(|d| d.ledger_id == id && d.kind == kind);
        let outcome = merge_kind(&mut marker, old.as_ref(), kind, decision, inputs);
        resolved += usize::from(outcome.status != "uncertain");
        outcomes.insert(name, outcome);
    }
    let replacement = (resolved > 0).then(|| replacement(participant, inputs, marker));
    let reasons = outcomes
        .values()
        .filter_map(|outcome| outcome.reason.as_deref())
        .collect::<Vec<_>>();
    let detection_outcome_json = serde_json::to_string(&serde_json::json!({
        "status": match resolved { 0 => "uncertain", 2 => "verified", _ => "partial" },
        "reason": (!reasons.is_empty()).then(|| reasons.join("; ")), "kinds": outcomes,
        "available_reference_key": inputs.reference_key, "analysis_policy_key": participant.context().analysis_policy_key,
        "source_version": row.file.version.source_version, "source_revision": row.file.version.revision,
        "reference_evidence": references.iter().map(|r| (&r.dispatch().ledger_id, &r.context().context_key)).collect::<Vec<_>>(),
        "model_reference_key": model_reference_key,
    }))?;
    Ok(ProbeAnalysisOutput {
        dispatch: participant.dispatch().clone(),
        detection_outcome_json,
        replacement,
    })
}

fn merge_kind(
    marker: &mut store::StoredFileMarker,
    old: Option<&store::StoredFileMarker>,
    kind: SegmentKind,
    selected: Option<&marker::matcher::EpisodeFingerprintDecision>,
    inputs: &Inputs,
) -> KindOutcome {
    let decision = selected.map(|selected| &selected.decision);
    let outcome = |status, reason| KindOutcome {
        decision: match decision {
            Some(FingerprintDecision::Verified(_)) => "verified",
            Some(FingerprintDecision::NoMatch) => "no_match",
            Some(FingerprintDecision::Uncertain(_)) => "uncertain",
            None => "missing",
        },
        status,
        reason,
        retained_previous: old.is_some_and(|old| {
            old.locked
                || status == "uncertain"
                    && match kind {
                        SegmentKind::Intro => {
                            old.intro_start_ms.is_some() || old.intro_end_ms.is_some()
                        }
                        SegmentKind::Outro => {
                            old.outro_start_ms.is_some() || old.outro_end_ms.is_some()
                        }
                    }
        }),
    };
    let bounds = match decision {
        Some(FingerprintDecision::Verified(interval)) => {
            (Some(interval.start_ms), Some(interval.end_ms))
        }
        Some(FingerprintDecision::NoMatch) => {
            let capture = inputs
                .evidence
                .iter()
                .find(|e| {
                    selected.is_some_and(|decision| e.sample_id == decision.sample_id)
                        && e.ledger_id == marker.file_id.to_string()
                        && e.kind == kind
                })
                .map(|e| &e.capture);
            if let Some(previous) = old
                && let Err(error) =
                    super::super::super::markers::validate_absence_coverage(previous, kind, capture)
            {
                return outcome("uncertain", Some(error.to_string()));
            }
            (None, None)
        }
        Some(FingerprintDecision::Uncertain(reason)) => {
            return outcome(
                "uncertain",
                Some(format!("verification_unresolved: {kind:?} {reason}")),
            );
        }
        None => {
            return outcome(
                "uncertain",
                Some(format!(
                    "verification_unresolved: {kind:?} missing_evidence"
                )),
            );
        }
    };
    match kind {
        SegmentKind::Intro => (marker.intro_start_ms, marker.intro_end_ms) = bounds,
        SegmentKind::Outro => (marker.outro_start_ms, marker.outro_end_ms) = bounds,
    }
    outcome(
        if matches!(decision, Some(FingerprintDecision::NoMatch)) {
            "no_match"
        } else {
            "verified"
        },
        None,
    )
}

fn replacement(
    participant: &ProbeAnalysisParticipant,
    inputs: &Inputs,
    marker: store::StoredFileMarker,
) -> store::MarkerResultReplacement {
    let row = &participant.context().target;
    let id = row.file.file_id.to_string();
    let intro = marker.intro_start_ms.zip(marker.intro_end_ms);
    let outro = marker.outro_start_ms.zip(marker.outro_end_ms);
    let chapters = marker::build_complete_timeline_chapters(
        inputs.chapters.get(&id).map(Vec::as_slice).unwrap_or(&[]),
        intro,
        outro,
        None,
    );
    // A preserved kind can legitimately have only one bound (for example an
    // accepted Outro start). Keep that marker fact even without a chapter pair.
    let has_marker = marker.intro_start_ms.is_some()
        || marker.intro_end_ms.is_some()
        || marker.outro_start_ms.is_some()
        || marker.outro_end_ms.is_some();
    store::MarkerResultReplacement {
        media_id: row.file.media_id,
        season: participant.context().season,
        targets: vec![row.clone()],
        markers: if has_marker { vec![marker] } else { vec![] },
        chapter_updates: vec![(id, chapters)],
    }
}
