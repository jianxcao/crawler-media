//! Exact browse membership and proven marginal count shortcuts.
mod facets;
mod plan;
mod user;

pub(crate) use facets::read_facets;
pub(crate) use plan::{CountStrategy, count_browse, plan_browse};
pub(crate) use user::user_join;
