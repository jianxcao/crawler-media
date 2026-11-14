use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{FavoriteOrder, PlaybackViewQuery};
use crate::StoreError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Primary {
    Time(i64),
    Title(String),
    Year(Option<i64>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    version: u8,
    scope: String,
    pub tier: i64,
    pub primary: Primary,
    pub media: String,
}
pub(super) fn invalid() -> StoreError {
    StoreError::Invalid("malformed favorite cursor".into())
}
fn scope(q: &PlaybackViewQuery) -> Result<String, StoreError> {
    let mut libraries: Vec<_> = q.libraries.iter().map(ToString::to_string).collect();
    libraries.sort_unstable();
    libraries.dedup();
    let order = match q.order {
        FavoriteOrder::FavoritedAt => "time",
        FavoriteOrder::Title => "title",
        FavoriteOrder::Year => "year",
    };
    let bytes = serde_json::to_vec(&(q.user, libraries, order, q.descending, q.unwatched_first))
        .map_err(|_| invalid())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
impl Cursor {
    pub(super) fn new(
        q: &PlaybackViewQuery,
        tier: i64,
        primary: Primary,
        media: String,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            version: 1,
            scope: scope(q)?,
            tier,
            primary,
            media,
        })
    }
    pub(super) fn encode(&self) -> Result<String, StoreError> {
        serde_json::to_string(self).map_err(|_| invalid())
    }
    pub(super) fn decode(q: &PlaybackViewQuery, raw: &str) -> Result<Self, StoreError> {
        let value: Self = serde_json::from_str(raw).map_err(|_| invalid())?;
        if value.version != 1
            || value.scope != scope(q)?
            || value.media.parse::<domain::MediaId>().is_err()
            || !(0..=2).contains(&value.tier)
            || !matches!(
                (&value.primary, q.order),
                (Primary::Time(_), FavoriteOrder::FavoritedAt)
                    | (Primary::Title(_), FavoriteOrder::Title)
                    | (Primary::Year(_), FavoriteOrder::Year)
            )
        {
            return Err(invalid());
        }
        Ok(value)
    }
}
