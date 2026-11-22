use std::path::{Path, PathBuf};

use domain::MediaId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{RenameSidecar, SidecarAction, SidecarSourceIdentity};
use crate::StoreError;

#[allow(clippy::too_many_arguments)]
pub(super) fn migrate_metadata_inputs(
    tx: &Connection,
    from_library: &str,
    to_library: &str,
    to_root: &str,
    media_id: MediaId,
    file_id: &str,
    from_video: &str,
    to_video: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    for side in sidecars {
        let is_nfo = side.kind == "nfo"
            || side
                .from
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("nfo"));
        if !is_nfo {
            continue;
        }
        migrate_nfo_sidecar_input(
            tx,
            from_library,
            to_library,
            to_root,
            file_id,
            from_video,
            to_video,
            side,
        )?;
    }
    migrate_remaining_video_inputs(
        tx,
        from_library,
        to_library,
        to_root,
        media_id,
        from_video,
        to_video,
    )
}

#[allow(clippy::too_many_arguments)]
fn migrate_nfo_sidecar_input(
    tx: &Connection,
    from_library: &str,
    to_library: &str,
    to_root: &str,
    file_id: &str,
    from_video: &str,
    to_video: &str,
    side: &RenameSidecar,
) -> Result<(), StoreError> {
    let from_nfo = side.from.to_string_lossy().into_owned();
    let to_nfo = side.to.to_string_lossy().into_owned();
    tx.execute(
        "INSERT INTO library_metadata_inputs
            (library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,
             parse_state,source_version,fields_json,present_fields_json,provenance_json,
             export_id,export_digest,export_fields_json,revision)
         SELECT ?2,media_id,entity_scope,season,episode,locale,norm_path_key(?3),?4,
                CASE WHEN path_key = norm_path_key(?5) THEN norm_path_key(?6) ELSE path_key END,
                parse_state,source_version,fields_json,present_fields_json,provenance_json,
                export_id,export_digest,export_fields_json,revision+1
         FROM library_metadata_inputs
         WHERE library_id = ?1 AND input_key = norm_path_key(?7)
         ON CONFLICT DO UPDATE SET
            root_id=excluded.root_id,
            path_key=excluded.path_key,
            parse_state=excluded.parse_state,
            source_version=excluded.source_version,
            fields_json=excluded.fields_json,
            present_fields_json=excluded.present_fields_json,
            provenance_json=excluded.provenance_json,
            export_id=excluded.export_id,
            export_digest=excluded.export_digest,
            export_fields_json=excluded.export_fields_json,
            revision=library_metadata_inputs.revision+1",
        params![
            from_library,
            to_library,
            to_nfo,
            to_root,
            from_video,
            to_video,
            from_nfo,
        ],
    )?;
    let delete_old = side.action.deletes_source()
        || (side.action == SidecarAction::SameEntry
            && !super::super::rename_sidecars::nfo_retained_in_library(
                tx,
                from_library,
                file_id,
                Path::new(from_video),
                &side.from,
            )?);
    if delete_old {
        tx.execute(
            "DELETE FROM library_metadata_inputs
             WHERE library_id = ?1 AND input_key = norm_path_key(?2)
               AND NOT (?1 = ?3 AND input_key = norm_path_key(?4))",
            params![from_library, from_nfo, to_library, to_nfo],
        )?;
    }
    Ok(())
}

fn migrate_remaining_video_inputs(
    tx: &Connection,
    from_library: &str,
    to_library: &str,
    to_root: &str,
    media_id: MediaId,
    from_video: &str,
    to_video: &str,
) -> Result<(), StoreError> {
    if from_library == to_library {
        tx.execute(
            "UPDATE library_metadata_inputs
             SET path_key = norm_path_key(?2), root_id = ?3, revision = revision + 1
             WHERE library_id = ?1 AND path_key = norm_path_key(?4)",
            params![from_library, to_video, to_root, from_video],
        )?;
        return Ok(());
    }
    let surviving: Option<(String, String)> = tx
        .query_row(
            "SELECT path_key, root_id FROM library_files
             WHERE library_id = ?1 AND media_id = ?2
             ORDER BY file_id LIMIT 1",
            params![from_library, media_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match surviving {
        Some((surviving_path, surviving_root)) => {
            tx.execute(
                "UPDATE library_metadata_inputs
                 SET path_key = ?2, root_id = ?3, revision = revision + 1
                 WHERE library_id = ?1 AND path_key = norm_path_key(?4)",
                params![from_library, surviving_path, surviving_root, from_video],
            )?;
        }
        None => {
            tx.execute(
                "DELETE FROM library_metadata_inputs
                 WHERE library_id = ?1 AND path_key = norm_path_key(?2)",
                params![from_library, from_video],
            )?;
        }
    }
    Ok(())
}

pub(in crate::library_write) fn merge_registered_sidecars(
    rows: &[(String, String)],
    from_video: &Path,
    to_video: &Path,
    sidecars: &mut Vec<RenameSidecar>,
) {
    let Some(from_parent) = from_video.parent() else {
        return;
    };
    let Some(to_parent) = to_video.parent() else {
        return;
    };
    let from_stem = from_video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let to_stem = to_video.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    for (kind, path) in rows {
        let from = PathBuf::from(path);
        if sidecars.iter().any(|s| s.from == from) {
            continue;
        }
        if !from.is_file() {
            continue;
        }
        // Only move sidecars that live next to the video or share its stem.
        let same_dir = from.parent() == Some(from_parent);
        let shares_stem = from
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| stem == from_stem);
        if !(same_dir || shares_stem) {
            continue;
        }
        let name = from.file_name().map(|n| n.to_os_string());
        let Some(name) = name else { continue };
        let to = if shares_stem {
            let ext = from.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext.is_empty() {
                to_parent.join(to_stem)
            } else {
                to_parent.join(format!("{to_stem}.{ext}"))
            }
        } else {
            to_parent.join(name)
        };
        let source_identity = SidecarSourceIdentity::observe(&from);
        sidecars.push(RenameSidecar {
            kind: kind.clone(),
            from,
            to,
            action: SidecarAction::Move,
            source_identity,
            target_identity: None,
        });
    }
}

pub(super) fn migrate_scrape_sidecars(
    tx: &Connection,
    file_id: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    for side in sidecars {
        let from = side.from.to_string_lossy().into_owned();
        let to = side.to.to_string_lossy().into_owned();
        let from_key = crate::path_key::canonical_recorded_key(tx, &from);
        let to_key = crate::path_key::canonical_recorded_key(tx, &to);
        tx.execute(
            "UPDATE scrape_sidecars SET path = ?3, path_key = ?4
             WHERE file_id = ?1 AND path_key = ?2",
            params![file_id, from_key, to, to_key],
        )?;
    }
    Ok(())
}

pub(super) fn rewrite_artwork_paths(
    tx: &Connection,
    file_id: &str,
    sidecars: &[RenameSidecar],
) -> Result<(), StoreError> {
    for side in sidecars {
        let from = side.from.to_string_lossy().into_owned();
        let to = side.to.to_string_lossy().into_owned();
        let from_key = crate::path_key::canonical_recorded_key(tx, &from);
        let to_key = crate::path_key::canonical_recorded_key(tx, &to);
        tx.execute(
            "UPDATE library_files SET
                poster_path = CASE WHEN poster_path_key = ?2 THEN ?3 ELSE poster_path END,
                poster_path_key = CASE WHEN poster_path_key = ?2 THEN ?4 ELSE poster_path_key END,
                backdrop_path = CASE WHEN backdrop_path_key = ?2 THEN ?3 ELSE backdrop_path END,
                backdrop_path_key = CASE WHEN backdrop_path_key = ?2 THEN ?4 ELSE backdrop_path_key END,
                still_path = CASE WHEN still_path_key = ?2 THEN ?3 ELSE still_path END,
                still_path_key = CASE WHEN still_path_key = ?2 THEN ?4 ELSE still_path_key END
             WHERE file_id = ?1",
            params![file_id, from_key, to, to_key],
        )?;
    }
    Ok(())
}
