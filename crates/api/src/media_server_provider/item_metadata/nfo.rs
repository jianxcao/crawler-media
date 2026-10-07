use domain::{Media, MediaKind};
use std::path::{Path, PathBuf};

pub(super) fn row_nfo_path(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> Option<PathBuf> {
    let path = Path::new(&row.path);
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    let series_dir = series_directory(row, dir);
    let candidates = if is_series {
        vec![
            series_dir.join("tvshow.nfo"),
            dir.join("tvshow.nfo"),
            dir.join("movie.nfo"),
            dir.join(format!("{stem}.nfo")),
        ]
    } else if media.kind == MediaKind::Tv {
        vec![
            dir.join(format!("{stem}.nfo")),
            series_dir.join("tvshow.nfo"),
        ]
    } else {
        vec![dir.join(format!("{stem}.nfo")), dir.join("movie.nfo")]
    };
    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn series_directory<'a>(row: &domain::LedgerRow, directory: &'a Path) -> &'a Path {
    let is_season_directory = directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().starts_with("season"));
    if row.season.is_some() && is_season_directory {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    }
}

pub(super) fn row_nfo_metadata(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> Option<library::NfoMeta> {
    let mut metadata = row_nfo_path(row, media, is_series)
        .and_then(|path| library::read_nfo(&path))
        .unwrap_or_default();
    if !is_series && media.kind == MediaKind::Tv {
        let path = Path::new(&row.path);
        let show_nfo = path
            .parent()
            .map(|directory| series_directory(row, directory).join("tvshow.nfo"));
        let fallback = show_nfo
            .filter(|path| path.is_file())
            .and_then(|path| library::read_nfo(&path));
        if let Some(fallback) = fallback {
            metadata = merge_nfo_metadata(metadata, fallback);
        }
    }
    (!nfo_is_empty(&metadata)).then_some(metadata)
}

fn merge_nfo_metadata(
    mut preferred: library::NfoMeta,
    fallback: library::NfoMeta,
) -> library::NfoMeta {
    preferred.title = preferred.title.or(fallback.title);
    preferred.original_title = preferred.original_title.or(fallback.original_title);
    preferred.year = preferred.year.or(fallback.year);
    preferred.plot = preferred.plot.or(fallback.plot);
    preferred.rating = preferred.rating.or(fallback.rating);
    preferred.runtime_minutes = preferred.runtime_minutes.or(fallback.runtime_minutes);
    preferred.tagline = preferred.tagline.or(fallback.tagline);
    preferred.premiered = preferred.premiered.or(fallback.premiered);
    preferred.end_date = preferred.end_date.or(fallback.end_date);
    preferred.content_rating = preferred.content_rating.or(fallback.content_rating);
    preferred.vote_count = preferred.vote_count.or(fallback.vote_count);
    preferred.original_language = preferred.original_language.or(fallback.original_language);
    preferred.status = preferred.status.or(fallback.status);
    preferred.number_of_seasons = preferred.number_of_seasons.or(fallback.number_of_seasons);
    preferred.number_of_episodes = preferred.number_of_episodes.or(fallback.number_of_episodes);
    preferred.aired = preferred.aired.or(fallback.aired);
    preferred.thumb = preferred.thumb.or(fallback.thumb);
    preferred.genres = merge_strings(preferred.genres, fallback.genres);
    preferred.countries = merge_strings(preferred.countries, fallback.countries);
    preferred.studios = merge_strings(preferred.studios, fallback.studios);
    preferred.directors = merge_strings(preferred.directors, fallback.directors);
    preferred.creators = merge_strings(preferred.creators, fallback.creators);
    let mut cast = preferred.cast;
    let mut people = cast
        .iter()
        .map(cast_member_key)
        .collect::<std::collections::HashSet<_>>();
    for member in fallback.cast {
        if people.insert(cast_member_key(&member)) {
            cast.push(member);
        }
    }
    preferred.cast = cast;
    preferred
}

fn merge_strings(mut preferred: Vec<String>, fallback: Vec<String>) -> Vec<String> {
    let mut seen = preferred
        .iter()
        .map(|value| value.to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    for value in fallback {
        if seen.insert(value.to_lowercase()) {
            preferred.push(value);
        }
    }
    preferred
}

fn cast_member_key(member: &library::CastMember) -> String {
    member
        .tmdb_id
        .as_deref()
        .map(|id| format!("tmdb:{id}"))
        .unwrap_or_else(|| format!("name:{}", member.name.to_lowercase()))
}

fn nfo_is_empty(metadata: &library::NfoMeta) -> bool {
    metadata.title.is_none()
        && metadata.original_title.is_none()
        && metadata.year.is_none()
        && metadata.plot.is_none()
        && metadata.rating.is_none()
        && metadata.runtime_minutes.is_none()
        && metadata.tagline.is_none()
        && metadata.premiered.is_none()
        && metadata.end_date.is_none()
        && metadata.content_rating.is_none()
        && metadata.vote_count.is_none()
        && metadata.original_language.is_none()
        && metadata.status.is_none()
        && metadata.number_of_seasons.is_none()
        && metadata.number_of_episodes.is_none()
        && metadata.aired.is_none()
        && metadata.genres.is_empty()
        && metadata.countries.is_empty()
        && metadata.studios.is_empty()
        && metadata.directors.is_empty()
        && metadata.creators.is_empty()
        && metadata.cast.is_empty()
        && metadata.thumb.is_none()
}
