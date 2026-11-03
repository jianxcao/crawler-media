//! Owned expectations built only from identity, files, metadata and Playback facts.
use super::types::{CounterMember, Person, ProjectionKey, Terms};
use crate::StoreError;
use crate::library_write::reduce as raw;
use rusqlite::{Connection, params, types::Value};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) type OwnedRow = BTreeMap<String, Value>;
pub(crate) struct ExpectedProjection {
    pub card: Option<OwnedRow>,
    pub web: Terms,
    pub protocol: Terms,
    pub people: BTreeSet<Person>,
    pub entities: super::entities::Entities,
    pub members: BTreeSet<CounterMember>,
}
impl ExpectedProjection {
    fn empty() -> Self {
        Self {
            card: None,
            web: Terms::new(),
            protocol: Terms::new(),
            people: BTreeSet::new(),
            entities: Default::default(),
            members: BTreeSet::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn expected_header(
    tx: &Connection,
    key: ProjectionKey,
    counts: &raw::Counts,
    preferred: Option<String>,
    seasons: i64,
    episodes: i64,
    first: Option<(i32, i32)>,
) -> Result<ExpectedProjection, StoreError> {
    if counts.file == 0 {
        return Ok(ExpectedProjection::empty());
    }
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let facts = raw::facts_for(tx, &lib, &media, preferred.as_deref())?;
    let (kind, identity) = raw::media_identity(tx, &media)?;
    let first = if kind == "tv" { first } else { None };
    let mut web = selected_terms(tx, &lib, &media, &facts, false)?;
    web.extend(super::browse::buckets(
        facts.year,
        facts.runtime,
        facts.rating,
        facts.language.as_deref(),
        counts.hdr,
        counts.missing > 0,
        facts.adopted,
    ));
    let (protocol, people) = super::protocol::derive(tx, &lib, &media, &web)?;
    let entities = super::entities::derive(tx, &lib, &media, &web)?;
    let mut members = super::counts::members(&kind, &web, &protocol);
    members.extend(super::entities::members(&entities));
    let mut card = card_row(
        tx, &lib, &media, counts, &facts, preferred, seasons, episodes, kind, identity,
    )?;
    card.insert(
        "search_text".into(),
        crate::protocol_index::expected_search(tx, &lib, &media, &facts.title)?.into(),
    );
    card.insert("first_owned_season".into(), first.map(|unit| unit.0).into());
    card.insert(
        "first_owned_episode".into(),
        first.map(|unit| unit.1).into(),
    );
    card.insert(
        "protocol_original_title_key".into(),
        super::protocol::original_title(tx, &lib, &media)?.into(),
    );
    Ok(ExpectedProjection {
        card: Some(card),
        web,
        protocol,
        people,
        entities,
        members,
    })
}
pub(crate) fn integer(row: &OwnedRow, key: &str) -> i64 {
    match row.get(key) {
        Some(Value::Integer(n)) => *n,
        _ => 0,
    }
}
#[allow(clippy::too_many_arguments)]
fn card_row(
    tx: &Connection,
    lib: &str,
    media: &str,
    c: &raw::Counts,
    f: &raw::Facts,
    preferred: Option<String>,
    seasons: i64,
    episodes: i64,
    kind: String,
    identity: String,
) -> Result<OwnedRow, StoreError> {
    let entries = [
        ("library_id", Value::from(lib.to_string())),
        ("media_id", media.to_string().into()),
        ("kind", kind.into()),
        ("title", f.title.clone().into()),
        ("title_sort", f.title.clone().into()),
        ("year", f.year.into()),
        ("identity_state", identity.into()),
        ("metadata_revision", f.revision.into()),
        ("release_date_sort", f.release_date.clone().into()),
        ("rating_sort", f.rating.into()),
        ("runtime_sort", f.runtime.into()),
        ("added_at_sort", c.added_ns.unwrap_or(i64::MIN).into()),
        ("language", f.language.clone().into()),
        ("size_bytes", c.size_bytes.into()),
        ("size_sort", c.size_bytes.unwrap_or(0).into()),
        ("resolution_rank", c.resolution_rank.into()),
        ("hdr", i64::from(c.hdr).into()),
        ("scraped", i64::from(f.adopted).into()),
        ("missing", i64::from(c.missing > 0).into()),
        (
            "stable_shuffle_key",
            format!("{:020}", raw::shuffle_key(media)).into(),
        ),
        ("file_count", c.file.into()),
        ("available_file_count", c.available.into()),
        ("missing_file_count", c.missing.into()),
        ("unknown_file_count", c.unknown.into()),
        ("distinct_episode_count", episodes.into()),
        ("distinct_season_count", seasons.into()),
        ("preferred_file_id", preferred.into()),
        ("has_still", i64::from(c.has_still).into()),
        ("poster_ref", c.poster_ref.clone().into()),
        ("backdrop_ref", c.backdrop_ref.clone().into()),
        ("protocol_title_key", f.title.to_lowercase().into()),
    ];
    let mut row: OwnedRow = entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let revision: i64 = tx.query_row(
        "SELECT revision FROM library_query_config WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    row.insert("query_config_revision".into(), revision.into());
    Ok(row)
}
pub(crate) fn raw_terms(
    tx: &Connection,
    lib: &str,
    media: &str,
    facts: &raw::Facts,
) -> Result<Terms, StoreError> {
    selected_terms(tx, lib, media, facts, true)
}

fn selected_terms(
    tx: &Connection,
    lib: &str,
    media: &str,
    f: &raw::Facts,
    include_resolution: bool,
) -> Result<Terms, StoreError> {
    let mut terms = Terms::new();
    for genre in &f.genres {
        if !genre.trim().is_empty() {
            terms.insert(("genre".into(), genre.clone()));
        }
    }
    for country in &f.countries {
        if !country.trim().is_empty() {
            terms.insert(("country".into(), country.clone()));
        }
    }
    for (kind, sql) in [
        (
            "resolution",
            "SELECT DISTINCT resolution FROM library_files WHERE library_id=?1 AND media_id=?2 AND publication_state='published' AND resolution IS NOT NULL AND TRIM(resolution)<>''",
        ),
        (
            "collection",
            "SELECT c.id FROM collections c JOIN collection_items ci ON ci.collection_id=c.id WHERE ci.media_id=?2 AND c.deleting=0 AND c.visibility='public' AND c.user_id IS NULL AND ?1 IS NOT NULL",
        ),
    ] {
        if kind == "resolution" && !include_resolution {
            continue;
        }
        for value in tx
            .prepare_cached(sql)?
            .query_map(params![lib, media], |r| r.get::<_, String>(0))?
        {
            terms.insert((kind.into(), value?));
        }
    }
    Ok(terms)
}
