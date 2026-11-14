//! Subscription depth endpoints (self API): timestamps, real progress, removal-preview
//! torrents, title-preview and upgrade-run reports, split by scenario. The shared fixtures
//! live in [`support`]; its helpers stay re-exported because the neighbouring
//! `subscription_depth_*` and `subscription_policy` modules build their fixtures on them.

mod support;
mod timestamps;
mod title_preview;
mod upgrade;

pub(super) use support::{authed_app, fake_state, seed_pending_torrent};
