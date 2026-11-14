//! Probing and replacement-safety arithmetic: the quality a file actually has,
//! and whether an arrival may replace a file the Library already owns.

use std::path::Path;

use domain::{Confidence, QualitySource, Release};
use downloader::Downloader;
use filter::ScoredTorrent;
use library::MediaProbe;

use crate::add::Added;

pub(super) fn replacement_survives_probe<D: Downloader + ?Sized>(
    added: &Added<'_, D>,
    scored: &ScoredTorrent,
    slots: &[(Option<u32>, Option<u32>)],
    approved: bool,
    quality: &library::FileQuality,
    was_probed: bool,
) -> (bool, i32) {
    if !added.input.subscribe.wash_cut {
        return (approved, scored.score);
    }
    // S1: deleting an owned file needs evidence about the file that replaces
    // it. Without a successful probe the quality is only the torrent title, so
    // never approve replacing an owned slot; keep the arrival as an alternate
    // (score 0) and let the probe retry decide later.
    let owns_file = slots.iter().any(|(s, e)| {
        added
            .input
            .facts
            .get(*s, *e)
            .is_some_and(|fact| fact.path.is_some())
    });
    if owns_file && scored.release.season_inferred {
        // G-H2: the season was guessed from a title without season marker;
        // that is not enough evidence to delete an owned file.
        tracing::warn!(torrent = %scored.torrent.title, "季号由解析器推断，拒绝洗版替换在位文件");
        return (false, 0);
    }
    if owns_file && !was_probed {
        tracing::warn!(
            torrent = %scored.torrent.title,
            "新文件 probe 失败，仅凭标题无法证明是升级，拒绝洗版替换并保留在位文件"
        );
        return (false, 0);
    }
    let mut release = scored.release.clone();
    release.resolution = quality.resolution.clone();
    release.codec = quality.codec.clone();
    release.hdr = quality.hdr.clone();
    let effective_filter = added.input.wash_filter.unwrap_or(added.input.filter);
    let admission = filter::admit_scored(
        vec![(scored.torrent.clone(), Some(release))],
        effective_filter,
    );
    let Some(probed) = admission.admitted.first() else {
        return (false, 0);
    };
    // Score must remain on the Filter's scale. Also reject physical downgrades
    // when no explicit ladder is configured, regardless of site/title scores.
    let safe_quality = !was_probed
        || crate::choose::collected_quality_is_safe(
            effective_filter,
            &added.input.facts,
            &probed.release,
            slots,
        );
    let replacement = approved
        && safe_quality
        && crate::slot_replacement::is_replacement_approved(
            added.input.subscribe,
            Some(effective_filter),
            &added.input.facts,
            probed,
            slots,
        );
    // Alternate rejected versions must not outrank owned facts when the ledger
    // is reloaded after restart. Fresh fill slots still keep their actual score.
    let owns_any = slots
        .iter()
        .any(|(s, e)| added.input.facts.get(*s, *e).is_some());
    (
        replacement,
        if owns_any && !replacement {
            0
        } else {
            probed.score
        },
    )
}

pub(super) fn quality_for(
    probe: &dyn MediaProbe,
    src: &Path,
    release: &Release,
    control: &library::WorkControl,
) -> (library::FileQuality, QualitySource, Confidence) {
    if let Ok(quality) = probe.probe_controlled(src, control) {
        (quality, QualitySource::Probe, Confidence::High)
    } else {
        (
            library::FileQuality {
                resolution: release.resolution.clone(),
                codec: release.codec.clone(),
                hdr: release.hdr.clone(),
            },
            QualitySource::Release,
            Confidence::Low,
        )
    }
}
