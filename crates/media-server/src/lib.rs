//! Media Server protocol layer (Jellyfin & Emby compatibility).
//!
//! Provides standard REST API endpoints for Jellyfin and Emby media players
//! (such as VidHub, Infuse, Emby/Jellyfin official clients).
//!
//! Architectural principles:
//! 1. Protocol isolation: encapsulates Jellyfin/Emby DTO dialect (BaseItemDto, UserViews, Ticks)
//!    without contaminating the core domain models.
//! 2. Multi-tenant security: every request strictly verifies user-level library visibility,
//!    ensuring users only see content authorized for them and manage their own playstate.

pub mod auth;
pub mod dto;
pub mod provider;
pub mod routes;

pub use auth::AuthUser;
pub use provider::MediaServerProvider;
pub use routes::{media_routes, routes};
