//! Library browse over the `library_items` summary: filter predicates, ordering and
//! keyset pagination, plus the page-scoped batch reads a poster wall needs.

pub(crate) mod filters;
pub(crate) mod hydration;
pub(crate) mod page;
pub(crate) mod query;
pub mod stats;

pub use stats::QueryWorkStats;

pub(crate) use filters::filter_clauses;
pub(crate) use page::item_from_row;
pub(crate) use query::Bindings;

/// Watch tiers as an ascending integer, mirroring `library::WatchTier` order.
pub(crate) const WATCH_TIER_SQL: &str = "CASE WHEN COALESCE(u.seen, 0) = 0 THEN 0 \
     WHEN COALESCE(u.played, 0) = 1 THEN 2 ELSE 1 END";

/// `library_items` columns every card query selects, in [`item_from_row`] order.
pub(crate) const ITEM_COLUMNS: &str = "i.library_id, i.media_id, i.kind, i.title, i.title_sort, \
     i.year, i.file_count, i.available_file_count, i.missing_file_count, i.unknown_file_count, \
     i.distinct_episode_count, i.distinct_season_count, i.size_bytes, i.preferred_file_id, \
     i.has_still, i.rating_sort, i.language, i.identity_state, i.missing, i.scraped, \
     i.poster_ref, i.backdrop_ref, i.resolution_rank";

#[cfg(test)]
mod tests;
