use super::scope::Scope;

pub(super) fn source_expr(scope: &Scope) -> String {
    let mut parts = Vec::new();
    if scope.granularity.wants_unit() {
        parts.push(
            "(SELECT f.file_id FROM library_files f \
               JOIN library_file_units fu ON fu.file_id = f.file_id \
                 AND fu.season = rows.season AND fu.episode = rows.episode \
               WHERE f.library_id = rows.library_id AND f.media_id = rows.media_id \
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
