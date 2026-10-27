mod recover;
mod select;
mod transfer;

use std::path::Path;

use domain::{FileCoverage, FileSource};
use downloader::Downloader;
use library::MediaProbe;

use crate::add::Added;
use crate::choose::is_complete;
use crate::{RunOutcome, SubscribeError};
use select::{collect_scored_torrent, reconcile_mapping_facts};

/// Durable final-path reservation around filesystem publication. Implementations
/// must not perform filesystem work while recording or completing a row.
pub trait TransferRecorder {
    fn prepare(
        &self,
        candidate: &FileCoverage,
        source: &Path,
        mode: library::TransferMode,
    ) -> Result<FileCoverage, SubscribeError>;
    fn published(&self, candidate: &FileCoverage) -> Result<(), SubscribeError>;
}

#[derive(Clone, Copy)]
struct TransferControl<'a> {
    control: &'a library::WorkControl,
    recorder: Option<&'a dyn TransferRecorder>,
}

#[derive(Default)]
struct CollectionOutput {
    files: Vec<FileCoverage>,
    file_sources: Vec<FileSource>,
    removed_paths: Vec<String>,
    transferred_enclosures: Vec<String>,
    rejected_enclosures: Vec<String>,
    awaiting_probe_enclosures: Vec<String>,
    collection_errors: Vec<String>,
}

pub fn collect_completed<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_with_destinations(added, probe, &[])
}

pub fn collect_completed_with_destinations<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_controlled(added, probe, existing, &library::WorkControl::default())
}

pub fn collect_completed_controlled<D: Downloader + ?Sized>(
    added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    control: &library::WorkControl,
) -> Result<RunOutcome, SubscribeError> {
    collect_completed_recorded(added, probe, existing, control, None)
}

pub fn collect_completed_recorded<D: Downloader + ?Sized>(
    mut added: Added<'_, D>,
    probe: &dyn MediaProbe,
    existing: &[crate::collection_destinations::DestinationMapping],
    control: &library::WorkControl,
    recorder: Option<&dyn TransferRecorder>,
) -> Result<RunOutcome, SubscribeError> {
    control.check()?;
    let mut output = CollectionOutput::default();

    // 先依据现有已确认的目标映射刷新/协调 facts，确保即便 Downloader 在 Move 模式下不再列出视频，
    // 本地持久化的映射依然能驱动并修复 facts
    for mapping in existing {
        if !mapping.transfer_pending && mapping.destination_path.is_file() {
            reconcile_mapping_facts(&mut added.input.facts, added.input.subscribe, mapping, 0);
        }
    }

    let chosen = std::mem::take(&mut added.chosen);
    for scored in chosen {
        control.check()?;
        collect_scored_torrent(
            &mut added,
            probe,
            existing,
            &scored,
            &mut output,
            control,
            recorder,
        );
    }
    control.check()?;
    Ok(RunOutcome {
        completed: is_complete(added.input.subscribe, &added.input.facts),
        facts: added.input.facts,
        files: output.files,
        file_sources: output.file_sources,
        removed_paths: output.removed_paths,
        transferred_enclosures: output.transferred_enclosures,
        rejected_enclosures: output.rejected_enclosures,
        awaiting_probe_enclosures: output.awaiting_probe_enclosures,
        collection_errors: output.collection_errors,
        submission_errors: added.add_errors,
        torrents_added: added.outcome.torrents_added,
    })
}
