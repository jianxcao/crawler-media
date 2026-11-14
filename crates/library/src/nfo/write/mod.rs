//! NFO 写入：在保留无关节点的前提下就地增删字段，并写入 streamdetails。
//!
//! 职责拆分：`queue` 决定要改哪些节点，`elements` 做 XML 元素级增删与文本落盘，
//! `streams` 负责 `<streamdetails>` 的生成。

mod elements;
mod queue;
mod streams;

use std::path::Path;

use domain::Media;

use super::parse::{NfoMeta, is_supported_root, name_is, parse_nfo};
use elements::apply_edits;
use queue::{NfoEditSource, queue_nfo_edits};

pub use streams::write_streamdetails_into_nfo;

/// XML 文本节点转义（写 NFO 前对标题/剧情/演员名等动态内容转义，
/// 否则含 `&` / `<` 的标题会生成非法 XML，播放器整份读取失败）。
pub(crate) fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Write an NFO beside a file (movie / tvshow / episode by kind).
pub fn write_nfo(path: &Path, media: &Media, meta: Option<&NfoMeta>) -> std::io::Result<()> {
    let is_episode = meta.as_ref().is_some_and(|m| m.episode.is_some());
    let expected_tag = root_tag(media, is_episode);
    let mut body = read_or_seed_nfo(path, expected_tag)?;
    let document = roxmltree::Document::parse(&body).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing to overwrite invalid NFO XML: {error}"),
        )
    })?;
    let root = document.root_element();
    if !is_supported_root(root.tag_name().name()) {
        return Err(invalid_nfo(
            "refusing to overwrite an NFO with an unsupported root element",
        ));
    }
    if is_episode != is_episode_root(root.tag_name().name()) {
        return Err(invalid_nfo(
            "refusing to change an existing NFO between item and episode roots",
        ));
    }
    let existing = parse_nfo(&body).unwrap_or_default();
    let mut edits = Vec::new();
    let mut insertions = String::new();
    queue_nfo_edits(
        NfoEditSource {
            body: &body,
            root,
            existing: &existing,
        },
        media,
        meta,
        is_episode,
        &mut edits,
        &mut insertions,
    );
    body = apply_edits(&body, root, edits, &insertions)?;
    std::fs::write(path, body)
}

fn root_tag(media: &Media, is_episode: bool) -> &'static str {
    if is_episode {
        return "episodedetails";
    }
    match media.kind {
        domain::MediaKind::Movie | domain::MediaKind::Video => "movie",
        domain::MediaKind::Tv => "tvshow",
    }
}

fn read_or_seed_nfo(path: &Path, expected_tag: &str) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(body) => Ok(body),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<{expected_tag}>\n</{expected_tag}>\n"
        )),
        Err(error) => Err(error),
    }
}

fn invalid_nfo(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn is_episode_root(name: &str) -> bool {
    name_is(name, "episodedetails") || name_is(name, "episode")
}
