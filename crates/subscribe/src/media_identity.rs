use domain::Media;

/// Matches a release title against a Media's title or original_title.
pub fn media_title_matches(media: &Media, release_title: &str) -> bool {
    if normalize_title(release_title).is_empty() {
        return false;
    }
    let candidates = [
        media.title.as_str(),
        media.original_title.as_deref().unwrap_or_default(),
    ];
    for want in candidates {
        if !want.is_empty() && title_matches(want, release_title) {
            return true;
        }
    }
    false
}

pub(crate) fn title_matches(wanted: &str, release_title: &str) -> bool {
    let wanted_normalized = normalize_title(wanted);
    let release_normalized = normalize_title(release_title);
    if wanted_normalized.is_empty() || release_normalized.is_empty() {
        return false;
    }
    if wanted_normalized == release_normalized {
        return true;
    }

    let wanted_tokens = title_tokens(wanted);
    let release_tokens = title_tokens(release_title);
    if !wanted_tokens.is_empty()
        && release_tokens
            .windows(wanted_tokens.len())
            .any(|window| window == wanted_tokens.as_slice())
    {
        return true;
    }

    let contains_non_ascii = wanted.chars().any(|ch| !ch.is_ascii());
    let enough_identity = wanted_normalized.chars().count() >= 8
        || (contains_non_ascii && wanted_normalized.chars().count() >= 2);
    enough_identity && release_normalized.contains(&wanted_normalized)
}

pub(crate) fn title_tokens(title: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            token.extend(ch.to_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

pub(crate) fn normalize_title(title: &str) -> String {
    title
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}
