//! Resolve only the selected Jobs' titles through two bounded SQL batches.
use domain::{MediaId, SubscribeId};
use serde_json::Value;
use std::collections::HashSet;
use std::str::FromStr;

#[derive(Clone, Copy)]
enum Source {
    Subscribe(SubscribeId),
    Media(MediaId),
    None,
}

fn source(def: &jobs::JobDef) -> Source {
    let Ok(payload) = serde_json::from_str::<Value>(&def.payload) else {
        return Source::None;
    };
    if let Some(raw) = payload.get("subscribe_id").and_then(Value::as_str) {
        return SubscribeId::from_str(raw)
            .map(Source::Subscribe)
            .unwrap_or(Source::None);
    }
    payload
        .get("media_id")
        .and_then(Value::as_str)
        .and_then(|raw| MediaId::from_str(raw).ok())
        .map(Source::Media)
        .unwrap_or(Source::None)
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
            Source::None => None,
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
                Source::None => return None,
            };
            media.get(id).map(|row| row.title.clone())
        })
        .collect())
}
