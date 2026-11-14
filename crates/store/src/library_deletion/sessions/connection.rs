//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn close_playback_sessions_for_removal_batch(&self, id: &str) -> Result<usize, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT s.user_id,s.device_id FROM library_removal_files f
             JOIN playback_sessions s ON s.file_id=f.file_id
             WHERE f.operation_id=?1 ORDER BY f.file_id,s.user_id,s.device_id LIMIT 100",
        )?;
        let sessions = statement
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (user, device) in &sessions {
            self.close_session(user.parse()?, device, crate::unix_now())?;
            tracing::info!(
                device_id = device,
                operation_id = id,
                "已关闭待删除媒体库相关的活跃播放会话"
            );
        }
        Ok(sessions.len())
    }

    pub fn close_playback_sessions_for_removal(&self, id: &str) -> Result<usize, StoreError> {
        while !self.capture_library_removal_batch(id)? {}
        let mut closed = 0;
        loop {
            let count = self.close_playback_sessions_for_removal_batch(id)?;
            closed += count;
            if count < 100 {
                return Ok(closed);
            }
        }
    }
}
