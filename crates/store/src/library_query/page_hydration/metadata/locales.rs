//! Select locales from indexed keys before reading or parsing any metadata payload.
use super::super::Owner;
use super::{InputKey, scope};
use crate::{
    StoreError,
    metadata::locale::{LocaleFallback, select_locale},
};
use domain::{EntityScope, LedgerId, LibraryId, MediaId};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

pub(super) const SQL: &str =
    "SELECT 'input',i.library_id,i.media_id,i.entity_scope,i.season,i.episode,i.locale
 FROM json_each(?1,'$.scopes') j CROSS JOIN library_metadata_inputs i
 ON i.library_id=json_extract(j.value,'$[0]') AND i.media_id=json_extract(j.value,'$[1]')
 AND i.entity_scope=json_extract(j.value,'$[2]') AND i.season=json_extract(j.value,'$[3]')
 AND i.episode=json_extract(j.value,'$[4]') WHERE i.parse_state='valid'
 UNION ALL SELECT 'adopted','',m.media_id,'work',-1,-1,m.locale
 FROM json_each(?1,'$.media') j CROSS JOIN media_metadata m ON m.media_id=j.value";

pub(super) struct Requests {
    pub inputs: String,
    pub adopted: String,
    pub entities: String,
    pub locales: HashMap<(LibraryId, MediaId), String>,
}

pub(super) fn select(
    conn: &Connection,
    owners: &[Owner],
    scopes: &HashMap<LedgerId, HashSet<EntityScope>>,
) -> Result<Requests, StoreError> {
    let mut keys = HashSet::new();
    let media: HashSet<_> = owners.iter().map(|o| o.media).collect();
    for owner in owners {
        if let Some(scopes) = scopes.get(&owner.file) {
            for scope in scopes {
                let (season, episode) = scope.season_episode();
                keys.insert((owner.library, owner.media, scope.as_str(), season, episode));
            }
        }
    }
    let encoded = serde_json::json!({"scopes":keys,"media":media}).to_string();
    let mut input_locales: HashMap<InputKey, Vec<String>> = HashMap::new();
    let mut adopted_locales: HashMap<MediaId, Vec<String>> = HashMap::new();
    let mut stmt = conn.prepare(SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(r) = rows.next()? {
        let media = r.get::<_, String>(2)?.parse()?;
        let locale = r.get(6)?;
        if r.get::<_, String>(0)? == "adopted" {
            adopted_locales.entry(media).or_default().push(locale);
        } else {
            let library = r.get::<_, String>(1)?.parse()?;
            let scope = scope(r.get(3)?, r.get(4)?, r.get(5)?)?;
            input_locales
                .entry((library, media, scope))
                .or_default()
                .push(locale);
        }
    }
    let default = crate::metadata_language::default_locale(conn)?.unwrap_or_default();
    Ok(requests(
        owners,
        scopes,
        input_locales,
        adopted_locales,
        &default,
    ))
}
fn requests(
    owners: &[Owner],
    scopes: &HashMap<LedgerId, HashSet<EntityScope>>,
    input_locales: HashMap<InputKey, Vec<String>>,
    adopted_locales: HashMap<MediaId, Vec<String>>,
    default: &str,
) -> Requests {
    let mut inputs = HashSet::new();
    let mut adopted = HashSet::new();
    let mut entities = HashSet::new();
    let mut locales = HashMap::new();
    for owner in owners {
        let chain = vec![owner.locale.clone(), default.to_string()];
        let chosen = adopted_locales
            .get(&owner.media)
            .and_then(|available| select_locale(available, &chain, LocaleFallback::AdoptedFirst));
        if let Some(locale) = &chosen {
            adopted.insert((owner.media, locale.clone()));
            locales.insert((owner.library, owner.media), locale.clone());
        }
        if let Some(scopes) = scopes.get(&owner.file) {
            for scope in scopes {
                let (season, episode) = scope.season_episode();
                if let Some(available) = input_locales.get(&(owner.library, owner.media, *scope))
                    && let Some(locale) =
                        select_locale(available, &chain, LocaleFallback::UnboundInputOnly)
                {
                    inputs.insert((
                        owner.library,
                        owner.media,
                        scope.as_str(),
                        season,
                        episode,
                        locale,
                    ));
                }
                if let Some(locale) = &chosen {
                    entities.insert((owner.media, scope.as_str(), season, episode, locale.clone()));
                }
            }
        }
    }
    Requests {
        inputs: serde_json::json!(inputs).to_string(),
        adopted: serde_json::json!(adopted).to_string(),
        entities: serde_json::json!(entities).to_string(),
        locales,
    }
}
