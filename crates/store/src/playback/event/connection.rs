//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn commit_playback_event(
        &self,
        event: &BoundPlaybackEvent,
        now_ms: i64,
    ) -> Result<EventOutcome, StoreError> {
        let (media_id, unit) = playback_unit(&event.item);
        self.ensure_owns_transaction()?;
        // `&self` only: `Store::library` is not reachable as `&mut` here, so ask rusqlite for
        // the IMMEDIATE lock directly instead of `Connection::transaction_with_behavior`.
        let tx = Transaction::new_unchecked(self.library, TransactionBehavior::Immediate)?;
        crate::library_operations::integrity_guard::ensure_media_integrity_writable(&tx, media_id)?;
        let Some(binding) = read_binding(&tx, event)? else {
            return Ok(EventOutcome::IgnoredStale);
        };
        if stale_bound_event(self, &tx, event, &binding, media_id, unit)? {
            return Ok(EventOutcome::IgnoredStale);
        }
        match crate::library_write::publish::ensure_media_identity_writable(&tx, media_id) {
            Ok(()) => {}
            Err(StoreError::Protected(_) | StoreError::Missing(_)) => {
                tracing::debug!(%media_id, session_id = %event.session_id,
                    "playback event for a frozen or retired identity ignored");
                return Ok(EventOutcome::IgnoredStale);
            }
            Err(error) => return Err(error),
        }
        // An admin ended this device's playback: a later event must not resurrect the session
        // nor write unit facts, it archives the live row instead. `Applied` (not
        // `IgnoredStale`) because this event *is* handled — it ends the session, which is a
        // write; `IgnoredStale` stays reserved for events that write nothing.
        if admin_ended_session(self, event, now_ms)?
            && event.event_type != PlaybackEventType::Stopped
        {
            archive_admin_ended_session(&tx, event, &binding, now_ms)?;
            tx.commit()?;
            tracing::debug!(
                session_id = %event.session_id,
                "playback event for an admin-ended session closed without unit writes"
            );
            return Ok(EventOutcome::Applied);
        }
        // Starting or progressing activates exactly this session; a session already closed by
        // a newer generation on the same device is refused instead of being reused.
        if event.event_type != PlaybackEventType::Stopped
            && !self.activate_play_session(event.user_id, &event.device_id, &event.session_id)?
        {
            return Ok(EventOutcome::IgnoredStale);
        }

        let previous = super::super::event_duration::read_event_resume(&tx, event, media_id, unit)?;
        let duration =
            super::super::event_duration::effective_event_duration(event, previous.as_ref());
        write_unit_facts(self, &tx, event, media_id, unit, duration, now_ms)?;
        if event.event_type == PlaybackEventType::Stopped {
            bump_play_count_for_completed_stop(&tx, event, media_id, unit, duration)?;
        }
        advance_session_state(self, &tx, event, &binding, now_ms)?;

        self.refresh_user_library_summary(
            event.user_id,
            media_id,
            SummaryScope::for_unit(unit.season, unit.episode),
        )?;
        tx.commit()?;
        tracing::debug!(
            session_id = %event.session_id,
            event = ?event.event_type,
            position_ms = event.position_ms,
            "playback event applied"
        );
        Ok(EventOutcome::Applied)
    }

    pub(super) fn ensure_owns_transaction(&self) -> Result<(), StoreError> {
        if !self.library.is_autocommit() {
            return Err(StoreError::Invalid(
                "commit_playback_event must own its IMMEDIATE transaction".into(),
            ));
        }
        Ok(())
    }
}
