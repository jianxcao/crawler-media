//! Batch loading and pure composition preserve the detail reader's locale/scope rules.
mod locales;
use super::{HydratedFile, Owner};
use crate::StoreError;
use crate::library_query::details::metadata::fill_from_adopted_parts;
use crate::library_query::protocol::metadata::{overlay_adopted_details, overlay_missing};
use crate::metadata::compose::{
    SourceSnapshot, compose_loaded, parse_selection_origins, source_rank,
};
use domain::{EntityScope, LedgerId, LibraryId, MediaId, MetadataFields};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

const INPUTS_SQL:&str="SELECT i.library_id,i.media_id,i.entity_scope,i.season,i.episode,i.fields_json
 FROM json_each(?1) j CROSS JOIN library_metadata_inputs i
 ON i.library_id=json_extract(j.value,'$[0]') AND i.media_id=json_extract(j.value,'$[1]')
 AND i.entity_scope=json_extract(j.value,'$[2]') AND i.season=json_extract(j.value,'$[3]')
 AND i.episode=json_extract(j.value,'$[4]') AND i.locale=json_extract(j.value,'$[5]')
 AND i.input_key=(SELECT chosen.input_key FROM library_metadata_inputs chosen
 WHERE chosen.library_id=i.library_id AND chosen.media_id=i.media_id AND chosen.entity_scope=i.entity_scope
 AND chosen.season=i.season AND chosen.episode=i.episode AND chosen.locale=i.locale
 AND chosen.parse_state='valid' ORDER BY chosen.input_key LIMIT 1)";
const ADOPTED_SQL: &str = "SELECT m.media_id,m.locale,m.title,m.details_json,m.field_origins_json
 FROM json_each(?1) j CROSS JOIN media_metadata m ON m.media_id=json_extract(j.value,'$[0]')
 AND m.locale=json_extract(j.value,'$[1]')";
// Source precedes locale in the primary key; constrain both for indexed locale seeks.
const SOURCES_SQL: &str = "SELECT m.media_id,m.entity_scope,m.season,m.episode,m.locale,m.source,
 m.provider_entity_id,m.source_revision,m.normalized_json,m.selection_origins_json
 FROM json_each(?1) j CROSS JOIN media_metadata_sources m ON m.media_id=json_extract(j.value,'$[0]')
 AND m.entity_scope=json_extract(j.value,'$[1]') AND m.season=json_extract(j.value,'$[2]')
 AND m.episode=json_extract(j.value,'$[3]') AND m.locale=json_extract(j.value,'$[4]')
 AND m.source IN ('tmdb','douban','tvdb','bangumi','anilist','fanart')";
const OVERRIDES_SQL:&str="SELECT m.media_id,m.entity_scope,m.season,m.episode,m.locale,m.fields_json,m.revision
 FROM json_each(?1) j CROSS JOIN media_metadata_overrides m ON m.media_id=json_extract(j.value,'$[0]')
 AND m.entity_scope=json_extract(j.value,'$[1]') AND m.season=json_extract(j.value,'$[2]')
 AND m.episode=json_extract(j.value,'$[3]') AND m.locale=json_extract(j.value,'$[4]')";
#[cfg(any(test, feature = "test-support"))]
pub(super) const PLANS: &[&str] = &[
    locales::SQL,
    INPUTS_SQL,
    ADOPTED_SQL,
    SOURCES_SQL,
    OVERRIDES_SQL,
];
type EntityLocale = (MediaId, EntityScope, String);
type InputKey = (LibraryId, MediaId, EntityScope);
struct Adopted {
    title: String,
    details: String,
    origins: String,
}
#[derive(Default)]
struct Facts {
    inputs: HashMap<InputKey, MetadataFields>,
    adopted: HashMap<(MediaId, String), Adopted>,
    sources: HashMap<EntityLocale, Vec<SourceSnapshot>>,
    overrides: HashMap<EntityLocale, (MetadataFields, i64)>,
}

pub(super) fn hydrate(
    conn: &Connection,
    owners: &[Owner],
    scopes: &HashMap<LedgerId, HashSet<EntityScope>>,
    output: &mut HashMap<LedgerId, HydratedFile>,
) -> Result<(), StoreError> {
    let requests = locales::select(conn, owners, scopes)?;
    let mut facts = Facts::default();
    load_inputs(conn, &requests.inputs, &mut facts)?;
    load_adopted(conn, &requests.adopted, &mut facts)?;
    load_sources(conn, &requests.entities, &mut facts)?;
    load_overrides(conn, &requests.entities, &mut facts)?;
    for owner in owners {
        let Some(hydrated) = output.get_mut(&owner.file) else {
            continue;
        };
        let locale = requests
            .locales
            .get(&(owner.library, owner.media))
            .map(String::as_str);
        let work = fields(&facts, owner, EntityScope::Work, locale);
        if let Some(scopes) = scopes.get(&owner.file) {
            for scope in scopes {
                let mut value = fields(&facts, owner, *scope, locale);
                if *scope != EntityScope::Work {
                    overlay_missing(&mut value, &work);
                }
                hydrated.fields.insert(*scope, value);
            }
        }
    }
    Ok(())
}
fn fields(
    facts: &Facts,
    owner: &Owner,
    scope: EntityScope,
    locale: Option<&str>,
) -> MetadataFields {
    let mut fields = facts
        .inputs
        .get(&(owner.library, owner.media, scope))
        .cloned()
        .unwrap_or_default();
    let Some(locale) = locale else { return fields };
    let Some(adopted) = facts.adopted.get(&(owner.media, locale.to_string())) else {
        return fields;
    };
    if scope == EntityScope::Work {
        overlay_adopted_details(&mut fields, &adopted.details, owner.media);
        fill_from_adopted_parts(
            &mut fields,
            &adopted.title,
            &adopted.details,
            &adopted.origins,
        );
    }
    let key = (owner.media, scope, locale.to_string());
    let sources = facts
        .sources
        .get(&key)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let composed = compose_loaded(owner.media, sources, facts.overrides.get(&key).cloned()).0;
    overlay_missing(&mut fields, &composed);
    fields
}
fn scope(kind: String, season: i32, episode: i32) -> Result<EntityScope, StoreError> {
    match kind.as_str() {
        "work" => Ok(EntityScope::Work),
        "season" if season >= 0 => Ok(EntityScope::Season(season as u32)),
        "episode" if season >= 0 && episode >= 0 => Ok(EntityScope::Episode {
            season: season as u32,
            episode: episode as u32,
        }),
        _ => Err(StoreError::Invalid(format!(
            "invalid stored metadata scope {kind}/{season}/{episode}"
        ))),
    }
}
fn load_inputs(conn: &Connection, encoded: &str, facts: &mut Facts) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(INPUTS_SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(r) = rows.next()? {
        let library = r.get::<_, String>(0)?.parse()?;
        let media = r.get::<_, String>(1)?.parse()?;
        let scope = scope(r.get(2)?, r.get(3)?, r.get(4)?)?;
        facts.inputs.insert(
            (library, media, scope),
            serde_json::from_str(&r.get::<_, String>(5)?)?,
        );
    }
    Ok(())
}
fn load_adopted(conn: &Connection, encoded: &str, facts: &mut Facts) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(ADOPTED_SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(r) = rows.next()? {
        let media = r.get::<_, String>(0)?.parse()?;
        facts.adopted.insert(
            (media, r.get(1)?),
            Adopted {
                title: r.get(2)?,
                details: r.get(3)?,
                origins: r.get(4)?,
            },
        );
    }
    Ok(())
}
fn load_sources(conn: &Connection, encoded: &str, facts: &mut Facts) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(SOURCES_SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(r) = rows.next()? {
        let media = r.get::<_, String>(0)?.parse()?;
        let scope = scope(r.get(1)?, r.get(2)?, r.get(3)?)?;
        let (origins, pending) = parse_selection_origins(&r.get::<_, String>(9)?);
        facts
            .sources
            .entry((media, scope, r.get(4)?))
            .or_default()
            .push(SourceSnapshot {
                source: r.get(5)?,
                provider_entity_id: r.get(6)?,
                revision: r.get(7)?,
                fields: serde_json::from_str(&r.get::<_, String>(8)?)?,
                origins,
                pending,
            });
    }
    for snapshots in facts.sources.values_mut() {
        snapshots.sort_by(|a, b| {
            (source_rank(&a.source), a.source.as_str())
                .cmp(&(source_rank(&b.source), b.source.as_str()))
        });
    }
    Ok(())
}
fn load_overrides(conn: &Connection, encoded: &str, facts: &mut Facts) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(OVERRIDES_SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(r) = rows.next()? {
        let media = r.get::<_, String>(0)?.parse()?;
        let scope = scope(r.get(1)?, r.get(2)?, r.get(3)?)?;
        facts.overrides.insert(
            (media, scope, r.get(4)?),
            (serde_json::from_str(&r.get::<_, String>(5)?)?, r.get(6)?),
        );
    }
    Ok(())
}
