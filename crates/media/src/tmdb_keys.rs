//! Exact request shapes shared by TMDB producers and local cache consumers.

/// Complete TV details shapes, in preference order: identity/translations,
/// rich metadata, and the basic details fallback. Season/similar endpoints differ.
pub fn tv_details_paths(id: &str) -> [String; 3] {
    [
        format!("/tv/{id}?append_to_response=translations,alternative_titles"),
        format!("/tv/{id}?append_to_response=aggregate_credits,content_ratings,translations"),
        format!("/tv/{id}"),
    ]
}

/// The exact TMDB HTTP cache key; empty language omits the query parameter.
pub fn request_key(path: &str, language: &str) -> String {
    if language.is_empty() {
        return path.to_string();
    }
    let join = if path.contains('?') { '&' } else { '?' };
    format!("{path}{join}language={language}")
}
