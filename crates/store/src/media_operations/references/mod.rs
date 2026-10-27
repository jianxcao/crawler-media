//! Fact merges for a canonical Media merge (§4.6): adopted metadata, user playback
//! facts and collection membership. Every function runs inside the caller's
//! `library.db` transaction and is deterministic from the surviving rows, so
//! re-running a phase after a crash cannot duplicate or double-count.
//!
//! Split by the merge's fact families — each sibling owns one family:
//! `metadata` (adopted rows + shared overrides), `snapshots` (source-snapshot
//! composition), `playback` (user playback), `scoped_facts` (media-scoped facts and
//! collection membership). This module only declares them and re-exports the four
//! entry points `apply` drives.

mod metadata;
mod playback;
mod scoped_facts;
mod snapshots;

pub(super) use metadata::merge_metadata;
pub(super) use playback::merge_playback_units;
pub(super) use scoped_facts::{merge_collection_items, merge_watch_facts};
