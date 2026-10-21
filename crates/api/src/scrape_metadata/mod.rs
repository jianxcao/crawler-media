//! Shared TMDB → NFO mapping for every metadata scrape entry point.

pub(crate) mod export;
pub(crate) mod series;
pub(crate) mod targets;
pub(crate) mod work;

pub(crate) use series::scrape_tv_sidecars;
pub(crate) use work::{
    adopt_tmdb_metadata, fetch_tmdb_metadata, nfo_from_tmdb, preferred_language,
    record_scraped_display, record_scraped_display_meta, scrape_movie_fanart, show_root, write_nfo,
};
