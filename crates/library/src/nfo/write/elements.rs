//! XML 元素级的增删：生成替换片段、写回文本、定位根元素收尾。

use super::super::parse::{CastMember, name_is};
use super::xml_escape;

#[derive(Debug)]
pub(super) struct XmlEdit {
    range: std::ops::Range<usize>,
    replacement: String,
}

pub(super) fn queue_optional_scalar(
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

pub(super) fn queue_optional_number(
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

pub(super) fn queue_scalar(
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

pub(super) fn queue_list(
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

pub(super) fn queue_cast(
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

pub(super) fn queue_ratings(
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
            if (child.is_element() || child.is_comment() || child.is_pi())
                && let Some(raw) = body.get(child.range())
            {
                preserved.push(raw.to_string());
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

pub(super) fn queue_remove(
    root: roxmltree::Node<'_, '_>,
    aliases: &[&str],
    edits: &mut Vec<XmlEdit>,
) {
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

pub(super) fn element_line(name: &str, value: &str) -> String {
    format!("  <{name}>{}</{name}>\n", xml_escape(value))
}

pub(super) fn apply_edits(
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

pub(super) fn root_close_start(body: &str, root: roxmltree::Node<'_, '_>) -> Option<usize> {
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
