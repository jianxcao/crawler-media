use super::connection::{next_attempt, save, snapshot};
use super::eligibility::{Eligibility, assess};
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn retire_chapter_frame_candidates(
        &self,
        claim: &ChapterFrameClaim,
    ) -> Result<Vec<(usize, ChapterFrameArtifact)>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let mut current = snapshot(&tx, &claim.task.id)?;
        let mut retired = Vec::new();
        for owned in &claim.targets {
            let Some(artifact) = &owned.candidate else {
                continue;
            };
            if artifact.attempt != claim.attempt {
                continue;
            }
            if let Some(current) = &mut current {
                if current.status.generation != claim.generation {
                    continue;
                }
                let Some(target) = current.targets.get_mut(owned.index) else {
                    continue;
                };
                if target.published.as_ref() == Some(artifact) {
                    continue;
                }
                if target.candidate.as_ref() == Some(artifact) {
                    if !current.status.state.fenced() {
                        continue;
                    }
                    target.candidate = None;
                }
            }
            retired.push((owned.index, artifact.clone()));
        }
        if let Some(current) = current {
            save(&tx, &current)?;
        }
        tx.commit()?;
        Ok(retired)
    }

    pub fn repair_missing_chapter_frame_targets(
        &self,
        id: &str,
        generation: &str,
        attempt: u64,
        missing: &[(usize, ChapterFrameArtifact)],
    ) -> Result<ChapterFrameReconcile, StoreError> {
        self.reconcile_chapter_frame_artifacts(id, generation, attempt, missing, false)
    }

    pub fn adopt_chapter_frame_candidates(
        &self,
        id: &str,
        generation: &str,
        attempt: u64,
        candidates: &[(usize, ChapterFrameArtifact)],
    ) -> Result<ChapterFrameReconcile, StoreError> {
        self.reconcile_chapter_frame_artifacts(id, generation, attempt, candidates, true)
    }

    fn reconcile_chapter_frame_artifacts(
        &self,
        id: &str,
        generation: &str,
        attempt: u64,
        observed: &[(usize, ChapterFrameArtifact)],
        adopt: bool,
    ) -> Result<ChapterFrameReconcile, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let mut current = snapshot(&tx, id)?.ok_or_else(|| StoreError::Missing(id.into()))?;
        let mut seen = std::collections::HashSet::new();
        let matching = !observed.is_empty()
            && !current.status.state.fenced()
            && current.status.generation == generation
            && current.status.attempt == attempt
            && (!adopt || current.status.state == ChapterFrameState::Queued)
            && observed.iter().all(|(index, artifact)| {
                seen.insert(*index)
                    && current.targets.get(*index).is_some_and(|target| {
                        if adopt {
                            !target.ready && target.candidate.as_ref() == Some(artifact)
                        } else {
                            target.ready && target.published.as_ref() == Some(artifact)
                        }
                    })
            });
        let applied = matching
            && match assess(&tx, &mut current)? {
                Eligibility::Ready => true,
                // Missing-output repair only records pending work; it does not
                // publish Media facts and must remain resumable after release.
                Eligibility::Deferred => !adopt,
                Eligibility::Fenced(_) => false,
            };
        if applied {
            for (index, artifact) in observed {
                let target = &mut current.targets[*index];
                target.ready = adopt;
                target.error = None;
                target.candidate = None;
                target.published = adopt.then(|| artifact.clone());
            }
            if adopt {
                if current.targets.iter().all(|target| target.ready) {
                    current.status.state = ChapterFrameState::Completed;
                }
            } else {
                current.status.state = ChapterFrameState::Queued;
                current.status.attempt = next_attempt(current.status.attempt)?;
                for target in current.targets.iter_mut().filter(|target| !target.ready) {
                    target.candidate = None;
                }
            }
            save(&tx, &current)?;
        }
        current.refresh_status();
        tx.commit()?;
        Ok(ChapterFrameReconcile {
            applied,
            status: current.status,
        })
    }
}
