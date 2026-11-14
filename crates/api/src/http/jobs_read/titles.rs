//! Resolve only the selected Jobs' titles through two bounded SQL batches.
use jobs::JobScope as Source;
use std::collections::HashSet;

fn source(def: &jobs::JobDef) -> Source {
    def.scope
}

pub(super) fn selected_titles(
    store: &crate::Store,
    selected: &[(jobs::JobDef, Option<jobs::Job>, Option<jobs::Job>)],
) -> Result<Vec<Option<String>>, store::StoreError> {
    let sources = selected
        .iter()
        .map(|(def, _, _)| source(def))
        .collect::<Vec<_>>();
    let subscriptions = sources
        .iter()
        .filter_map(|source| match source {
            Source::Subscribe(id) => Some(*id),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let subscribed_media = store.subscribe_media_batch(&subscriptions)?;
    let ids = sources
        .iter()
        .filter_map(|source| match source {
            Source::Subscribe(id) => subscribed_media.get(id).copied(),
            Source::Media(id) => Some(*id),
            Source::Global => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let media = store.media_batch(&ids)?;
    Ok(sources
        .iter()
        .map(|source| {
            let id = match source {
                Source::Subscribe(id) => subscribed_media.get(id)?,
                Source::Media(id) => id,
                Source::Global => return None,
            };
            media.get(id).map(|row| row.title.clone())
        })
        .collect())
}
