//! Library facts are revalidated on the writer that admits the reservation.
use std::collections::HashSet;

pub(super) fn validate_library_snapshot(
    db: &store::LibraryConnection<'_>,
    expected: &store::SubscribeLibrarySnapshot,
    ignored: &HashSet<String>,
) -> Result<(), String> {
    let ownership = db.library_ownership().map_err(|error| error.to_string())?;
    let same_ownership = expected.ownership.libraries() == ownership.libraries()
        && expected.ownership.libraries().iter().all(|library| {
            expected.ownership.accepts_work(&library.id) == ownership.accepts_work(&library.id)
        });
    let mut fingerprint = db
        .read_fingerprint(
            &expected.fingerprint.library_id,
            expected.subscribe.media_id,
        )
        .map_err(|error| error.to_string())?;
    let mut expected_fingerprint = expected.fingerprint.clone();
    fingerprint
        .files
        .retain(|file| !ignored.contains(&file.file_id));
    expected_fingerprint
        .files
        .retain(|file| !ignored.contains(&file.file_id));
    let mut rows = db
        .file_coverage_for_media(expected.subscribe.media_id)
        .map_err(|error| error.to_string())?;
    rows.retain(|row| !ignored.contains(&row.file.file_id.to_string()));
    let expected_rows: Vec<_> = expected
        .rows
        .iter()
        .filter(|row| !ignored.contains(&row.file.file_id.to_string()))
        .cloned()
        .collect();
    let qualities_match = rows.iter().all(|row| {
        expected.qualities.get(&row.file.file_id)
            == Some(&db.owned_quality(row).map_err(|error| error.to_string()))
    });
    if !same_ownership
        || fingerprint != expected_fingerprint
        || rows != expected_rows
        || !qualities_match
    {
        return Err("Transfer Subscribe facts or Library ownership changed".into());
    }
    Ok(())
}
