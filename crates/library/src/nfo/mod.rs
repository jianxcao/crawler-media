//! Emby/TMM-style NFO sidecars. Parse common Kodi/Emby/Jellyfin XML metadata
//! while preserving unrelated nodes when updating metadata or stream details.
//!
//! Sidecar 候选与作用域判定见 [`candidates`]：同一目录下可能存在多份 NFO，
//! 每份只对"自己那一层"（Work / Season / Episode）负责。

mod candidates;
mod parse;
mod write;

pub mod export;
pub mod fields;

pub use candidates::{NfoLayer, NfoSidecar, nfo_sidecars, resolve_nfo_scope};
pub use export::{
    NFO_DIGEST_PREFIX, nfo_body_digest, nfo_digest, render_nfo, replace_nfo_atomically,
};
pub use fields::{NfoInput, NfoReadOutcome, parse_nfo_fields};
pub use parse::{CastMember, NfoHeader, NfoMeta, parse_nfo, parse_nfo_header, read_nfo};
pub use write::{write_nfo, write_streamdetails_into_nfo};

pub(crate) use write::xml_escape;
