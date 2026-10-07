//! Emby/TMM-style NFO sidecars. Parse common Kodi/Emby/Jellyfin XML metadata
//! while preserving unrelated nodes when updating metadata or stream details.

use std::path::Path;

use domain::Media;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NfoMeta {
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub year: Option<String>,
    pub plot: Option<String>,
    pub tagline: Option<String>,
    pub rating: Option<String>,
    pub vote_count: Option<u64>,
    pub runtime_minutes: Option<String>,
    pub premiered: Option<String>,
    pub end_date: Option<String>,
    pub content_rating: Option<String>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub aired: Option<String>,
    pub number_of_seasons: Option<u32>,
    pub number_of_episodes: Option<u32>,
    pub thumb: Option<String>,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub studios: Vec<String>,
    pub directors: Vec<String>,
    pub creators: Vec<String>,
    pub cast: Vec<CastMember>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CastMember {
    pub name: String,
    pub role: Option<String>,
    /// TMDB person id（写进 <actor><tmdbid>，供人物页跳转）。
    pub tmdb_id: Option<String>,
    /// TMDB 头像路径（写进 <actor><thumb>）。
    pub thumb: Option<String>,
    /// 演员在影片演员表中的顺序（写进 <actor><order>）。
    pub order: Option<u32>,
}

/// Parse an NFO file if present; missing/unreadable/invalid XML → None.
pub fn read_nfo(path: &Path) -> Option<NfoMeta> {
    let body = std::fs::read_to_string(path).ok()?;
    parse_nfo(&body)
}

pub fn parse_nfo(body: &str) -> Option<NfoMeta> {
    let document = roxmltree::Document::parse(body).ok()?;
    let root = document.root_element();
    if !is_supported_root(root.tag_name().name()) {
        return None;
    }

    let (rating, vote_count) = parse_rating(root);
    let mut meta = NfoMeta {
        title: child_text(root, &["title", "name", "localtitle"]),
        original_title: child_text(root, &["originaltitle", "original_title"]),
        year: child_text(root, &["year"]),
        plot: child_text(root, &["plot", "overview", "outline"]),
        tagline: child_text(root, &["tagline"]),
        rating,
        vote_count,
        runtime_minutes: child_text(root, &["runtime", "runtime_minutes"]),
        premiered: child_text(root, &["premiered", "firstaired", "release_date"]),
        end_date: child_text(root, &["enddate", "ended", "lastaired", "end_date"]),
        content_rating: child_text(root, &["mpaa", "contentrating", "certification"]),
        original_language: child_text(root, &["original_language", "originallanguage"]),
        status: child_text(root, &["status"]),
        season: child_u32(root, &["season"]),
        episode: child_u32(root, &["episode"]),
        aired: child_text(root, &["aired"]),
        number_of_seasons: child_u32(
            root,
            &["seasoncount", "numberofseasons", "number_of_seasons"],
        ),
        number_of_episodes: child_u32(
            root,
            &["episodecount", "numberofepisodes", "number_of_episodes"],
        ),
        thumb: child_text(root, &["thumb"]),
        genres: child_texts(root, &["genre", "tag"]),
        countries: child_texts(root, &["country"]),
        studios: child_texts(root, &["studio", "network", "productioncompany"]),
        directors: child_texts(root, &["director"]),
        creators: child_texts(root, &["creator"]),
        ..NfoMeta::default()
    };
    meta.cast = root
        .children()
        .filter(|node| node.is_element() && name_is(node.tag_name().name(), "actor"))
        .filter_map(|actor| {
            let name = child_text(actor, &["name"])?;
            Some(CastMember {
                name,
                role: child_text(actor, &["role"]),
                tmdb_id: child_text(actor, &["tmdbid"]),
                thumb: child_text(actor, &["thumb"]),
                order: child_u32(actor, &["order"]),
            })
        })
        .collect();
    meta.cast
        .sort_by_key(|member| member.order.unwrap_or(u32::MAX));
    Some(meta)
}

fn is_supported_root(name: &str) -> bool {
    [
        "movie",
        "tvshow",
        "tvseries",
        "series",
        "episodedetails",
        "episode",
        "season",
        "video",
        "musicvideo",
    ]
    .iter()
    .any(|candidate| name_is(name, candidate))
}

fn name_is(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn child_nodes<'a>(
    node: roxmltree::Node<'a, 'a>,
    names: &'a [&'a str],
) -> impl Iterator<Item = roxmltree::Node<'a, 'a>> + 'a {
    node.children().filter(move |child| {
        child.is_element()
            && names
                .iter()
                .any(|name| name_is(child.tag_name().name(), name))
    })
}

fn node_text(node: roxmltree::Node<'_, '_>) -> Option<String> {
    node.text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn child_text(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Option<String> {
    child_nodes(node, names).find_map(node_text)
}

fn child_texts(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Vec<String> {
    child_nodes(node, names).filter_map(node_text).collect()
}

fn child_u32(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Option<u32> {
    child_text(node, names)?.parse().ok()
}

fn parse_rating(root: roxmltree::Node<'_, '_>) -> (Option<String>, Option<u64>) {
    let ratings = root
        .children()
        .find(|child| child.is_element() && name_is(child.tag_name().name(), "ratings"));
    let entries: Vec<_> = ratings
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| node.is_element() && name_is(node.tag_name().name(), "rating"))
        .collect();
    let selected = entries
        .iter()
        .copied()
        .find(|entry| {
            entry.attribute("name").is_some_and(|name| {
                matches!(name.to_ascii_lowercase().as_str(), "themoviedb" | "tmdb")
            })
        })
        .or_else(|| {
            entries.iter().copied().find(|entry| {
                entry
                    .attribute("default")
                    .is_some_and(|v| v.eq_ignore_ascii_case("true"))
            })
        })
        .or_else(|| entries.first().copied());

    if let Some(entry) = selected {
        let value = child_text(entry, &["value"]).or_else(|| node_text(entry));
        let count = child_text(entry, &["votes"]).or_else(|| child_text(root, &["votes"]));
        return (value, parse_vote_count(count.as_deref()));
    }
    let rating = child_text(root, &["rating"]);
    let count = child_text(root, &["votes"]);
    (rating, parse_vote_count(count.as_deref()))
}

fn parse_vote_count(value: Option<&str>) -> Option<u64> {
    value?
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}

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
        &body,
        root,
        media,
        meta,
        is_episode,
        &existing,
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

fn queue_nfo_edits(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    media: &Media,
    meta: Option<&NfoMeta>,
    is_episode: bool,
    existing: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_nfo_identity(body, root, media, meta, is_episode, edits, insertions);
    if let Some(metadata) = meta {
        queue_nfo_metadata(body, root, metadata, existing, edits, insertions);
    }
}

fn queue_nfo_identity(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    media: &Media,
    meta: Option<&NfoMeta>,
    is_episode: bool,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    let title = meta
        .and_then(|item| item.title.as_deref())
        .filter(|value| !value.is_empty());
    queue_scalar(
        body,
        root,
        &["title", "name", "localtitle"],
        "title",
        title.unwrap_or(&media.title),
        edits,
        insertions,
    );
    let original_title = meta
        .and_then(|item| item.original_title.as_deref())
        .filter(|value| !value.is_empty())
        .or(media.original_title.as_deref())
        .filter(|value| !value.is_empty());
    queue_optional_scalar(
        body,
        root,
        &["originaltitle", "original_title"],
        "originaltitle",
        original_title,
        edits,
        insertions,
    );
    if !is_episode {
        if let Some(year) = media.year {
            queue_scalar(
                body,
                root,
                &["year"],
                "year",
                &year.to_string(),
                edits,
                insertions,
            );
        }
    }
}

fn queue_nfo_metadata(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    metadata: &NfoMeta,
    existing: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_metadata_scalars(body, root, metadata, edits, insertions);
    queue_metadata_numbers(body, root, metadata, edits, insertions);
    queue_metadata_lists(body, root, metadata, edits, insertions);
    queue_cast(body, root, &metadata.cast, edits, insertions);
    if metadata.rating.is_some() || metadata.vote_count.is_some() {
        queue_ratings(
            body,
            root,
            metadata.rating.as_deref().or(existing.rating.as_deref()),
            metadata.vote_count.or(existing.vote_count),
            edits,
            insertions,
        );
    }
}

macro_rules! queue_fields {
    ($body:ident, $root:ident, $edits:ident, $insertions:ident; $queue:ident; $($aliases:expr => $output:literal => $value:expr),* $(,)?) => {
        $( $queue($body, $root, $aliases, $output, $value, $edits, $insertions); )*
    };
}

fn queue_metadata_scalars(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_optional_scalar;
        &["plot", "overview", "outline"] => "plot" => meta.plot.as_deref(),
        &["tagline"] => "tagline" => meta.tagline.as_deref(),
        &["runtime", "runtime_minutes"] => "runtime" => meta.runtime_minutes.as_deref(),
        &["mpaa", "contentrating", "certification"] => "mpaa" => meta.content_rating.as_deref(),
        &["premiered", "firstaired", "release_date"] => "premiered" => meta.premiered.as_deref(),
        &["enddate", "ended", "lastaired", "end_date"] => "enddate" => meta.end_date.as_deref(),
        &["original_language", "originallanguage"] => "original_language" => meta.original_language.as_deref(),
        &["status"] => "status" => meta.status.as_deref(),
        &["thumb"] => "thumb" => meta.thumb.as_deref(),
        &["aired"] => "aired" => meta.aired.as_deref(),
    );
}

fn queue_metadata_numbers(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_optional_number;
        &["season"] => "season" => meta.season,
        &["episode"] => "episode" => meta.episode,
        &["seasoncount", "numberofseasons", "number_of_seasons"] => "seasoncount" => meta.number_of_seasons,
        &["episodecount", "numberofepisodes", "number_of_episodes"] => "episodecount" => meta.number_of_episodes,
    );
}

fn queue_metadata_lists(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_list;
        &["genre", "tag"] => "genre" => &meta.genres,
        &["country"] => "country" => &meta.countries,
        &["studio", "network", "productioncompany"] => "studio" => &meta.studios,
        &["director"] => "director" => &meta.directors,
        &["creator"] => "creator" => &meta.creators,
    );
}

#[derive(Debug)]
struct XmlEdit {
    range: std::ops::Range<usize>,
    replacement: String,
}

fn queue_optional_scalar(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    aliases: &[&str],
    output: &str,
    value: Option<&str>,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        queue_scalar(body, root, aliases, output, value, edits, insertions);
    }
}

fn queue_optional_number(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    aliases: &[&str],
    output: &str,
    value: Option<u32>,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    if let Some(value) = value {
        queue_scalar(
            body,
            root,
            aliases,
            output,
            &value.to_string(),
            edits,
            insertions,
        );
    }
}

fn queue_scalar(
    _body: &str,
    root: roxmltree::Node<'_, '_>,
    aliases: &[&str],
    output: &str,
    value: &str,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_remove(root, aliases, edits);
    insertions.push_str(&element_line(output, value));
}

fn queue_list(
    _body: &str,
    root: roxmltree::Node<'_, '_>,
    aliases: &[&str],
    output: &str,
    values: &[String],
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    if values.is_empty() {
        return;
    }
    queue_remove(root, aliases, edits);
    for value in values {
        insertions.push_str(&element_line(output, value));
    }
}

fn queue_cast(
    _body: &str,
    root: roxmltree::Node<'_, '_>,
    cast: &[CastMember],
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    if cast.is_empty() {
        return;
    }
    queue_remove(root, &["actor"], edits);
    for member in cast {
        insertions.push_str("  <actor>\n");
        insertions.push_str(&element_line("name", &member.name).replace("  ", "    "));
        if let Some(role) = member.role.as_deref() {
            insertions.push_str(&element_line("role", role).replace("  ", "    "));
        }
        if let Some(tmdb_id) = member.tmdb_id.as_deref() {
            insertions.push_str(&element_line("tmdbid", tmdb_id).replace("  ", "    "));
        }
        if let Some(thumb) = member.thumb.as_deref() {
            insertions.push_str(&element_line("thumb", thumb).replace("  ", "    "));
        }
        if let Some(order) = member.order {
            insertions.push_str(&format!("    <order>{order}</order>\n"));
        }
        insertions.push_str("  </actor>\n");
    }
}

fn queue_ratings(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    rating: Option<&str>,
    vote_count: Option<u64>,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_remove(root, &["rating", "ratings", "votes"], edits);
    let mut preserved = Vec::new();
    if let Some(ratings) = root
        .children()
        .find(|child| child.is_element() && name_is(child.tag_name().name(), "ratings"))
    {
        for child in ratings.children() {
            if child.is_element()
                && name_is(child.tag_name().name(), "rating")
                && child.attribute("name").is_some_and(|name| {
                    matches!(name.to_ascii_lowercase().as_str(), "themoviedb" | "tmdb")
                })
            {
                continue;
            }
            if child.is_element() || child.is_comment() || child.is_pi() {
                if let Some(raw) = body.get(child.range()) {
                    preserved.push(raw.to_string());
                }
            }
        }
    }
    let mut block = String::from("  <ratings>\n");
    for other in preserved {
        block.push_str("    ");
        block.push_str(&other.replace('\n', "\n    "));
        block.push('\n');
    }
    block.push_str("    <rating name=\"themoviedb\" max=\"10\" default=\"true\">\n");
    if let Some(rating) = rating {
        block.push_str(&format!("      <value>{}</value>\n", xml_escape(rating)));
    }
    if let Some(vote_count) = vote_count {
        block.push_str(&format!("      <votes>{vote_count}</votes>\n"));
    }
    block.push_str("    </rating>\n  </ratings>\n");
    insertions.push_str(&block);
}

fn queue_remove(root: roxmltree::Node<'_, '_>, aliases: &[&str], edits: &mut Vec<XmlEdit>) {
    for child in root.children().filter(|child| child.is_element()) {
        if aliases
            .iter()
            .any(|name| name_is(child.tag_name().name(), name))
        {
            edits.push(XmlEdit {
                range: child.range(),
                replacement: String::new(),
            });
        }
    }
}

fn element_line(name: &str, value: &str) -> String {
    format!("  <{name}>{}</{name}>\n", xml_escape(value))
}

fn apply_edits(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    mut edits: Vec<XmlEdit>,
    insertions: &str,
) -> std::io::Result<String> {
    if !insertions.is_empty() {
        let close_start = root_close_start(body, root).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "NFO root closing tag is missing",
            )
        })?;
        let mut insertion = String::new();
        if close_start > 0 && !body[..close_start].ends_with('\n') {
            insertion.push('\n');
        }
        insertion.push_str(insertions);
        edits.push(XmlEdit {
            range: close_start..close_start,
            replacement: insertion,
        });
    }
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    let mut output = String::with_capacity(body.len() + insertions.len());
    let mut cursor = 0;
    for edit in edits {
        if edit.range.start < cursor {
            continue;
        }
        output.push_str(&body[cursor..edit.range.start]);
        output.push_str(&edit.replacement);
        cursor = edit.range.end;
    }
    output.push_str(&body[cursor..]);
    Ok(output)
}

fn root_close_start(body: &str, root: roxmltree::Node<'_, '_>) -> Option<usize> {
    let end = root.range().end.min(body.len());
    let name = root.tag_name().name();
    let marker = format!("</{name}");
    body[..end]
        .match_indices(&marker)
        .last()
        .map(|(index, _)| index)
        .or_else(|| {
            let lower = body[..end].to_ascii_lowercase();
            lower
                .match_indices(&marker.to_ascii_lowercase())
                .last()
                .map(|(index, _)| index)
        })
}

/// 把 ffprobe 探测到的媒体流信息（<fileinfo><streamdetails>）合并写入已有 NFO。
///
/// 探测是懒加载的，所以这里采用「探测完成后再补写」的语义：
/// - 目标 NFO 不存在 → 跳过（没有可以补的宿主文件）；
/// - 已含 `<fileinfo>`（例如 MoviePilot / TMM 早已生成）→ 跳过，绝不覆盖别人的成果；
/// - 否则在根闭合标签（`</episodedetails>` / `</movie>` / `</tvshow>` / `</season>`）前插入。
pub fn write_streamdetails_into_nfo(
    nfo_path: &Path,
    video: Option<&crate::VideoTrack>,
    audio: &[crate::AudioTrack],
    subtitles: &[crate::SubtitleTrack],
) -> std::io::Result<()> {
    let body = match std::fs::read_to_string(nfo_path) {
        Ok(body) => body,
        Err(_) => return Ok(()),
    };
    let Ok(document) = roxmltree::Document::parse(&body) else {
        return Ok(());
    };
    let root = document.root_element();
    if !is_supported_root(root.tag_name().name())
        || root
            .children()
            .any(|child| child.is_element() && name_is(child.tag_name().name(), "fileinfo"))
    {
        return Ok(());
    }
    let Some(idx) = root_close_start(&body, root) else {
        return Ok(());
    };
    let block = streamdetails_xml(video, audio, subtitles);
    let mut insertion = String::new();
    if idx > 0 && !body[..idx].ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push_str(&block);
    let mut updated = String::with_capacity(body.len() + insertion.len());
    updated.push_str(&body[..idx]);
    updated.push_str(&insertion);
    updated.push_str(&body[idx..]);
    std::fs::write(nfo_path, updated)
}

fn streamdetails_xml(
    video: Option<&crate::VideoTrack>,
    audio: &[crate::AudioTrack],
    subtitles: &[crate::SubtitleTrack],
) -> String {
    let mut block = String::from("<fileinfo>\n  <streamdetails>\n");
    if let Some(v) = video {
        append_video_stream(&mut block, v);
    }
    for t in audio {
        append_audio_stream(&mut block, t);
    }
    for t in subtitles {
        append_subtitle_stream(&mut block, t);
    }
    block.push_str("  </streamdetails>\n</fileinfo>\n");
    block
}

fn append_video_stream(block: &mut String, track: &crate::VideoTrack) {
    block.push_str("    <video>\n");
    if let Some(codec) = &track.codec {
        block.push_str(&stream_field("codec", codec));
    }
    if let Some(value) = track.bit_rate {
        block.push_str(&stream_field("bitrate", &value.to_string()));
    }
    if let Some(value) = track.width {
        block.push_str(&stream_field("width", &value.to_string()));
    }
    if let Some(value) = track.height {
        block.push_str(&stream_field("height", &value.to_string()));
    }
    if let (Some(width), Some(height)) = (track.width, track.height) {
        if height > 0 {
            block.push_str(&stream_field(
                "aspect",
                &format!("{:.2}:1", width as f64 / height as f64),
            ));
        }
    }
    if let Some(value) = track.frame_rate {
        block.push_str(&stream_field("framerate", &format!("{value:.3}")));
    }
    if let Some(value) = track.duration_secs {
        block.push_str(&stream_field("duration", &format!("{value:.0}")));
    }
    block.push_str("    </video>\n");
}

fn append_audio_stream(block: &mut String, track: &crate::AudioTrack) {
    block.push_str("    <audio>\n");
    append_optional_stream_field(block, "codec", track.codec.as_deref());
    if let Some(channels) = track.channels {
        block.push_str(&stream_field("channels", &channels.to_string()));
    }
    append_optional_stream_field(block, "language", track.language.as_deref());
    append_optional_stream_field(block, "samplingrate", track.sample_rate.as_deref());
    block.push_str(&stream_field("default", bool_text(track.is_default)));
    block.push_str("    </audio>\n");
}

fn append_subtitle_stream(block: &mut String, track: &crate::SubtitleTrack) {
    block.push_str("    <subtitle>\n");
    append_optional_stream_field(block, "codec", track.codec.as_deref());
    append_optional_stream_field(block, "language", track.language.as_deref());
    block.push_str(&stream_field("default", bool_text(track.is_default)));
    block.push_str(&stream_field("forced", bool_text(track.forced)));
    block.push_str("    </subtitle>\n");
}

fn append_optional_stream_field(block: &mut String, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        block.push_str(&stream_field(name, value));
    }
}

fn stream_field(name: &str, value: &str) -> String {
    format!("      <{name}>{}</{name}>\n", xml_escape(value))
}

fn bool_text(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}
