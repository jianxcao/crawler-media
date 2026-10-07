use domain::{LedgerId, LedgerRow, Media, MediaId, Subscribe, UserId};
use std::collections::HashMap;
use std::path::Path;

use crate::Store;

pub(crate) struct SubscriptionViewSnapshot {
    pub subscribe: Subscribe,
    pub media: Media,
    pub facts: subscribe::SubscribeFacts,
    pub pending: Vec<(i32, crate::store::PendingDownload)>,
    pub created_at: String,
    pub updated_at: String,
    pub library_poster_candidate: Option<LedgerId>,
    pub total: i64,
    pub imported: i64,
}

pub(crate) fn load_visible_subscription_views(
    store: &Store,
    user_id: UserId,
) -> Result<Vec<SubscriptionViewSnapshot>, crate::store::StoreError> {
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);

    let subscribes = if admin {
        store.list_all_subscribes()?
    } else {
        store.list_subscribes_for_user(user_id)?
    };

    // 预批量提取所有相关 ledger rows，按 media_id 分组
    let all_ledger = store.list_ledger().unwrap_or_default();
    let mut ledger_by_media: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    for row in all_ledger {
        ledger_by_media.entry(row.media_id).or_default().push(row);
    }

    let mut snapshots = Vec::with_capacity(subscribes.len());

    for sub in subscribes {
        let Some(media) = store.get_media(sub.media_id).ok().flatten() else {
            continue;
        };

        let facts = store.load_subscribe_facts(sub.id).unwrap_or_default();
        let pending = store.load_pending(sub.id).unwrap_or_default();
        let (created_at, updated_at) = store.subscribe_times(sub.id).unwrap_or_default();

        let (total, imported) = super::views::progress(store, &sub, &media, &facts);

        let library_poster_candidate = ledger_by_media
            .get(&media.id)
            .and_then(|rows| select_preferred_library_poster(rows));

        snapshots.push(SubscriptionViewSnapshot {
            subscribe: sub,
            media,
            facts,
            pending,
            created_at,
            updated_at,
            library_poster_candidate,
            total,
            imported,
        });
    }

    Ok(snapshots)
}

pub(crate) fn load_single_subscription_view(
    store: &Store,
    sub: &Subscribe,
    media: &Media,
    facts: &subscribe::SubscribeFacts,
) -> SubscriptionViewSnapshot {
    let pending = store.load_pending(sub.id).unwrap_or_default();
    let (created_at, updated_at) = store.subscribe_times(sub.id).unwrap_or_default();
    let (total, imported) = super::views::progress(store, sub, media, facts);

    let rows: Vec<LedgerRow> = store
        .list_ledger()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.media_id == media.id)
        .collect();

    let library_poster_candidate = select_preferred_library_poster(&rows);

    SubscriptionViewSnapshot {
        subscribe: sub.clone(),
        media: media.clone(),
        facts: facts.clone(),
        pending,
        created_at,
        updated_at,
        library_poster_candidate,
        total,
        imported,
    }
}

fn select_preferred_library_poster(rows: &[LedgerRow]) -> Option<LedgerId> {
    let selected = rows.iter().max_by_key(|row| {
        (
            row.season.unwrap_or(0),
            row.episode.unwrap_or(0),
            row.path.clone(),
        )
    })?;

    let poster = Path::new(&selected.path).parent().map(|d| d.join("poster.jpg"))?;
    if poster.is_file() {
        Some(selected.id)
    } else {
        None
    }
}
