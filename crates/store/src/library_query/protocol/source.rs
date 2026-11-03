use super::scope::Scope;

pub(super) fn source_expr(scope: &Scope) -> String {
    let mut parts = Vec::new();
    if scope.granularity.wants_unit() {
        parts.push(
            // Keep coverage first: one episode's versions bound this lookup,
            // rather than every file belonging to the same Media.
            "(SELECT f.file_id FROM library_file_units fu \
               CROSS JOIN library_files f ON f.file_id = fu.file_id \
               WHERE fu.library_id = rows.library_id AND fu.media_id = rows.media_id \
                 AND fu.season = rows.season AND fu.episode = rows.episode \
                 AND f.library_id = rows.library_id AND f.media_id = rows.media_id \
                 AND +f.publication_state = 'published' AND +f.presence_state = 'present' \
               ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1)"
                .to_string(),
        );
    }
    if scope.granularity.wants_work() {
        let unit_guard = if scope.granularity.wants_unit() {
            "rows.season = -1 AND "
        } else {
            ""
        };
        parts.push(format!(
            "(SELECT f.file_id FROM library_files f \
               WHERE f.library_id = rows.library_id AND f.media_id = rows.media_id \
                 AND {unit_guard}+f.publication_state = 'published' \
                 AND +f.presence_state = 'present' \
               ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1)"
        ));
    }
    if parts.len() == 1 {
        return parts.pop().unwrap_or_else(|| "NULL".to_string());
    }
    format!("COALESCE({})", parts.join(", "))
}
