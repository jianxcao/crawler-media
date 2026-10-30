//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn begin_rename_disk_record(
        &self,
        receipt: &RenameDiskReceipt,
    ) -> Result<Option<RenameSidecarPlan>, StoreError> {
        let rename_id = receipt.rename_id.as_str();
        let request = self.load_file_rename(rename_id)?;
        match request.phase.as_str() {
            "planned" => Ok(Some(RenameSidecarPlan {
                request,
                receipt: receipt.clone(),
            })),
            "disk_applied" | "library_published" | "references_applied" => Ok(None),
            phase => Err(StoreError::Protected(format!(
                "rename {rename_id} is {phase}; disk receipt refused"
            ))),
        }
    }

    pub fn decide_rename_disk(
        &self,
        observed: ObservedRenameSidecars,
    ) -> Result<DecidedRenameSidecars, StoreError> {
        let ObservedRenameSidecars { plan, observed } = observed;
        let RenameSidecarPlan { request, receipt } = plan;
        let mut sidecars = request.sidecars.clone();
        let destination = &receipt.destination;
        let tx = crate::write_tx(self.library)?;
        decide_for_execution(
            &tx,
            &request.file_id.to_string(),
            &request.from_path,
            &mut sidecars,
            &observed,
        )?;
        let updated = tx.execute(
            "UPDATE pending_file_renames
             SET sidecars_json = ?2, dest_size = ?3, dest_device = ?4, dest_inode = ?5,
                 dest_mtime_ns = ?6
             WHERE rename_id = ?1 AND phase = 'planned'",
            params![
                request.rename_id,
                sidecars_json(&sidecars)?,
                destination.size_bytes as i64,
                destination.device as i64,
                destination.inode as i64,
                destination.mtime_ns
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::Conflict(format!(
                "rename {} left `planned` while its sidecars were observed",
                request.rename_id
            )));
        }
        tx.commit()?;
        Ok(DecidedRenameSidecars { request, sidecars })
    }

    pub fn finish_rename_disk(
        &self,
        applied: AppliedRenameSidecars,
        now: i64,
    ) -> Result<(), StoreError> {
        self.mark_rename_disk_applied(&applied.request.rename_id, &applied.sidecars, now)
    }
}
