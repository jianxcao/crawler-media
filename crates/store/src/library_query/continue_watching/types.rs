//! Continuation selection returns logical references before page hydration.
use domain::{LibraryId, MediaId, ProtocolItemRef, ProtocolParent, UserId};

#[derive(Clone, Debug)]
pub struct ContinueQuery {
    pub user_id: UserId,
    pub libraries: Vec<LibraryId>,
    pub parent: ProtocolParent,
    pub media_id: Option<MediaId>,
    pub season: Option<i32>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct ContinueEntry {
    pub reference: ProtocolItemRef,
    pub last_played_at: i64,
}

#[derive(Clone, Debug, Default)]
pub struct ContinuePage {
    pub items: Vec<ContinueEntry>,
    pub total: usize,
}

#[derive(Clone, Debug)]
pub struct HomeQuery {
    pub user_id: UserId,
    pub libraries: Vec<LibraryId>,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct HomeEntry {
    pub reference: ProtocolItemRef,
    pub last_activity: i64,
    pub advanced: bool,
}

#[derive(Clone, Debug)]
pub struct HomeWindow {
    pub media_id: MediaId,
    pub season: i32,
    pub episode_from: i32,
    pub episode_to: Option<i32>,
}
