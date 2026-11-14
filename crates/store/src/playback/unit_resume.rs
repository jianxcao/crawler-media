//! Logical marks retain an existing resume package; progress names a selected source.
use domain::{MediaId, PhysicalSourceId, UserId};
use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

pub(super) struct UnitIdentity {
    pub user: UserId,
    pub media: MediaId,
    pub season: i32,
    pub episode: i32,
}

#[derive(Default)]
pub(super) struct ResumePackage {
    pub file_id: Option<String>,
    pub revision: Option<i64>,
    pub source_version: Option<String>,
    pub timeline: Option<String>,
    pub tracks_version: Option<i64>,
    pub position: i64,
    pub duration: Option<i64>,
    pub audio: Option<String>,
    pub subtitle: Option<String>,
}

pub(super) struct ResumeProgress {
    pub position: i64,
    pub duration: Option<i64>,
    pub audio: Option<String>,
    pub subtitle: Option<String>,
}

impl Store {}

#[path = "unit_resume/connection.rs"]
mod connection;
