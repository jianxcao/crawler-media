use std::fs;
use std::io::{self, Error, ErrorKind};
use std::path::Path;
use tracing::info;

use crate::nfo::NfoMeta;

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn write_season_nfo(path: &Path, meta: &NfoMeta) -> io::Result<()> {
    if path.is_file() {
        let existing = fs::read_to_string(path)?;
        let doc = roxmltree::Document::parse(&existing)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
        if doc.root_element().tag_name().name() != "season" {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "existing file does not have <season> root tag",
            ));
        }
    }

    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<season>\n");

    if let Some(ref title) = meta.title {
        if !title.is_empty() {
            body.push_str(&format!("  <title>{}</title>\n", escape_xml(title)));
        }
    }

    if let Some(ref plot) = meta.plot {
        if !plot.is_empty() {
            body.push_str(&format!("  <plot>{}</plot>\n", escape_xml(plot)));
        }
    }

    if let Some(ref premiered) = meta.premiered {
        if !premiered.is_empty() {
            body.push_str(&format!("  <premiered>{}</premiered>\n", escape_xml(premiered)));
            if premiered.len() >= 4 {
                if let Ok(year) = premiered[0..4].parse::<u16>() {
                    body.push_str(&format!("  <year>{}</year>\n", year));
                }
            }
        }
    }

    if let Some(season) = meta.season {
        body.push_str(&format!("  <seasonnumber>{}</seasonnumber>\n", season));
    }

    if let Some(ref thumb) = meta.thumb {
        if !thumb.is_empty() {
            body.push_str(&format!("  <thumb>{}</thumb>\n", escape_xml(thumb)));
        }
    }

    body.push_str("</season>\n");

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, body)?;
    info!(path = %path.display(), "已写入 season.nfo");
    Ok(())
}
