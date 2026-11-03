//! Read physical facts and operation scratch without holding the writer.
use super::*;

pub(crate) fn prepare(conn: &Connection, operation: &str) -> Result<Option<Prepared>, StoreError> {
    let row:Option<(String,String,String,String,Option<String>)>=conn.query_row(
        "SELECT library_id,kind,state,plan_json,cursor_json FROM library_operations WHERE operation_id=?1",
        [operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((library, kind, status, plan, before)) = row else {
        return Ok(None);
    };
    if kind != "integrity" || status == "succeeded" {
        return Ok(None);
    }
    IntegrityOwner::acquire(conn, operation, library.parse()?)?;
    let mut state = read_integrity_cursor(conn, operation)?;
    let plan: IntegrityPlan = serde_json::from_str(&plan)?;
    let mut actions = Vec::new();
    state.step = state.step.saturating_add(1);
    if state.current.is_none() {
        let next = next_media(conn, &library, state.last_media_id.as_deref())?;
        state.current = next.map(MediaPage::new);
    }
    let mut finished_media = false;
    let finish = state.current.is_none();
    if let Some(mut page) = state.current.take() {
        match page.phase {
            Phase::Files => facts::files(conn, &library, &mut page, &mut actions)?,
            Phase::Coverage => facts::coverage(conn, &library, &mut page, &mut actions)?,
            Phase::Header => prepare_header(
                conn,
                operation,
                &library,
                &mut page,
                plan.repair,
                &mut actions,
            )?,
            Phase::Users => users(conn, operation, &library, &mut page, &mut actions)?,
            Phase::Sync => sync::expected_page(
                conn,
                operation,
                &library,
                &mut page,
                plan.repair,
                false,
                &mut actions,
            )?,
            Phase::Reverse => sync::actual_page(
                conn,
                operation,
                &library,
                &mut page,
                plan.repair,
                false,
                &mut actions,
            )?,
            Phase::Verify => sync::expected_page(
                conn,
                operation,
                &library,
                &mut page,
                false,
                true,
                &mut actions,
            )?,
            Phase::VerifyReverse => sync::actual_page(
                conn,
                operation,
                &library,
                &mut page,
                false,
                true,
                &mut actions,
            )?,
            Phase::VerifyHeader => {
                actions.push(Mutation::Header(header(conn, operation, &library, &page)?));
                page.phase = Phase::Members;
            }
            Phase::Members => {
                let terms = rows::page(
                    conn,
                    operation,
                    &page.media,
                    "web_term",
                    page.after.as_ref(),
                )?;
                for row in &terms {
                    if rows::text(row, "term_kind") == "resolution" {
                        actions.push(Mutation::Resolution(rows::text(row, "term_value")));
                    }
                    page.after = Some(rows::key("web_term", row));
                }
                if terms.len() < 100 {
                    page.after = None;
                    page.phase = Phase::Cleanup;
                }
            }
            Phase::Cleanup => {
                let cleanup = sync::cleanup(conn, operation, &page)?;
                if cleanup.is_empty() {
                    finished_media = true;
                } else {
                    actions.extend(
                        cleanup
                            .into_iter()
                            .map(|(surface, key)| Mutation::Cleanup(surface, key)),
                    );
                }
            }
        }
        state.current = Some(page);
    }
    Ok(Some(Prepared {
        library,
        before,
        state,
        plan,
        actions,
        finished_media,
        finish,
    }))
}

fn next_media(
    conn: &Connection,
    library: &str,
    after: Option<&str>,
) -> Result<Option<String>, StoreError> {
    let tables = crate::library_operations::integrity_guard::MEDIA_OWNERSHIP_TABLES;
    let mut next: Option<String> = None;
    for table in tables {
        let id:Option<String>=conn.query_row(&format!("SELECT media_id FROM {table} WHERE library_id=?1 AND media_id>COALESCE(?2,'') ORDER BY media_id LIMIT 1"),params![library,after],|r|r.get(0)).optional()?;
        if let Some(id) = id
            && next.as_ref().is_none_or(|old| id < *old)
        {
            next = Some(id);
        }
    }
    Ok(next)
}

fn header(
    conn: &Connection,
    _operation: &str,
    library: &str,
    page: &MediaPage,
) -> Result<ExpectedProjection, StoreError> {
    let header = crate::query_projection::expected::expected_header(
        conn,
        ProjectionKey {
            library_id: library.parse()?,
            media_id: page.media.parse()?,
        },
        &page.files.counts,
        page.files.preferred.as_ref().map(|p| p.file.clone()),
        page.seasons,
        page.episodes,
        page.first_available,
    )?;
    Ok(header)
}

fn users(
    conn: &Connection,
    operation: &str,
    library: &str,
    page: &mut MediaPage,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    if page.user.is_none() {
        let id:Option<String>=conn.query_row("SELECT min(user_id) FROM (
            SELECT * FROM (SELECT user_id FROM playback_units WHERE media_id=?1 AND user_id>COALESCE(?3,'') ORDER BY user_id LIMIT 1)
            UNION ALL SELECT * FROM (SELECT user_id FROM user_library_items WHERE library_id=?2 AND media_id=?1 AND user_id>COALESCE(?3,'') ORDER BY user_id LIMIT 1)
            UNION ALL SELECT * FROM (SELECT user_id FROM user_library_continue_units WHERE library_id=?2 AND media_id=?1 AND user_id>COALESCE(?3,'') ORDER BY user_id LIMIT 1))",
            params![page.media,library,page.user_after],|r|r.get(0))?;
        let Some(id) = id else {
            page.phase = Phase::Sync;
            return Ok(());
        };
        let is_tv: bool = conn.query_row(
            "SELECT kind='tv' FROM media WHERE media_id=?1",
            [&page.media],
            |r| r.get(0),
        )?;
        page.user = Some(crate::playback::user_summary::IntegrityUser::new(id, is_tv));
    }
    let mut user = page
        .user
        .take()
        .ok_or_else(|| StoreError::Invalid("missing Integrity user".into()))?;
    if !user.facts_done {
        user.facts_page(conn, &page.media)?;
        page.user = Some(user);
        return Ok(());
    }
    if !user.has_facts || page.files.counts.file == 0 {
        page.user_after = Some(user.id);
        return Ok(());
    }
    page.after = user.units_after.map(|(s, e)| Key {
        season: s,
        episode: e,
        ..Key::default()
    });
    let units = facts::units(conn, operation, page)?;
    let continued = user.units_page(conn, library, page.media.parse()?, &units)?;
    out.extend(
        continued
            .into_iter()
            .map(|row| Mutation::Stage("continue", row)),
    );
    page.after = None;
    if units.len() < 100 {
        page.user_after = Some(user.id.clone());
        out.push(Mutation::Stage(
            "user",
            user.finish(library, page.media.parse()?),
        ));
    } else {
        page.user = Some(user);
    }
    Ok(())
}

fn prepare_header(
    conn: &Connection,
    operation: &str,
    library: &str,
    page: &mut MediaPage,
    repair: bool,
    out: &mut Vec<Mutation>,
) -> Result<(), StoreError> {
    let header = header(conn, operation, library, page)?;
    let actual = rows::actual(
        conn,
        "library_items",
        library,
        &page.media,
        "card",
        &Key::default(),
    )?;
    if header.card != actual {
        page.mismatch = true;
        if header.card.is_some() && actual.is_none() {
            page.presence_issues += 1;
            out.push(Mutation::MissingItem);
        }
        if header.card.is_none() && actual.is_some() {
            page.presence_issues += 1;
            out.push(Mutation::OrphanItem);
        }
    }
    if let Some(card) = &header.card {
        out.push(Mutation::Stage("card", card.clone()));
        if repair && actual.as_ref() != Some(card) {
            out.push(Mutation::Upsert("library_items", card.clone()));
        }
    }
    for (kind, value) in header.web {
        let row = OwnedRow::from([
            ("library_id".into(), library.to_string().into()),
            ("media_id".into(), page.media.clone().into()),
            ("term_kind".into(), kind.into()),
            ("term_value".into(), value.into()),
        ]);
        out.push(Mutation::Stage("web_term", row));
    }
    page.phase = Phase::Users;
    Ok(())
}
