use crate::library_write::reduce::Counts;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Key {
    pub(super) user: String,
    pub(super) season: i32,
    pub(super) episode: i32,
    pub(super) a: String,
    pub(super) b: String,
    pub(super) c: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct MediaPage {
    pub(super) media: String,
    pub(super) phase: Phase,
    pub(super) files: Files,
    pub(super) coverage_after: Option<(i32, i32, String)>,
    pub(super) unit: Option<Unit>,
    pub(super) seasons: i64,
    pub(super) episodes: i64,
    pub(super) last_season: Option<i32>,
    pub(super) first_available: Option<(i32, i32)>,
    pub(super) user_after: Option<String>,
    pub(super) user: Option<crate::playback::user_summary::IntegrityUser>,
    pub(super) table: usize,
    pub(super) after: Option<Key>,
    pub(super) mismatch: bool,
    pub(super) presence_issues: usize,
}

impl MediaPage {
    pub(super) fn new(media: String) -> Self {
        Self {
            media,
            phase: Phase::Files,
            files: Files::default(),
            coverage_after: None,
            unit: None,
            seasons: 0,
            episodes: 0,
            last_season: None,
            first_available: None,
            user_after: None,
            user: None,
            table: 0,
            after: None,
            mismatch: false,
            presence_issues: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum Phase {
    Files,
    Coverage,
    Header,
    Users,
    Sync,
    Reverse,
    Verify,
    VerifyReverse,
    VerifyHeader,
    Members,
    Cleanup,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Files {
    pub(super) after: Option<String>,
    pub(super) counts: Counts,
    pub(super) preferred: Option<Choice>,
    pub(super) poster: Option<Choice>,
    pub(super) backdrop: Option<Choice>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Choice {
    pub(super) file: String,
    pub(super) present: bool,
    pub(super) revision: i64,
}

impl Choice {
    pub(super) fn wins(&self, old: &Option<Self>) -> bool {
        old.as_ref().is_none_or(|old| {
            (self.present, self.revision, std::cmp::Reverse(&self.file))
                > (old.present, old.revision, std::cmp::Reverse(&old.file))
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Unit {
    pub(super) season: i32,
    pub(super) episode: i32,
    pub(super) files: i64,
    pub(super) available: i64,
    pub(super) preferred: Option<Choice>,
}
