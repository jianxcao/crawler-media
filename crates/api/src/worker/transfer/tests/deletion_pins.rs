use super::*;

pub(super) fn publication_observation(
    outcome: &subscribe::RunOutcome,
    mode: &str,
) -> TransferFileObservation {
    TransferFileObservation {
        mode: mode.into(),
        source: None,
        artwork: store::files::DiscoveredArtwork::discover(&outcome.files[0].file.path),
    }
}
