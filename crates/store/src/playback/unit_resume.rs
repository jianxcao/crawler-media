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

impl Store {
    pub(super) fn played_assertion(
        &self,
        id: &UnitIdentity,
        played: Option<bool>,
        now: i64,
    ) -> Result<(i64, Option<i64>), StoreError> {
        if played.is_some() {
            return Ok((1, Some(now)));
        }
        Ok(self
            .library
            .query_row(
                "SELECT has_played_assertion,played_changed_at_ms FROM playback_units
             WHERE user_id=?1 AND media_id=?2 AND season=?3 AND episode=?4",
                params![
                    id.user.to_string(),
                    id.media.to_string(),
                    id.season,
                    id.episode
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((0, None)))
    }

    pub(super) fn resume_for_unit(
        &self,
        id: &UnitIdentity,
        progress: Option<ResumeProgress>,
        source: Option<PhysicalSourceId>,
    ) -> Result<ResumePackage, StoreError> {
        match progress {
            None => self.retained_resume(id),
            Some(progress) => self.selected_resume(id, progress, source),
        }
    }

    fn retained_resume(&self, id: &UnitIdentity) -> Result<ResumePackage, StoreError> {
        let stored = self.library.query_row(
            "SELECT resume_file_id,resume_file_revision,resume_source_version,timeline_mode,tracks_version,
             position_ms,duration_ms,audio_track,subtitle_track FROM playback_units
             WHERE user_id=?1 AND media_id=?2 AND season=?3 AND episode=?4",
            params![id.user.to_string(),id.media.to_string(),id.season,id.episode],
            |r|Ok(ResumePackage { file_id:r.get(0)?,revision:r.get(1)?,source_version:r.get(2)?,
                timeline:r.get(3)?,tracks_version:r.get(4)?,position:r.get(5)?,duration:r.get(6)?,
                audio:r.get(7)?,subtitle:r.get(8)? }),
        ).optional()?;
        Ok(stored
            .filter(|r| {
                r.file_id.is_some()
                    && r.revision.is_some()
                    && r.source_version.is_some()
                    && r.timeline.is_some()
            })
            .unwrap_or_default())
    }

    fn selected_resume(
        &self,
        id: &UnitIdentity,
        progress: ResumeProgress,
        source: Option<PhysicalSourceId>,
    ) -> Result<ResumePackage, StoreError> {
        let Some(source) = source else {
            return Ok(ResumePackage::default());
        };
        let binding: Option<(String,i64,String,i64)> = self.library.query_row(
            "SELECT f.file_id,f.file_revision,f.source_version,COALESCE(d.tracks_version,f.tracks_version)
             FROM library_files f JOIN library_file_units u ON f.file_id=u.file_id
             LEFT JOIN file_details d ON d.file_id=f.file_id
             WHERE u.media_id=?1 AND u.season=?2 AND u.episode=?3 AND f.presence_state='present'
             AND f.publication_state='published' AND f.file_id=?4 AND f.file_revision=?5",
            params![id.media.to_string(),id.season,id.episode,source.file_id.to_string(),source.revision],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional()?;
        Ok(match binding {
            Some((file, revision, version, tracks)) => ResumePackage {
                file_id: Some(file),
                revision: Some(revision),
                source_version: Some(version),
                timeline: Some("full_file".into()),
                tracks_version: Some(tracks),
                position: progress.position,
                duration: progress.duration,
                audio: progress.audio,
                subtitle: progress.subtitle,
            },
            None => ResumePackage::default(),
        })
    }
}
