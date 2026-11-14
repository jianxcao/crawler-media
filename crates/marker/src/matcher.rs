//! Candidate extraction APIs return accepted intervals only. Production marker
//! publication uses `verify_season_fingerprints_with` to retain every decision.
mod consensus;
mod decisions;
pub mod session;

pub use decisions::{
    EpisodeFingerprintDecision, FingerprintDecision, verify_season_fingerprints_with,
};
mod intro;
mod outro;

pub use intro::{match_episodes_fingerprints, match_episodes_fingerprints_with};
pub use outro::{match_episodes_outros, match_episodes_outros_with};
