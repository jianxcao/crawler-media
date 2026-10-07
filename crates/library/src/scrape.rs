use std::fs;
use std::path::Path;

use domain::{Media, MediaKind};

use crate::LibraryError;

pub fn scrape_beside(
    video: &Path,
    media: &Media,
    enabled: bool,
    poster: Option<&[u8]>,
) -> Result<(), LibraryError> {
    if !enabled {
        return Ok(());
    }
    let Some(stem) = video.file_stem().and_then(|s| s.to_str()) else {
        return Ok(());
    };
    let dir = video.parent().unwrap_or_else(|| Path::new("."));
    fs::write(dir.join(format!("{stem}.nfo")), nfo_xml(media))?;
    if let Some(bytes) = poster.filter(|bytes| !bytes.is_empty()) {
        fs::write(dir.join("poster.jpg"), bytes)?;
    }
    Ok(())
}

pub fn scrape_directory(dir: &Path, media: &Media) -> Result<(), LibraryError> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("mkv")
            || path.extension().and_then(|ext| ext.to_str()) == Some("mp4")
        {
            scrape_beside(&path, media, true, None)?;
        }
    }
    Ok(())
}

fn nfo_xml(media: &Media) -> String {
    let year = media
        .year
        .map(|year| format!("  <year>{year}</year>\n"))
        .unwrap_or_default();
    let aliases = [
        ("tmdbid", media.tmdb_id.as_deref()),
        ("doubanid", media.douban_id.as_deref()),
        ("tvdbid", media.tvdb_id.as_deref()),
        ("bangumiid", media.bangumi_id.as_deref()),
        ("anilistid", media.anilist_id.as_deref()),
    ]
    .into_iter()
    .filter_map(|(tag, id)| id.map(|id| format!("  <{tag}>{id}</{tag}>\n")))
    .collect::<String>();
    let tag = match media.kind {
        MediaKind::Movie => "movie",
        MediaKind::Tv => "tvshow",
        MediaKind::Video => "movie",
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <{tag}>\n  <title>{}</title>\n{year}{aliases}</{tag}>\n",
        media.title
    )
}
