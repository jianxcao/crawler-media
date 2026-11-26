use std::path::{Component, Path, PathBuf};

use domain::{Media, MediaKind, Release};

use crate::LibraryError;

const KNOWN: &[&str] = &[
    "title",
    "original_title",
    "year",
    "season",
    "episode",
    "season_episode",
    "part",
    "resolution",
    "source",
    "media_source",
    "codec",
    "hdr",
    "ext",
    "episode_title",
    "tmdb_id",
    "imdb_id",
    "release_group",
];

pub fn default_pattern(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Movie | MediaKind::Video => {
            "{title} ({year})/{title} ({year}){part} - {resolution}{ext}"
        }
        MediaKind::Tv => "{title} ({year})/Season {season}/{title} - {season_episode}{part}{ext}",
    }
}

pub fn validate_pattern(pattern: &str) -> Result<(), LibraryError> {
    for name in placeholders(pattern) {
        let core = core_name(&name);
        if !KNOWN.contains(&core.as_str()) {
            return Err(LibraryError::Naming(format!(
                "unknown placeholder {{{name}}}"
            )));
        }
        if let Some((_, suffix)) = name.split_once(':') {
            let width: usize = suffix
                .strip_suffix('d')
                .and_then(|w| w.parse().ok())
                .ok_or_else(|| LibraryError::Naming(format!("bad pad suffix {{{name}}}")))?;
            if width > 4 {
                return Err(LibraryError::Naming(format!(
                    "pad width too large {{{name}}}"
                )));
            }
            let numeric = matches!(core.as_str(), "season" | "episode" | "year");
            if !numeric {
                return Err(LibraryError::Naming(format!(
                    "{{{name}}}: pad suffix only applies to numeric tokens"
                )));
            }
        }
    }
    Ok(())
}

pub fn render_path(
    root: &Path,
    pattern: &str,
    media: &Media,
    release: &Release,
    src: &Path,
) -> Result<PathBuf, LibraryError> {
    validate_pattern(pattern)?;
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let relative = render_relative(pattern, media, release, &ext)?;
    if relative.is_empty()
        || !Path::new(&relative)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(LibraryError::Naming(format!(
            "invalid rendered relative path: {relative:?}"
        )));
    }
    Ok(root.join(relative))
}

/// The token core of a placeholder: `{season:02d}` → `season`.
fn core_name(name: &str) -> String {
    name.split_once(':')
        .map(|(core, _)| core)
        .unwrap_or(name)
        .to_string()
}

fn placeholders(pattern: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        names.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    names
}

fn ungrouped_placeholders(pattern: &str) -> Vec<String> {
    let mut ungrouped = String::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        let Some(open) = rest.find(['[', '(']) else {
            ungrouped.push_str(rest);
            break;
        };
        ungrouped.push_str(&rest[..open]);
        let opener = rest.as_bytes()[open] as char;
        let closer = if opener == '[' { ']' } else { ')' };
        rest = &rest[open + 1..];
        let Some(close) = rest.find(closer) else {
            ungrouped.push(opener);
            ungrouped.push_str(rest);
            break;
        };
        rest = &rest[close + 1..];
    }
    placeholders(&ungrouped)
}

fn render_relative(
    pattern: &str,
    media: &Media,
    release: &Release,
    ext: &str,
) -> Result<String, LibraryError> {
    if ungrouped_placeholders(pattern)
        .iter()
        .any(|name| core_name(name) == "title")
        && sanitized_value_for("title", media, release, ext).is_empty()
    {
        return Err(LibraryError::Naming(
            "title token is empty after sanitization".into(),
        ));
    }
    let template_segments: Vec<&str> = pattern
        .split('/')
        .filter(|seg| !seg.trim().is_empty())
        .collect();
    if template_segments.is_empty() {
        return Err(LibraryError::Naming("naming pattern is empty".into()));
    }
    let sanitized_ext = sanitized_value_for("ext", media, release, ext);
    let mut out = String::new();
    for template_segment in template_segments {
        let cleaned = if let Some(stem_template) = template_segment.strip_suffix("{ext}") {
            let stem = collapse_segment(&substitute(stem_template, media, release, ext));
            if stem.is_empty() {
                String::new()
            } else {
                format!("{stem}{sanitized_ext}")
            }
        } else {
            collapse_segment(&substitute(template_segment, media, release, ext))
        };
        if cleaned.is_empty() {
            return Err(LibraryError::Naming(format!(
                "naming segment {template_segment:?} collapsed to empty"
            )));
        }
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&cleaned);
    }
    Ok(out)
}

fn substitute(pattern: &str, media: &Media, release: &Release, ext: &str) -> String {
    let empty: Vec<String> = placeholders(pattern)
        .iter()
        .filter(|name| sanitized_value_for(name, media, release, ext).is_empty())
        .cloned()
        .collect();
    let mut out = drop_empty_groups(pattern, &empty);
    for name in placeholders(&out) {
        let value = sanitized_value_for(&name, media, release, ext);
        out = out.replace(&format!("{{{name}}}"), &value);
    }
    out
}

fn sanitized_value_for(name: &str, media: &Media, release: &Release, ext: &str) -> String {
    let raw = value_for(name, media, release, ext);
    if core_name(name) == "ext" {
        let stem = raw.strip_prefix('.').unwrap_or(&raw);
        let cleaned = collapse_segment(&sanitize_token_value(stem));
        return if cleaned.is_empty() {
            String::new()
        } else {
            format!(".{cleaned}")
        };
    }
    collapse_segment(&sanitize_token_value(&raw))
}

/// Strip directory separators, cross-platform forbidden filename characters,
/// and ASCII control bytes from substituted metadata tokens so titles never
/// split into unintended subdirs or produce invalid SMB/Windows filenames.
fn sanitize_token_value(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\x01'..='\x1f' => ' ',
            other => other,
        })
        .collect()
}

/// Remove `[...]` / `(...)` groups that reference an empty placeholder; also
/// trims whitespace inside surviving groups (`[2160p ]` → `[2160p]`).
fn drop_empty_groups(template: &str, empty: &[String]) -> String {
    let mut out = String::new();
    let mut rest = template;
    while !rest.is_empty() {
        let open = rest.find(['[', '(']);
        let Some(open) = open else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open]);
        let opener = rest.as_bytes()[open] as char;
        let closer = if opener == '[' { ']' } else { ')' };
        rest = &rest[open + 1..];
        let Some(close) = rest.find(closer) else {
            out.push(opener);
            out.push_str(rest);
            break;
        };
        let inner = &rest[..close];
        let references_empty = placeholders(inner)
            .iter()
            .any(|name| empty.iter().any(|e| core_name(e) == core_name(name)));
        if !references_empty {
            out.push(opener);
            out.push_str(inner.trim());
            out.push(closer);
        }
        rest = &rest[close + 1..];
    }
    out
}

fn value_for(name: &str, media: &Media, release: &Release, ext: &str) -> String {
    let (core, pad) = name
        .split_once(':')
        .map(|(core, suffix)| {
            let width = suffix
                .strip_suffix('d')
                .and_then(|w| w.parse().ok())
                .unwrap_or(0);
            (core, width)
        })
        .unwrap_or((name, 0));
    let value = value_for_core(core, media, release, ext);
    if pad > 0 {
        match core {
            "season" | "episode" | "year" => format!("{value:0>pad$}"),
            _ => value,
        }
    } else {
        value
    }
}

fn value_for_core(core: &str, media: &Media, release: &Release, ext: &str) -> String {
    match core {
        "title" => media.title.clone(),
        "original_title" => media.original_title.clone().unwrap_or_default(),
        "year" => media
            .year
            .or(release.year)
            .map(|y| y.to_string())
            .unwrap_or_default(),
        "season" => release
            .season
            .map(|s| format!("{s:02}"))
            .unwrap_or_default(),
        "episode" => release
            .episode
            .map(|e| format!("{e:02}"))
            .unwrap_or_default(),
        "season_episode" => season_episode(release),
        "part" => String::new(),
        "resolution" => release.resolution.clone().unwrap_or_default(),
        "source" | "media_source" => release.source.clone().unwrap_or_default(),
        "codec" => release.codec.clone().unwrap_or_default(),
        "hdr" => release.hdr.clone().unwrap_or_default(),
        "ext" => ext.to_string(),
        "episode_title" => String::new(),
        "tmdb_id" => media.tmdb_id.clone().unwrap_or_default(),
        "imdb_id" => String::new(),
        "release_group" => release.group.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

fn season_episode(release: &Release) -> String {
    match (release.season, release.episode, release.episode_to) {
        (Some(season), Some(from), Some(to)) if to != from => {
            format!("S{season:02}E{from:02}-E{to:02}")
        }
        (Some(season), Some(episode), _) => format!("S{season:02}E{episode:02}"),
        _ => String::new(),
    }
}

fn collapse_segment(segment: &str) -> String {
    let mut s = segment.to_string();
    loop {
        let next = s
            .replace("()", "")
            .replace("[]", "")
            .replace("  ", " ")
            .trim_matches([' ', '_', '.', '-'])
            .to_string();
        if next == s {
            break;
        }
        s = next;
    }
    if s == "." || s == ".." {
        return String::new();
    }
    s
}
