//! Stream physical facts, including an arbitrarily large single unit's versions.
use super::{
    Mutation, rows,
    state::{Choice, MediaPage, Phase, Unit},
};
use crate::{StoreError, query_projection::expected::OwnedRow};
use rusqlite::{Connection, params};

pub(super) fn files(
    conn: &Connection,
    library: &str,
    page: &mut MediaPage,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    let mut statement=conn.prepare_cached("SELECT file_id,presence_state,size_bytes,resolution,hdr,has_still,has_poster,has_backdrop,COALESCE(created_ns,mtime_ns),file_revision,publication_state
        FROM library_files WHERE library_id=?1 AND media_id=?2 AND file_id>COALESCE(?3,'')
        ORDER BY file_id LIMIT 100")?;
    let facts = statement
        .query_map(params![library, page.media, page.files.after], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, bool>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, Option<i64>>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, String>(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (
        id,
        presence,
        size,
        resolution,
        hdr,
        still,
        poster,
        backdrop,
        added,
        revision,
        publication,
    ) in &facts
    {
        page.files.after = Some(id.clone());
        if publication != "published" {
            continue;
        }
        let c = &mut page.files.counts;
        c.file += 1;
        match presence.as_str() {
            "present" => c.available += 1,
            "missing" => c.missing += 1,
            _ => c.unknown += 1,
        }
        if let Some(size) = size {
            c.size_bytes = Some(c.size_bytes.unwrap_or(0) + size);
        }
        c.has_still |= still;
        c.hdr |= hdr.as_ref().is_some_and(|s| !s.trim().is_empty());
        c.resolution_rank = c
            .resolution_rank
            .max(crate::library_write::reduce::resolution_rank(
                resolution.as_deref().unwrap_or(""),
            ));
        if let Some(added) = added {
            c.added_ns = Some(c.added_ns.map_or(*added, |old| old.min(*added)));
        }
        let choice = Choice {
            file: id.clone(),
            present: presence == "present",
            revision: *revision,
        };
        if choice.present && choice.wins(&page.files.preferred) {
            page.files.preferred = Some(choice.clone());
        }
        if *poster && choice.wins(&page.files.poster) {
            page.files.poster = Some(choice.clone());
        }
        if *backdrop && choice.wins(&page.files.backdrop) {
            page.files.backdrop = Some(choice);
        }
        if let Some(resolution) = resolution.as_ref().filter(|s| !s.trim().is_empty()) {
            let row = OwnedRow::from([
                ("library_id".into(), library.to_string().into()),
                ("media_id".into(), page.media.clone().into()),
                ("term_kind".into(), "resolution".to_string().into()),
                ("term_value".into(), resolution.clone().into()),
            ]);
            out.push(Mutation::Stage("web_term", row));
        }
    }
    if facts.len() < 100 {
        page.files.counts.poster_ref = page.files.poster.as_ref().map(|c| c.file.clone());
        page.files.counts.backdrop_ref = page.files.backdrop.as_ref().map(|c| c.file.clone());
        page.phase = Phase::Coverage;
    }
    Ok(())
}

pub(super) fn coverage(
    conn: &Connection,
    library: &str,
    page: &mut MediaPage,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    let after = page
        .coverage_after
        .clone()
        .unwrap_or((i32::MIN, i32::MIN, String::new()));
    let facts=conn.prepare_cached("SELECT u.season,u.episode,u.file_id,f.presence_state,f.file_revision,f.publication_state
        FROM library_file_units u JOIN library_files f ON f.file_id=u.file_id
        WHERE u.library_id=?1 AND u.media_id=?2 AND (u.season,u.episode,u.file_id)>(?3,?4,?5)
          ORDER BY u.season,u.episode,u.file_id LIMIT 100")?
        .query_map(params![library,page.media,after.0,after.1,after.2],|r|Ok((r.get::<_,i32>(0)?,r.get::<_,i32>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?)))?
        .collect::<Result<Vec<_>,_>>()?;
    for (season, episode, file, presence, revision, publication) in &facts {
        page.coverage_after = Some((*season, *episode, file.clone()));
        if publication != "published" {
            continue;
        }
        if page
            .unit
            .as_ref()
            .is_some_and(|u| (u.season, u.episode) != (*season, *episode))
        {
            finish_unit(library, page, out);
        }
        let unit = page.unit.get_or_insert(Unit {
            season: *season,
            episode: *episode,
            files: 0,
            available: 0,
            preferred: None,
        });
        unit.files += 1;
        if presence == "present" {
            unit.available += 1;
            let choice = Choice {
                file: file.clone(),
                present: true,
                revision: *revision,
            };
            if choice.wins(&unit.preferred) {
                unit.preferred = Some(choice);
            }
        }
    }
    if facts.len() < 100 {
        finish_unit(library, page, out);
        page.phase = Phase::Header;
    }
    Ok(())
}

fn finish_unit(library: &str, page: &mut MediaPage, out: &mut Vec<Mutation>) {
    let Some(unit) = page.unit.take() else {
        return;
    };
    if unit.season >= 0 {
        page.episodes += 1;
        if page.last_season != Some(unit.season) {
            page.seasons += 1;
            page.last_season = Some(unit.season);
        }
        if unit.available > 0 && page.first_available.is_none() {
            page.first_available = Some((unit.season, unit.episode));
        }
    }
    let row = OwnedRow::from([
        ("library_id".into(), library.to_string().into()),
        ("media_id".into(), page.media.clone().into()),
        ("season".into(), unit.season.into()),
        ("episode".into(), unit.episode.into()),
        ("file_count".into(), unit.files.into()),
        ("available_file_count".into(), unit.available.into()),
        (
            "preferred_file_id".into(),
            unit.preferred.map(|c| c.file).into(),
        ),
    ]);
    out.push(Mutation::Stage("unit", row));
}

pub(super) fn units(
    conn: &Connection,
    operation: &str,
    page: &MediaPage,
) -> Result<Vec<OwnedRow>, StoreError> {
    rows::page(conn, operation, &page.media, "unit", page.after.as_ref())
}
