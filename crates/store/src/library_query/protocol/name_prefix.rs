//! Candidate name filtering using the same pure metadata composition as page hydration.
use domain::{EntityScope, FieldValue, LibraryId, MediaId, MetadataFields};
use rusqlite::{Connection, functions::FunctionFlags};

use crate::StoreError;
use crate::library_query::page_hydration::metadata::{Adopted, Facts, fields};
use crate::library_query::protocol::metadata::overlay_missing;
use crate::metadata::compose::{SourceSnapshot, parse_selection_origins, source_rank};
use crate::metadata::locale::{LocaleFallback, select_locale};

use super::predicates::Branch;

pub(super) fn register(conn: &Connection) -> Result<(), StoreError> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    conn.create_scalar_function("protocol_pick_locale", 4, flags, |ctx| {
        let available: Vec<String> = parse(&ctx.get::<String>(0)?)?;
        let chain = vec![ctx.get::<String>(1)?, ctx.get::<String>(2)?];
        let fallback = if ctx.get::<bool>(3)? {
            LocaleFallback::AdoptedFirst
        } else {
            LocaleFallback::UnboundInputOnly
        };
        Ok(select_locale(&available, &chain, fallback))
    })?;
    conn.create_scalar_function("protocol_name_prefix", 11, flags, |ctx| {
        let library: LibraryId = parse_id(ctx.get::<String>(0)?)?;
        let media: MediaId = parse_id(ctx.get::<String>(1)?)?;
        let season: i32 = ctx.get(2)?;
        let episode: i32 = ctx.get(3)?;
        let title: String = ctx.get(4)?;
        let series: bool = ctx.get(5)?;
        let prefix: String = ctx.get(6)?;
        if series {
            return Ok(title.to_lowercase().starts_with(&prefix));
        }
        let locale: Option<String> = ctx.get(7)?;
        let inputs: String = ctx.get(8)?;
        let adopted: Option<String> = ctx.get(9)?;
        let sources: String = ctx.get(10)?;
        let mut facts = Facts::default();
        let scope = if season >= 0 {
            EntityScope::Episode {
                season: season as u32,
                episode: episode as u32,
            }
        } else {
            EntityScope::Work
        };
        load_inputs(&mut facts, library, media, scope, &inputs)?;
        if let (Some(locale), Some(adopted)) = (locale.as_deref(), adopted) {
            load_adopted(&mut facts, media, locale, &adopted)?;
            load_sources(&mut facts, media, locale, scope, &sources)?;
        }
        let work = fields(&facts, library, media, EntityScope::Work, locale.as_deref());
        let mut value = fields(&facts, library, media, scope, locale.as_deref());
        if season >= 0 {
            overlay_missing(&mut value, &work);
        }
        let fallback = if season >= 0 {
            format!("S{season:02}E{episode:02}")
        } else {
            title
        };
        let name = match value.title {
            FieldValue::Value(name) => name,
            _ => fallback,
        };
        Ok(name.to_lowercase().starts_with(&prefix))
    })?;
    Ok(())
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
}

fn parse_id<T: std::str::FromStr>(raw: String) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    raw.parse()
        .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))
}

fn load_inputs(
    facts: &mut Facts,
    library: LibraryId,
    media: MediaId,
    scope: EntityScope,
    raw: &str,
) -> rusqlite::Result<()> {
    let rows: Vec<(String, Option<String>)> = parse(raw)?;
    for (kind, json) in rows {
        if let Some(json) = json {
            facts.inputs.insert(
                (
                    library,
                    media,
                    if kind == "work" {
                        EntityScope::Work
                    } else {
                        scope
                    },
                ),
                parse(&json)?,
            );
        }
    }
    Ok(())
}

fn load_adopted(
    facts: &mut Facts,
    media: MediaId,
    locale: &str,
    raw: &str,
) -> rusqlite::Result<()> {
    let (title, details, origins): (String, String, String) = parse(raw)?;
    let _: serde_json::Value = parse(&details)?;
    let _: serde_json::Value = parse(&origins)?;
    facts.adopted.insert(
        (media, locale.into()),
        Adopted {
            title,
            details,
            origins,
        },
    );
    Ok(())
}

fn load_sources(
    facts: &mut Facts,
    media: MediaId,
    locale: &str,
    scope: EntityScope,
    raw: &str,
) -> rusqlite::Result<()> {
    let rows: Vec<(
        String,
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
        Option<i64>,
    )> = parse(raw)?;
    for (
        kind,
        source,
        provider_entity_id,
        revision,
        json,
        origins_json,
        override_json,
        override_revision,
    ) in rows
    {
        let target = if kind == "work" {
            EntityScope::Work
        } else {
            scope
        };
        let key = (media, target, locale.to_string());
        if let Some(json) = override_json {
            facts.overrides.insert(
                key.clone(),
                (
                    parse::<MetadataFields>(&json)?,
                    override_revision.unwrap_or_default(),
                ),
            );
        }
        if !source.is_empty() {
            let _: serde_json::Value = parse(&origins_json)?;
            let (origins, pending) = parse_selection_origins(&origins_json);
            facts.sources.entry(key).or_default().push(SourceSnapshot {
                source,
                provider_entity_id,
                revision,
                fields: parse(&json)?,
                origins,
                pending,
            });
        }
    }
    for snapshots in facts.sources.values_mut() {
        snapshots.sort_by(|a, b| {
            (source_rank(&a.source), a.source.as_str())
                .cmp(&(source_rank(&b.source), b.source.as_str()))
        });
    }
    Ok(())
}

pub(super) fn predicate(branch: Branch, prefix: &str) -> String {
    let (season, episode) = if branch == Branch::Unit {
        ("u.season", "u.episode")
    } else {
        ("-1", "-1")
    };
    let chain = "COALESCE(s.pref_locale,''),COALESCE(s.instance_locale,'')";
    let adopted_locale = format!(
        "protocol_pick_locale((SELECT json_group_array(locale) FROM media_metadata WHERE media_id=s.media_id),{chain},1)"
    );
    let input = |kind: &str, se: &str, ep: &str| {
        let base = format!(
            "FROM library_metadata_inputs mi WHERE mi.library_id=s.library_id AND mi.media_id=s.media_id AND mi.entity_scope='{kind}' AND mi.season={se} AND mi.episode={ep} AND mi.parse_state='valid'"
        );
        format!(
            "(SELECT mi.fields_json {base} AND mi.locale=protocol_pick_locale((SELECT json_group_array(mi.locale) {base}),{chain},0) ORDER BY mi.input_key LIMIT 1)"
        )
    };
    let work_input = input("work", "-1", "-1");
    let episode_input = if branch == Branch::Unit {
        input("episode", season, episode)
    } else {
        "NULL".into()
    };
    let scope_filter = format!(
        "((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='episode' AND season={season} AND episode={episode}))"
    );
    let adopted = format!(
        "(SELECT json_array(title,details_json,field_origins_json) FROM media_metadata WHERE media_id=s.media_id AND locale={adopted_locale})"
    );
    let sources = format!(
        "(SELECT json_group_array(json_array(kind,source,provider,revision,fields,origins,override_fields,override_revision)) FROM (\
        SELECT ms.entity_scope AS kind,ms.source AS source,ms.provider_entity_id AS provider,ms.source_revision AS revision,ms.normalized_json AS fields,ms.selection_origins_json AS origins,NULL AS override_fields,NULL AS override_revision FROM media_metadata_sources ms WHERE ms.media_id=s.media_id AND ms.locale={adopted_locale} AND {scope_filter} AND ms.source IN ('tmdb','douban','tvdb','bangumi','anilist','fanart') \
        UNION ALL SELECT mo.entity_scope,'','',0,'{{}}','{{}}',mo.fields_json,mo.revision FROM media_metadata_overrides mo WHERE mo.media_id=s.media_id AND mo.locale={adopted_locale} AND {scope_filter}))"
    );
    let title = "(SELECT title FROM media WHERE media_id=s.media_id)";
    // CASE evaluates only the selected branch: Series Name is the Media title,
    // so its candidates never load the unrelated cold metadata arguments.
    format!(
        "CASE WHEN s.kind='tv' AND {season}=-1 THEN instr(casefold({title}),{prefix})=1 ELSE protocol_name_prefix(s.library_id,s.media_id,{season},{episode},{title},0,{prefix},{adopted_locale},json_array(json_array('work',{work_input}),json_array('episode',{episode_input})),{adopted},{sources}) END"
    )
}
