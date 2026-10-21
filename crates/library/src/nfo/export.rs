//! Atomic replacement and digest identity for generated NFO sidecars.
//!
//! A generated sidecar is a mirror of effective metadata: the export driver
//! must be able to tell "the file still holds what I last wrote" from "someone
//! else changed the file", and it must never leave a truncated target behind.

use std::io::{self, Write};
use std::path::Path;

use domain::{EntityScope, FieldValue, MetadataCast, MetadataFields, MetadataGenre};
use sha2::{Digest, Sha256};

/// Digest prefix so a stored digest names its algorithm.
pub const NFO_DIGEST_PREFIX: &str = "sha256:";

/// Digest of an NFO body, independent of where it is written.
pub fn nfo_body_digest(body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    format!("{NFO_DIGEST_PREFIX}{:x}", hasher.finalize())
}

/// Digest of the file currently on disk; `None` when there is no file.
pub fn nfo_digest(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            Ok(Some(format!("{NFO_DIGEST_PREFIX}{:x}", hasher.finalize())))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Replace `path` with `body` through a same-directory temporary file and a
/// rename. The target is never truncated: readers either see the old bytes or
/// the new ones.
pub fn replace_nfo_atomically(path: &Path, body: &str) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("nfo path {} has no parent directory", path.display()),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("nfo path {} has no usable file name", path.display()),
            )
        })?;
    let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let write = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(body.as_bytes())?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

/// Render a **whole** NFO body from effective metadata fields.
///
/// This is a rewrite, not a merge: the file becomes exactly what effective
/// metadata says, so a stale element the library no longer believes can never
/// keep the card locked. `Missing` fields are omitted; `Empty` fields become an
/// empty element, which survives a re-parse as "explicitly empty".
///
/// Only fields the parser reads back are rendered, so
/// [`parse_nfo_fields`](super::parse_nfo_fields) of this body round-trips to the
/// same values. Artwork URLs and list provenance have no sidecar syntax of
/// their own and are deliberately not rendered.
pub fn render_nfo(scope: EntityScope, fields: &MetadataFields) -> String {
    let mut body = String::with_capacity(512);
    let root = match scope {
        EntityScope::Work => "movie",
        EntityScope::Season(_) => "season",
        EntityScope::Episode { .. } => "episodedetails",
    };
    body.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    body.push('<');
    body.push_str(root);
    body.push_str(">\n");
    render_scope_numbers(&mut body, scope);
    scalar(&mut body, "title", &fields.title);
    scalar(&mut body, "originaltitle", &fields.original_title);
    scalar(&mut body, "year", &fields.year);
    scalar(&mut body, "plot", &fields.overview);
    scalar(&mut body, "tagline", &fields.tagline);
    scalar(&mut body, "rating", &fields.rating);
    scalar(&mut body, "votes", &fields.vote_count);
    render_runtime(&mut body, fields);
    scalar(&mut body, "mpaa", &fields.content_rating);
    scalar(&mut body, "original_language", &fields.language);
    scalar(&mut body, "status", &fields.status);
    scalar(&mut body, "premiered", &fields.release_date);
    scalar(&mut body, "aired", &fields.air_date);
    scalar(&mut body, "enddate", &fields.end_date);
    list(&mut body, "country", &fields.countries);
    list(&mut body, "studio", &fields.studios);
    list(&mut body, "director", &fields.directors);
    list(&mut body, "creator", &fields.creators);
    render_genres(&mut body, &fields.genres);
    render_cast(&mut body, &fields.cast);
    body.push_str("</");
    body.push_str(root);
    body.push_str(">\n");
    body
}

/// Season / episode coordinates live on the element, not in `MetadataFields`.
fn render_scope_numbers(body: &mut String, scope: EntityScope) {
    match scope {
        EntityScope::Work => {}
        EntityScope::Season(season) => element(body, "season", &season.to_string()),
        EntityScope::Episode { season, episode } => {
            element(body, "season", &season.to_string());
            element(body, "episode", &episode.to_string());
        }
    }
}

/// One `<runtime>` element: the item runtime wins, an episode runtime fills in.
///
/// `Empty` renders an empty element rather than nothing, so the parser reads back
/// "explicitly empty" instead of "unknown" — the same contract as [`scalar`].
fn render_runtime(body: &mut String, fields: &MetadataFields) {
    match &fields.runtime_minutes {
        FieldValue::Missing => {}
        FieldValue::Empty => return element(body, "runtime", ""),
        FieldValue::Value(minutes) => return element(body, "runtime", &minutes.to_string()),
    }
    if let Some(minutes) = fields
        .episode_run_time
        .value()
        .and_then(|runtimes| runtimes.first())
    {
        element(body, "runtime", &minutes.to_string());
    }
}

fn list(body: &mut String, name: &str, values: &FieldValue<Vec<String>>) {
    let Some(values) = values.value() else {
        return;
    };
    for value in values {
        element(body, name, value);
    }
}

fn render_genres(body: &mut String, genres: &FieldValue<Vec<MetadataGenre>>) {
    let Some(genres) = genres.value() else {
        return;
    };
    for genre in genres {
        element(body, "genre", &genre.name);
    }
}

fn render_cast(body: &mut String, cast: &FieldValue<Vec<MetadataCast>>) {
    let Some(cast) = cast.value() else {
        return;
    };
    for member in cast {
        body.push_str("  <actor>\n");
        element(body, "name", &member.name);
        if let Some(role) = member.role.as_deref() {
            element(body, "role", role);
        }
        if let Some(tmdb_id) = member.tmdb_id.as_deref() {
            element(body, "tmdbid", tmdb_id);
        }
        if let Some(thumb) = member.thumb.as_deref() {
            element(body, "thumb", thumb);
        }
        if let Some(order) = member.order {
            element(body, "order", &order.to_string());
        }
        body.push_str("  </actor>\n");
    }
}

fn scalar(body: &mut String, name: &str, value: &FieldValue<impl ToString>) {
    match value {
        FieldValue::Missing => {}
        FieldValue::Empty => element(body, name, ""),
        FieldValue::Value(value) => element(body, name, &value.to_string()),
    }
}

fn element(body: &mut String, name: &str, value: &str) {
    body.push_str("  <");
    body.push_str(name);
    body.push('>');
    body.push_str(&super::xml_escape(value));
    body.push_str("</");
    body.push_str(name);
    body.push_str(">\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nfo::{NfoReadOutcome, parse_nfo_fields};

    fn movie_fields() -> MetadataFields {
        MetadataFields {
            title: FieldValue::Value("Fixture & <Movie>".into()),
            overview: FieldValue::Value("plot".into()),
            year: FieldValue::Value(2020),
            rating: FieldValue::Value(8.5),
            vote_count: FieldValue::Value(100),
            runtime_minutes: FieldValue::Value(120),
            language: FieldValue::Value("en".into()),
            countries: FieldValue::Value(vec!["US".into()]),
            studios: FieldValue::Value(vec!["Fixture Studio".into()]),
            genres: FieldValue::Value(vec![MetadataGenre {
                source: "tmdb".into(),
                source_id: Some("18".into()),
                name: "Drama".into(),
                filter_key: String::new(),
            }]),
            cast: FieldValue::Value(vec![MetadataCast {
                name: "Lead".into(),
                role: Some("Hero".into()),
                tmdb_id: Some("7".into()),
                thumb: None,
                order: Some(0),
            }]),
            ..MetadataFields::default()
        }
    }

    #[test]
    fn rendered_body_round_trips_through_the_parser() {
        let body = render_nfo(EntityScope::Work, &movie_fields());
        let NfoReadOutcome::Valid(parsed) = parse_nfo_fields(&body, EntityScope::Work) else {
            panic!("a rendered body must parse back: {body}");
        };
        assert_eq!(
            parsed.fields.title.value().map(String::as_str),
            Some("Fixture & <Movie>")
        );
        assert_eq!(
            parsed.fields.overview.value().map(String::as_str),
            Some("plot")
        );
        assert_eq!(parsed.fields.runtime_minutes.value(), Some(&120));
        assert_eq!(parsed.fields.year.value(), Some(&2020));
        assert_eq!(
            parsed
                .fields
                .genres
                .value()
                .map(|genres| genres[0].name.as_str()),
            Some("Drama")
        );
        assert_eq!(
            parsed
                .fields
                .cast
                .value()
                .and_then(|cast| cast.first())
                .and_then(|member| member.role.as_deref()),
            Some("Hero")
        );
        // 未渲染的字段保持 Missing：整文件重写不会凭空造出一个值。
        assert!(parsed.fields.tagline.is_missing());
        assert!(parsed.fields.content_rating.is_missing());
    }
}
