//! Real probe, bounded ordered window and exhaustive fallback search statements.
use super::*;
use rusqlite::types::Value;

pub(super) type Query = (String, Vec<Value>);
pub(super) struct Queries {
    pub probe: Query,
    pub window: Query,
    pub fallback: Query,
}
enum Source<'a> {
    Window,
    Fallback,
    Candidates(&'a [i64]),
}
const ALLOWED: &str = "allowed AS MATERIALIZED (
    SELECT l.library_id FROM libraries l
    WHERE l.library_id IN (SELECT value FROM json_each(?2))
      AND l.lifecycle_state='ready' AND l.integrity_state<>'failed'
      AND NOT EXISTS(SELECT 1 FROM library_removals r
        WHERE r.library_id=l.library_id AND r.state<>'succeeded'))";

pub(super) fn queries(
    libraries: &[LibraryId],
    keyword: &str,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Queries, StoreError> {
    Ok(Queries {
        probe: probe(libraries, keyword, limit)?,
        window: page(libraries, keyword, limit, cursor, Source::Window)?,
        fallback: page(libraries, keyword, limit, cursor, Source::Fallback)?,
    })
}
fn probe(libraries: &[LibraryId], keyword: &str, limit: usize) -> Result<Query, StoreError> {
    let needle = keyword.trim().to_lowercase();
    if let Some(phrase) = fts_phrase(&needle) {
        return Ok((
            "SELECT rowid FROM library_search_fts WHERE library_search_fts MATCH ?1 LIMIT ?2"
                .into(),
            vec![phrase.into(), ((limit + 1) as i64).into()],
        ));
    }
    let mut binds = vec![
        needle.clone().into(),
        serde_json::to_string(libraries)?.into(),
    ];
    let (predicate, _) = predicates(&needle, &mut binds);
    let lookahead = bind(&mut binds, ((limit + 1) as i64).into());
    Ok((
        format!(
            "SELECT i.search_rowid FROM library_items i INDEXED BY items_search_prefix
         WHERE i.library_id IN (SELECT value FROM json_each(?2))
           AND {predicate} LIMIT {lookahead}"
        ),
        binds,
    ))
}
pub(super) fn candidate_page(
    libraries: &[LibraryId],
    keyword: &str,
    limit: usize,
    cursor: Option<&str>,
    candidates: &[i64],
) -> Result<Query, StoreError> {
    page(
        libraries,
        keyword,
        limit,
        cursor,
        Source::Candidates(candidates),
    )
}
fn page(
    libraries: &[LibraryId],
    keyword: &str,
    limit: usize,
    cursor: Option<&str>,
    source: Source<'_>,
) -> Result<Query, StoreError> {
    let needle = keyword.trim().to_lowercase();
    let mut binds = vec![
        needle.clone().into(),
        serde_json::to_string(libraries)?.into(),
    ];
    let (predicate, owner_match) = predicates(&needle, &mut binds);
    let matched_by = if fts_phrase(&needle).is_some() {
        "CASE WHEN instr(i.search_text,?1)>0 THEN 'title' ELSE 'person' END"
    } else {
        "'title'"
    };
    let (sort, media) = parse_cursor(cursor)?;
    let after = match (sort, media) {
        (Some(sort), Some(media)) => {
            let sort = bind(&mut binds, sort.into());
            let media = bind(&mut binds, media.into());
            format!("(i.title_sort,i.media_id)>({sort},{media})")
        }
        _ => "i.title_sort>=''".into(),
    };
    let (with, from, predicate, after): (String, String, String, String) = match source {
        Source::Window => {
            // Substring hits can be less dense in title order than prefixes.
            let factor = if fts_phrase(&needle).is_some() { 8 } else { 4 };
            let size = ((limit + 1) * factor).clamp(128, 512);
            let size = bind(&mut binds, (size as i64).into());
            // Take raw ordered keys BEFORE scope/match filtering. Sparse or hidden
            // predecessors can cost at most this window, then use the fallback.
            (
                format!(
                    "WITH {ALLOWED}, raw_window AS MATERIALIZED (
            SELECT i.search_rowid,i.title_sort,i.media_id,i.library_id
            FROM library_items i INDEXED BY items_search_title WHERE {after}
            ORDER BY i.title_sort,i.media_id,i.library_id LIMIT {size})"
                ),
                "raw_window w CROSS JOIN library_items i ON i.rowid=w.search_rowid".into(),
                predicate,
                String::new(),
            )
        }
        Source::Candidates(candidates) => {
            let candidates = bind(&mut binds, serde_json::to_string(candidates)?.into());
            (
                format!("WITH {ALLOWED}"),
                format!(
                    "json_each({candidates}) candidate CROSS JOIN library_items i ON i.rowid=candidate.value"
                ),
                predicate,
                format!(" AND {after}"),
            )
        }
        Source::Fallback => {
            let (from, predicate) = if let Some(phrase) = fts_phrase(&needle) {
                let phrase = bind(&mut binds, phrase.into());
                (
                "library_search_fts CROSS JOIN library_items i ON i.rowid=library_search_fts.rowid"
                    .into(),
                format!("library_search_fts MATCH {phrase} AND {predicate}"),
            )
            } else {
                (
                    "library_items i INDEXED BY items_search_prefix".into(),
                    predicate,
                )
            };
            (
                format!("WITH {ALLOWED}"),
                from,
                predicate,
                format!(" AND {after}"),
            )
        }
    };
    let lookahead = bind(&mut binds, ((limit + 1) as i64).into());
    let candidate_eligible = eligible("i");
    let owner_eligible = eligible("owner");
    // Owner selection is independent of both the raw window and the cursor.
    Ok((
        format!(
            "{with} SELECT i.library_id,i.media_id,i.title,i.kind,i.year,i.title_sort,
          {matched_by} AS matched_by FROM {from} WHERE {candidate_eligible} AND {predicate}
          AND NOT EXISTS(SELECT 1 FROM library_items owner INDEXED BY items_media
            WHERE owner.media_id=i.media_id AND owner.library_id<i.library_id
              AND {owner_eligible} AND {owner_match}) {after}
         ORDER BY i.title_sort,i.media_id LIMIT {lookahead}"
        ),
        binds,
    ))
}
fn eligible(alias: &str) -> String {
    format!(
        "{alias}.library_id IN (SELECT library_id FROM allowed) AND {alias}.available_file_count>0"
    )
}
fn text_match(alias: &str) -> String {
    format!(
        "(instr({alias}.search_text,?1)>0 OR EXISTS(
        SELECT 1 FROM library_protocol_people p WHERE p.library_id={alias}.library_id
          AND p.media_id={alias}.media_id AND instr(p.name_key,?1)>0))"
    )
}
fn bind(values: &mut Vec<Value>, value: Value) -> String {
    values.push(value);
    format!("?{}", values.len())
}
fn predicates(needle: &str, binds: &mut Vec<Value>) -> (String, String) {
    if fts_phrase(needle).is_some() {
        return (text_match("i"), text_match("owner"));
    }
    let upper = if needle.is_empty() {
        None
    } else {
        prefix_upper(needle).map(|upper| bind(binds, upper.into()))
    };
    let prefix = |alias: &str| {
        if needle.is_empty() {
            return "1=1".into();
        }
        let upper = upper
            .as_ref()
            .map(|upper| format!(" AND {alias}.protocol_title_key<{upper}"))
            .unwrap_or_default();
        format!("{alias}.protocol_title_key>=?1{upper}")
    };
    (prefix("i"), prefix("owner"))
}
