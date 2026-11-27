#[path = "browser_html_foreign.rs"]
mod foreign;
#[path = "browser_html_rawtext.rs"]
mod rawtext;
#[path = "browser_html_xml.rs"]
mod xml;

use foreign::{NsStack, is_table_scope_end_tag};
use rawtext::{find_rawtext_close, find_rawtext_close_opt, scan_script_close};
use xml::attr_is_password_type;
pub(super) use xml::decode_nav_body_for_sanitize;

#[derive(Default)]
struct UnclosedQuoteFlags {
    dquote: bool,
    squote: bool,
}

enum TagScan {
    NotATag,
    Aborted {
        tag_name_end: usize,
        end: usize,
    },
    Closed {
        tag_name_end: usize,
        end: usize,
        self_closing: bool,
        first_unquoted_lt: Option<usize>,
    },
}

#[derive(Clone, Copy)]
enum CommentState {
    Start,
    StartDash,
    Body,
    EndDash,
    End,
    EndBang,
}

#[derive(Clone, Copy)]
enum AttrState {
    BeforeAttrName,
    AttrName,
    AfterAttrName,
    BeforeAttrValue,
    AttrValueDoubleQuoted,
    AttrValueSingleQuoted,
    AfterAttrValueQuoted,
    AttrValueUnquoted,
    SelfClosingStartTag,
}

pub(super) fn sanitize_html(raw: &str) -> String {
    sanitize_html_with_mode(raw, None)
}

pub(super) fn sanitize_html_with_mode(raw: &str, explicit_xhtml: Option<bool>) -> String {
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    let mut pos = 0;
    let mut unclosed = UnclosedQuoteFlags::default();
    let is_xhtml = explicit_xhtml.unwrap_or_else(|| xml::detect_prefixed_xhtml_root(raw));
    let mut ns = NsStack::new(is_xhtml);

    while pos < bytes.len() {
        let Some(rel_lt) = bytes[pos..].iter().position(|&b| b == b'<') else {
            if !ns.suppressing_script() {
                out.push_str(&raw[pos..]);
            }
            break;
        };
        let lt = pos + rel_lt;
        if !ns.suppressing_script() {
            out.push_str(&raw[pos..lt]);
        }
        match bytes.get(lt + 1) {
            None => {
                if !ns.suppressing_script() {
                    out.push('<');
                }
                break;
            }
            Some(b'!')
                if (ns.in_foreign_namespace() || ns.is_xhtml())
                    && bytes[lt..].starts_with(b"<![CDATA[") =>
            {
                let end = scan_cdata_end(bytes, lt);
                if !ns.suppressing_script() {
                    out.push_str(&raw[lt..end]);
                }
                pos = end;
            }
            Some(b'!') if bytes[lt..].starts_with(b"<!--") => {
                let end = if ns.is_xhtml() {
                    xml::scan_xml_comment_end(bytes, lt)
                } else {
                    scan_html_comment_end(bytes, lt)
                };
                if !ns.suppressing_script() {
                    out.push_str(&raw[lt..end]);
                }
                pos = end;
            }
            Some(b'?') if ns.is_xhtml() => {
                let end = xml::scan_xml_pi_end(bytes, lt);
                if !ns.suppressing_script() {
                    out.push_str(&raw[lt..end]);
                }
                pos = end;
            }
            Some(b'!') if ns.is_xhtml() => {
                let end = xml::scan_xml_decl_end(bytes, lt);
                if !ns.suppressing_script() {
                    out.push_str(&raw[lt..end]);
                }
                pos = end;
            }
            Some(b'/') if bytes.get(lt + 2).is_some_and(u8::is_ascii_alphabetic) => {
                pos = handle_end_tag(raw, bytes, lt, &mut unclosed, &mut ns, &mut out);
            }
            Some(b'/' | b'!' | b'?') => {
                if ns.suppressing_script() {
                    let mut ignored = String::new();
                    pos = handle_bogus_comment(raw, bytes, lt, &mut ignored);
                } else {
                    pos = handle_bogus_comment(raw, bytes, lt, &mut out);
                }
            }
            Some(&b) if b.is_ascii_alphabetic() => {
                pos = handle_start_tag(
                    raw,
                    bytes,
                    lt,
                    explicit_xhtml,
                    &mut unclosed,
                    &mut ns,
                    &mut out,
                );
            }
            Some(_) => {
                if !ns.suppressing_script() {
                    out.push('<');
                }
                pos = lt + 1;
            }
        }
    }
    out
}

fn scan_cdata_end(bytes: &[u8], lt: usize) -> usize {
    let body_start = lt + 9;
    bytes[body_start..]
        .windows(3)
        .position(|w| w == b"]]>")
        .map_or(bytes.len(), |rel| body_start + rel + 3)
}

fn handle_bogus_comment(raw: &str, bytes: &[u8], lt: usize, out: &mut String) -> usize {
    let body_start = lt + 2;
    if let Some(rel_gt) = bytes[body_start..].iter().position(|&b| b == b'>') {
        let close_gt = body_start + rel_gt;
        let inner = &raw[body_start..close_gt];
        if inner.contains('<') {
            out.push_str(&raw[lt..body_start]);
            out.push_str(&sanitize_html(inner));
            out.push('>');
        } else {
            out.push_str(&raw[lt..=close_gt]);
        }
        close_gt + 1
    } else {
        let inner = &raw[body_start..];
        if inner.contains('<') {
            out.push_str(&raw[lt..body_start]);
            out.push_str(&sanitize_html(inner));
        } else {
            out.push_str(&raw[lt..]);
        }
        bytes.len()
    }
}

fn handle_end_tag(
    raw: &str,
    bytes: &[u8],
    lt: usize,
    unclosed: &mut UnclosedQuoteFlags,
    ns: &mut NsStack,
    out: &mut String,
) -> usize {
    match scan_end_tag(bytes, lt, ns.is_xhtml(), unclosed) {
        TagScan::NotATag => {
            if !ns.suppressing_script() {
                out.push('<');
            }
            lt + 1
        }
        TagScan::Aborted { tag_name_end, end } => {
            if !ns.suppressing_script() {
                out.push_str(&raw[lt..tag_name_end]);
            }
            end
        }
        TagScan::Closed {
            tag_name_end, end, ..
        } => {
            let tag_name = &bytes[lt + 2..tag_name_end];
            let was_suppressing = ns.suppressing_script();
            let is_script_end =
                xml::local_name(tag_name, ns.is_xhtml()).eq_ignore_ascii_case(b"script");
            ns.on_end_tag(tag_name);
            if !ns.suppressing_script() && !(was_suppressing && is_script_end) {
                if end == tag_name_end + 1 {
                    out.push_str(&raw[lt..end]);
                } else {
                    out.push_str(&raw[lt..tag_name_end]);
                    out.push('>');
                }
            }
            end
        }
    }
}

fn is_select_allowed_start_tag(tag_name: &[u8]) -> bool {
    tag_name.eq_ignore_ascii_case(b"script")
        || tag_name.eq_ignore_ascii_case(b"template")
        || tag_name.eq_ignore_ascii_case(b"option")
        || tag_name.eq_ignore_ascii_case(b"optgroup")
        || tag_name.eq_ignore_ascii_case(b"html")
        || tag_name.eq_ignore_ascii_case(b"hr")
}

fn emit_plain_start_tag(
    raw: &str,
    lt: usize,
    end: usize,
    first_unquoted_lt: Option<usize>,
    explicit_xhtml: Option<bool>,
    out: &mut String,
) {
    if let Some(inner_lt) = first_unquoted_lt {
        out.push_str(&raw[lt..inner_lt]);
        out.push_str(&sanitize_html_with_mode(
            &raw[inner_lt..end],
            explicit_xhtml,
        ));
    } else {
        out.push_str(&raw[lt..end]);
    }
}

fn is_html_rawtext_or_rcdata(tag_name: &[u8]) -> bool {
    tag_name.eq_ignore_ascii_case(b"style")
        || tag_name.eq_ignore_ascii_case(b"title")
        || tag_name.eq_ignore_ascii_case(b"textarea")
        || tag_name.eq_ignore_ascii_case(b"iframe")
        || tag_name.eq_ignore_ascii_case(b"xmp")
        || tag_name.eq_ignore_ascii_case(b"noembed")
        || tag_name.eq_ignore_ascii_case(b"noframes")
}

fn handle_start_tag(
    raw: &str,
    bytes: &[u8],
    lt: usize,
    explicit_xhtml: Option<bool>,
    unclosed: &mut UnclosedQuoteFlags,
    ns: &mut NsStack,
    out: &mut String,
) -> usize {
    match scan_start_tag(bytes, lt, ns.is_xhtml(), unclosed) {
        TagScan::NotATag => {
            if !ns.suppressing_script() {
                out.push('<');
            }
            lt + 1
        }
        TagScan::Aborted { tag_name_end, end } => {
            if !ns.suppressing_script() {
                let tag_name = &bytes[lt + 1..tag_name_end];
                if xml::local_name(tag_name, ns.is_xhtml()).eq_ignore_ascii_case(b"input") {
                    out.push_str(&redact_input_tag(&raw[lt..end], tag_name_end - lt));
                } else {
                    out.push_str(&raw[lt..end]);
                }
            }
            end
        }
        TagScan::Closed {
            tag_name_end,
            end,
            self_closing,
            first_unquoted_lt,
        } => {
            let tag_name = &bytes[lt + 1..tag_name_end];
            let attrs_raw = &raw[tag_name_end..end];
            if !ns.is_xhtml() && ns.in_select_scope() {
                if ns.in_select_in_table() && is_table_scope_end_tag(tag_name) {
                    ns.pop_select_in_scope();
                } else if tag_name.eq_ignore_ascii_case(b"select") {
                    ns.pop_select_in_scope();
                    emit_plain_start_tag(raw, lt, end, first_unquoted_lt, explicit_xhtml, out);
                    return end;
                } else if tag_name.eq_ignore_ascii_case(b"input")
                    || tag_name.eq_ignore_ascii_case(b"keygen")
                    || tag_name.eq_ignore_ascii_case(b"textarea")
                {
                    ns.pop_select_in_scope();
                } else if !is_select_allowed_start_tag(tag_name) {
                    emit_plain_start_tag(raw, lt, end, first_unquoted_lt, explicit_xhtml, out);
                    return end;
                }
            }
            let was_foreign = ns.in_foreign_for_start_tag(tag_name);
            let is_xhtml = ns.is_xhtml();
            let next_mode = Some(is_xhtml);
            let local = xml::local_name(tag_name, is_xhtml);
            let is_html_raw = !was_foreign
                && !is_xhtml
                && (is_html_rawtext_or_rcdata(tag_name)
                    || tag_name.eq_ignore_ascii_case(b"noscript"));
            ns.on_start_tag(
                tag_name,
                attrs_raw,
                self_closing,
                is_html_raw || (!was_foreign && !is_xhtml && local.eq_ignore_ascii_case(b"script")),
            );
            if local.eq_ignore_ascii_case(b"script") {
                if !was_foreign && !is_xhtml {
                    scan_script_close(bytes, end)
                } else {
                    end
                }
            } else if ns.suppressing_script() {
                end
            } else if !was_foreign && !is_xhtml && tag_name.eq_ignore_ascii_case(b"noscript") {
                find_rawtext_close(bytes, end, tag_name)
            } else if !was_foreign && !is_xhtml && is_html_rawtext_or_rcdata(tag_name) {
                if self_closing {
                    if let Some((close_lt, close_end)) =
                        find_rawtext_close_opt(bytes, end, tag_name)
                    {
                        out.push_str(&raw[lt..end]);
                        out.push_str(&sanitize_html_with_mode(&raw[end..close_lt], next_mode));
                        out.push_str(&raw[close_lt..close_end]);
                        close_end
                    } else {
                        out.push_str(&raw[lt..end]);
                        end
                    }
                } else {
                    let close_end = find_rawtext_close(bytes, end, tag_name);
                    out.push_str(&raw[lt..close_end]);
                    close_end
                }
            } else if local.eq_ignore_ascii_case(b"input") {
                out.push_str(&redact_input_tag(&raw[lt..end], tag_name_end - lt));
                end
            } else {
                emit_plain_start_tag(raw, lt, end, first_unquoted_lt, next_mode, out);
                end
            }
        }
    }
}

fn scan_html_comment_end(bytes: &[u8], lt: usize) -> usize {
    let mut state = CommentState::Start;
    let mut i = lt + 4;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            CommentState::Start => match b {
                b'-' => {
                    state = CommentState::StartDash;
                    i += 1;
                }
                b'>' => return i + 1,
                _ => state = CommentState::Body,
            },
            CommentState::StartDash => match b {
                b'-' => {
                    state = CommentState::End;
                    i += 1;
                }
                b'>' => return i + 1,
                _ => state = CommentState::Body,
            },
            CommentState::Body => {
                if b == b'-' {
                    state = CommentState::EndDash;
                }
                i += 1;
            }
            CommentState::EndDash => match b {
                b'-' => {
                    state = CommentState::End;
                    i += 1;
                }
                _ => state = CommentState::Body,
            },
            CommentState::End => match b {
                b'>' => return i + 1,
                b'!' => {
                    state = CommentState::EndBang;
                    i += 1;
                }
                b'-' => i += 1,
                _ => state = CommentState::Body,
            },
            CommentState::EndBang => match b {
                b'-' => {
                    state = CommentState::EndDash;
                    i += 1;
                }
                b'>' => return i + 1,
                _ => state = CommentState::Body,
            },
        }
    }
    bytes.len()
}

fn scan_start_tag(
    bytes: &[u8],
    lt: usize,
    is_xhtml: bool,
    unclosed: &mut UnclosedQuoteFlags,
) -> TagScan {
    scan_named_tag(bytes, lt + 1, true, is_xhtml, unclosed)
}

fn scan_end_tag(
    bytes: &[u8],
    lt: usize,
    is_xhtml: bool,
    unclosed: &mut UnclosedQuoteFlags,
) -> TagScan {
    scan_named_tag(bytes, lt + 2, false, is_xhtml, unclosed)
}

fn scan_named_tag(
    bytes: &[u8],
    name_start: usize,
    allow_input_special: bool,
    is_xhtml: bool,
    unclosed: &mut UnclosedQuoteFlags,
) -> TagScan {
    let mut i = name_start + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() || b == b'/' {
            let is_input = allow_input_special
                && xml::local_name(&bytes[name_start..i], is_xhtml).eq_ignore_ascii_case(b"input");
            return scan_tag_attrs(bytes, i, i, is_input, unclosed);
        }
        if b == b'>' {
            return TagScan::Closed {
                tag_name_end: i,
                end: i + 1,
                self_closing: false,
                first_unquoted_lt: None,
            };
        }
        if b == b'<' {
            return TagScan::Aborted {
                tag_name_end: i,
                end: i,
            };
        }
        if b == 0 {
            return TagScan::NotATag;
        }
        i += 1;
    }
    TagScan::Aborted {
        tag_name_end: bytes.len(),
        end: bytes.len(),
    }
}

fn scan_tag_attrs(
    bytes: &[u8],
    tag_name_end: usize,
    start: usize,
    is_input: bool,
    unclosed: &mut UnclosedQuoteFlags,
) -> TagScan {
    let mut state = AttrState::BeforeAttrName;
    let mut i = start;
    let mut first_unquoted_lt: Option<usize> = None;

    while i < bytes.len() {
        let b = bytes[i];
        match state {
            AttrState::AttrValueDoubleQuoted => {
                if b == b'"' {
                    state = AttrState::AfterAttrValueQuoted;
                }
                i += 1;
            }
            AttrState::AttrValueSingleQuoted => {
                if b == b'\'' {
                    state = AttrState::AfterAttrValueQuoted;
                }
                i += 1;
            }
            AttrState::SelfClosingStartTag if b == b'>' => {
                return TagScan::Closed {
                    tag_name_end,
                    end: i + 1,
                    self_closing: true,
                    first_unquoted_lt,
                };
            }
            _ if b == b'>' => {
                return TagScan::Closed {
                    tag_name_end,
                    end: i + 1,
                    self_closing: false,
                    first_unquoted_lt,
                };
            }
            AttrState::SelfClosingStartTag => {
                state = AttrState::BeforeAttrName;
            }
            _ => {
                if b == b'<' && first_unquoted_lt.is_none() {
                    first_unquoted_lt = Some(i);
                }
                let Some(next) = step_attr_state(state, b, is_input, unclosed) else {
                    return TagScan::NotATag;
                };
                if !matches!(state, AttrState::AfterAttrValueQuoted)
                    || b == b'/'
                    || b.is_ascii_whitespace()
                {
                    i += 1;
                }
                state = next;
            }
        }
    }

    match state {
        AttrState::AttrValueDoubleQuoted if !is_input => {
            unclosed.dquote = true;
            TagScan::NotATag
        }
        AttrState::AttrValueSingleQuoted if !is_input => {
            unclosed.squote = true;
            TagScan::NotATag
        }
        _ => TagScan::Aborted {
            tag_name_end,
            end: bytes.len(),
        },
    }
}

fn step_attr_state(
    state: AttrState,
    b: u8,
    is_input: bool,
    unclosed: &mut UnclosedQuoteFlags,
) -> Option<AttrState> {
    match state {
        AttrState::BeforeAttrName => Some(if b == b'/' {
            AttrState::SelfClosingStartTag
        } else if !b.is_ascii_whitespace() {
            AttrState::AttrName
        } else {
            AttrState::BeforeAttrName
        }),
        AttrState::AttrName => Some(if b.is_ascii_whitespace() {
            AttrState::AfterAttrName
        } else if b == b'/' {
            AttrState::SelfClosingStartTag
        } else if b == b'=' {
            AttrState::BeforeAttrValue
        } else {
            AttrState::AttrName
        }),
        AttrState::AfterAttrName => Some(if b == b'/' {
            AttrState::SelfClosingStartTag
        } else if b == b'=' {
            AttrState::BeforeAttrValue
        } else if !b.is_ascii_whitespace() {
            AttrState::AttrName
        } else {
            AttrState::AfterAttrName
        }),
        AttrState::BeforeAttrValue => {
            if b.is_ascii_whitespace() {
                return Some(AttrState::BeforeAttrValue);
            }
            if b == b'"' {
                return (!(!is_input && unclosed.dquote))
                    .then_some(AttrState::AttrValueDoubleQuoted);
            }
            if b == b'\'' {
                return (!(!is_input && unclosed.squote))
                    .then_some(AttrState::AttrValueSingleQuoted);
            }
            Some(AttrState::AttrValueUnquoted)
        }
        AttrState::AfterAttrValueQuoted => Some(if b == b'/' {
            AttrState::SelfClosingStartTag
        } else {
            AttrState::BeforeAttrName
        }),
        AttrState::AttrValueUnquoted => Some(if b.is_ascii_whitespace() {
            AttrState::BeforeAttrName
        } else {
            AttrState::AttrValueUnquoted
        }),
        _ => Some(state),
    }
}

pub(super) struct HtmlAttr {
    pub(super) name_start: usize,
    pub(super) name_end: usize,
    pub(super) val_start: usize,
    pub(super) val_end: usize,
    replace_start: usize,
    replace_end: usize,
    pub(super) has_eq: bool,
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn parse_attr_value(bytes: &[u8], val_pos: usize) -> (usize, usize, usize, usize, usize) {
    if val_pos >= bytes.len() || bytes[val_pos] == b'>' {
        return (val_pos, val_pos, val_pos, val_pos, val_pos);
    }
    let b = bytes[val_pos];
    if b == b'"' || b == b'\'' {
        let mut end = val_pos + 1;
        while end < bytes.len() && bytes[end] != b {
            end += 1;
        }
        let next = if end < bytes.len() { end + 1 } else { end };
        return (val_pos + 1, end, val_pos, next, next);
    }
    let mut end = val_pos;
    while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b'>' {
        end += 1;
    }
    if end > val_pos && bytes[end - 1] == b'/' && end < bytes.len() && bytes[end] == b'>' {
        end -= 1;
    }
    (val_pos, end, val_pos, end, end)
}

pub(super) fn parse_html_attrs(tag: &[u8], attr_start: usize) -> Vec<HtmlAttr> {
    let mut attrs = Vec::new();
    let mut i = attr_start;
    while i < tag.len() {
        while i < tag.len() && (tag[i].is_ascii_whitespace() || tag[i] == b'/') {
            i += 1;
        }
        if i >= tag.len() || tag[i] == b'>' {
            break;
        }
        let name_start = i;
        if tag[i] == b'=' {
            i += 1;
        }
        while i < tag.len()
            && !tag[i].is_ascii_whitespace()
            && !matches!(tag[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let name_end = i;
        if name_end == name_start {
            i += 1;
            continue;
        }
        let after_name = skip_ws(tag, i);
        if after_name < tag.len() && tag[after_name] == b'=' {
            let val_pos = skip_ws(tag, after_name + 1);
            let (vs, ve, rs, re, next) = parse_attr_value(tag, val_pos);
            attrs.push(HtmlAttr {
                name_start,
                name_end,
                val_start: vs,
                val_end: ve,
                replace_start: rs,
                replace_end: re,
                has_eq: true,
            });
            i = next;
        } else {
            attrs.push(HtmlAttr {
                name_start,
                name_end,
                val_start: name_end,
                val_end: name_end,
                replace_start: name_end,
                replace_end: name_end,
                has_eq: false,
            });
            i = after_name;
        }
    }
    attrs
}

fn redact_input_tag(tag: &str, attr_start: usize) -> String {
    let bytes = tag.as_bytes();
    let attrs = parse_html_attrs(bytes, attr_start);
    let is_secret = attrs.iter().any(|a| {
        let name = &tag[a.name_start..a.name_end];
        name.eq_ignore_ascii_case("data-cm-secret")
            || (a.has_eq
                && name.eq_ignore_ascii_case("type")
                && attr_is_password_type(&tag[a.val_start..a.val_end]))
    });
    if !is_secret {
        return tag.to_string();
    }
    let mut out = String::with_capacity(tag.len());
    let mut cursor = 0;
    for a in &attrs {
        if a.has_eq && tag[a.name_start..a.name_end].eq_ignore_ascii_case("value") {
            out.push_str(&tag[cursor..a.replace_start]);
            out.push_str("\"\"");
            cursor = a.replace_end;
        }
    }
    out.push_str(&tag[cursor..]);
    out
}

#[cfg(test)]
#[path = "browser_html_tests.rs"]
mod tests;
