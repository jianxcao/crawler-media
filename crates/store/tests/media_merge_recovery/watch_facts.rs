use super::*;

/// Media-scoped watch facts are not keyed by file, so they are not carried by the
/// file move: a merge must re-point them onto the survivor itself. The retired
/// identity keeps no timeline markers, no fingerprint season model and no
/// season-comparison history, and the survivor's own value wins per coordinate.
#[test]
fn merge_carries_watch_facts_and_leaves_no_loser_references() {
    let f = setup_pending_merge();
    let db = Connection::open(f.tmp.path().join("library.db")).unwrap();
    let (survivor, loser) = (f.survivor.to_string(), f.loser.to_string());

    // The survivor already observed S01E01; the loser observed that same episode
    // plus a lone S01E02 that only its (now moved) file knows about.
    insert_marker(&db, &survivor, 1, 1, 10_000, 20_000);
    insert_marker(&db, &loser, 1, 1, 30_000, 40_000);
    insert_marker(&db, &loser, 1, 2, 50_000, 60_000);
    insert_season_model(&db, "model-loser", &loser, 1);
    insert_comparison(&db, &survivor, 1, "survivor-digest");
    insert_comparison(&db, &loser, 1, "loser-digest");

    let status = f.store.advance_media_merge(&f.op, 2000).unwrap();
    assert_eq!(status.state, "succeeded");

    assert_eq!(
        markers(&db, &survivor),
        vec![
            (1, 1, Some(10_000), Some(20_000)),
            (1, 1, Some(30_000), Some(40_000)),
            (1, 2, Some(50_000), Some(60_000))
        ],
        "both physical file cuts retain their own boundary after Media merge"
    );
    assert_eq!(
        markers(&db, &loser),
        Vec::new(),
        "retired loser kept markers"
    );

    let model: (String, i64) = db
        .query_row(
            "SELECT media_id, season FROM fingerprint_season_models WHERE model_id = 'model-loser'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the loser's season model must survive on the survivor");
    assert_eq!(model, (survivor.clone(), 1));

    let (survivor_digest, loser_history) = (comparison(&db, &survivor), comparison(&db, &loser));
    assert_eq!(survivor_digest, "survivor-digest");
    assert_eq!(loser_history, "");

    for table in ["library_items", "library_item_terms", "user_library_items"] {
        assert_eq!(
            rows_for(&db, table, &loser),
            0,
            "retired loser kept {table}"
        );
    }
}

fn insert_marker(db: &Connection, media: &str, season: i64, episode: i64, start: i64, end: i64) {
    db.execute(
        "INSERT INTO file_markers
         (file_id,file_revision,source_version,season,episode,intro_start_ms,intro_end_ms,source,locked,updated_at)
         SELECT file_id,file_revision,source_version,?2,?3,?4,?5,'detected',0,1 FROM library_files WHERE media_id=?1",
        params![media, season, episode, start, end],
    )
    .unwrap();
}

fn insert_season_model(db: &Connection, model_id: &str, media: &str, season: i64) {
    db.execute(
        "INSERT INTO fingerprint_season_models
         (model_id, media_id, season, kind, model_version, membership_key, policy_key, model_json,
          created_at_ms)
         VALUES (?1, ?2, ?3, 'intro', 1, 'membership', 'policy', '{}', 1)",
        params![model_id, media, season],
    )
    .unwrap();
}

fn insert_comparison(db: &Connection, media: &str, season: i64, digest: &str) {
    db.execute(
        "INSERT INTO probe_season_comparison_history
         (media_id, season, comparison_digest, published_at_ms) VALUES (?1, ?2, ?3, 1)",
        params![media, season, digest],
    )
    .unwrap();
}

fn markers(db: &Connection, media: &str) -> Vec<(i64, i64, Option<i64>, Option<i64>)> {
    db.prepare(
        "SELECT m.season,m.episode,m.intro_start_ms,m.intro_end_ms FROM file_markers m
         JOIN library_files f ON f.file_id=m.file_id WHERE f.media_id=?1 ORDER BY m.season,m.episode,m.intro_start_ms",
    )
    .unwrap()
    .query_map(params![media], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn comparison(db: &Connection, media: &str) -> String {
    db.query_row(
        "SELECT comparison_digest FROM probe_season_comparison_history WHERE media_id = ?1",
        params![media],
        |r| r.get(0),
    )
    .unwrap_or_default()
}
