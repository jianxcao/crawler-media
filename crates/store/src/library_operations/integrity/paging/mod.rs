//! Reader-prepared, owner-fenced pages inside one Media.
mod commit;
mod facts;
mod prepare;
mod rows;
mod state;
mod sync;

use super::*;
use crate::query_projection::{ExpectedProjection, expected::OwnedRow};
pub(crate) use state::MediaPage;
use state::{Key, Phase};

enum Mutation {
    Stage(&'static str, OwnedRow),
    Upsert(&'static str, OwnedRow),
    Delete(&'static str, &'static str, Key),
    MissingItem,
    OrphanItem,
    Resolution(String),
    MissingUser(String),
    OrphanUser(String),
    Header(ExpectedProjection),
    Cleanup(String, Key),
}

pub(crate) struct Prepared {
    pub library: String,
    pub before: Option<String>,
    pub state: IntegrityCursor,
    pub plan: IntegrityPlan,
    actions: Vec<Mutation>,
    pub finished_media: bool,
    pub finish: bool,
}

pub(crate) use commit::{commit, record_failure};
pub(crate) use prepare::prepare;
