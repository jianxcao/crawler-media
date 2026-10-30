//! Owned expectations built only from identity, files, metadata and Playback facts.
use super::types::{CounterMember, Person, ProjectionKey, Terms};
use crate::library_write::reduce as raw;
use crate::{Store, StoreError};
use rusqlite::{Connection, params, types::Value};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) type OwnedRow = BTreeMap<String, Value>;
pub(crate) struct ExpectedProjection {
    pub card: Option<OwnedRow>,
    pub units: Vec<OwnedRow>,
    pub users: Vec<OwnedRow>,
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
            units: vec![],
            users: vec![],
            web: Terms::new(),
            protocol: Terms::new(),
            people: BTreeSet::new(),
            entities: Default::default(),
            members: BTreeSet::new(),
        }
    }
}
pub(crate) fn expected_projection(
    tx: &Connection,
    key: ProjectionKey,
) -> Result<ExpectedProjection, StoreError> {
    let (lib, media) = (key.library_id.to_string(), key.media_id.to_string());
    let files = raw::published_files(tx, &lib, &media)?;
    if files.is_empty() {
        return Ok(ExpectedProjection::empty());
    }
    let counts = raw::counts_of(&files);
    let preferred = raw::preferred_file(&files);
    let facts = raw::facts_for(tx, &lib, &media, preferred.as_deref())?;
    let units = unit_rows(tx, &lib, &media)?;
    let owned: Vec<(i32, i32)> = units
        .iter()
        .map(|r| (integer(r, "season") as i32, integer(r, "episode") as i32))
        .collect();
    let seasons = owned
        .iter()
        .filter(|(s, _)| *s >= 0)
        .map(|(s, _)| *s)
        .collect::<BTreeSet<_>>()
        .len() as i64;
    let episodes = owned.iter().filter(|(s, e)| *s >= 0 && *e >= 0).count() as i64;
    let (kind, identity) = raw::media_identity(tx, &media)?;
    let mut web = raw_terms(tx, &lib, &media, &facts)?;
    web.extend(super::browse::buckets(
        facts.year,
        facts.runtime,
        facts.rating,
        facts.language.as_deref(),
        counts.hdr,
        counts.missing > 0,
        facts.adopted,
    ));
    let search = crate::protocol_index::expected_search(tx, &lib, &media, &facts.title)?;
    let (protocol, people) = super::protocol::derive(tx, &lib, &media, &web)?;
    let entities = super::entities::derive(tx, &lib, &media, &web)?;
    let mut members = super::counts::members(&kind, &web, &protocol);
    members.extend(super::entities::members(&entities));
    let users = Store::expected_user_rows(tx, &lib, key.media_id, &owned, kind == "tv")?;
    let mut card = card_row(
        tx, &lib, &media, &counts, &facts, preferred, seasons, episodes, kind, identity,
    )?;
    card.insert("search_text".into(), search.into());
    card.insert(
        "protocol_original_title_key".into(),
        super::protocol::original_title(tx, &lib, &media)?.into(),
    );
    Ok(ExpectedProjection {
        card: Some(card),
        units,
        users,
        web,
        protocol,
        people,
        entities,
        members,
    })
}
pub(super) fn integer(row: &OwnedRow, key: &str) -> i64 {
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
fn unit_rows(tx: &Connection, lib: &str, media: &str) -> Result<Vec<OwnedRow>, StoreError> {
    let sql="SELECT u.season,u.episode,COUNT(DISTINCT u.file_id),COUNT(DISTINCT CASE WHEN f.presence_state='present' THEN u.file_id END),
        (SELECT u2.file_id FROM library_file_units u2 JOIN library_files f2 ON f2.file_id=u2.file_id
        WHERE f2.library_id=?1 AND f2.media_id=?2 AND f2.publication_state='published' AND +f2.presence_state='present'
        AND u2.season=u.season AND u2.episode=u.episode ORDER BY f2.file_revision DESC,f2.file_id ASC LIMIT 1)
        FROM library_file_units u JOIN library_files f ON f.file_id=u.file_id
        WHERE f.library_id=?1 AND f.media_id=?2 AND f.publication_state='published' GROUP BY u.season,u.episode";
    Ok(tx
        .prepare_cached(sql)?
        .query_map(params![lib, media], |r| {
            let mut row = OwnedRow::new();
            for (i, k) in [
                "season",
                "episode",
                "file_count",
                "available_file_count",
                "preferred_file_id",
            ]
            .iter()
            .enumerate()
            {
                row.insert(k.to_string(), r.get(i)?);
            }
            row.insert("library_id".into(), lib.to_string().into());
            row.insert("media_id".into(), media.to_string().into());
            Ok(row)
        })?
        .collect::<Result<_, _>>()?)
}
pub(crate) fn raw_terms(
    tx: &Connection,
    lib: &str,
    media: &str,
    f: &raw::Facts,
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
            "SELECT c.id FROM collections c JOIN collection_items ci ON ci.collection_id=c.id WHERE ci.media_id=?2 AND c.visibility='public' AND c.user_id IS NULL AND ?1 IS NOT NULL",
        ),
    ] {
        for value in tx
            .prepare_cached(sql)?
            .query_map(params![lib, media], |r| r.get::<_, String>(0))?
        {
            terms.insert((kind.into(), value?));
        }
    }
    Ok(terms)
}
