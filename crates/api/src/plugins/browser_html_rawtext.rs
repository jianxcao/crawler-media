#[derive(Clone, Copy)]
enum EndAttrState {
    BeforeName,
    Name,
    AfterName,
    BeforeValue,
    DoubleQuoted,
    SingleQuoted,
    Unquoted,
}

pub(super) fn scan_close_tag_after_name(bytes: &[u8], after: usize) -> Option<usize> {
    let mut state = EndAttrState::BeforeName;
    let mut i = after;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            EndAttrState::DoubleQuoted => {
                if b == b'"' {
                    state = EndAttrState::BeforeName;
                }
            }
            EndAttrState::SingleQuoted => {
                if b == b'\'' {
                    state = EndAttrState::BeforeName;
                }
            }
            _ if b == b'>' => return Some(i + 1),
            EndAttrState::BeforeName => {
                if !b.is_ascii_whitespace() && b != b'/' {
                    state = EndAttrState::Name;
                }
            }
            EndAttrState::Name => {
                if b == b'=' {
                    state = EndAttrState::BeforeValue;
                } else if b.is_ascii_whitespace() {
                    state = EndAttrState::AfterName;
                } else if b == b'/' {
                    state = EndAttrState::BeforeName;
                }
            }
            EndAttrState::AfterName => {
                if b == b'=' {
                    state = EndAttrState::BeforeValue;
                } else if !b.is_ascii_whitespace() && b != b'/' {
                    state = EndAttrState::Name;
                }
            }
            EndAttrState::BeforeValue => {
                if b == b'"' {
                    state = EndAttrState::DoubleQuoted;
                } else if b == b'\'' {
                    state = EndAttrState::SingleQuoted;
                } else if !b.is_ascii_whitespace() {
                    state = EndAttrState::Unquoted;
                }
            }
            EndAttrState::Unquoted => {
                if b.is_ascii_whitespace() {
                    state = EndAttrState::BeforeName;
                }
            }
        }
        i += 1;
    }
    None
}

pub(super) fn find_rawtext_close_opt(
    bytes: &[u8],
    start: usize,
    tag_name: &[u8],
) -> Option<(usize, usize)> {
    let mut i = start;
    while i + 2 + tag_name.len() <= bytes.len() {
        if bytes[i] == b'<'
            && bytes[i + 1] == b'/'
            && bytes[i + 2..i + 2 + tag_name.len()].eq_ignore_ascii_case(tag_name)
        {
            let after = i + 2 + tag_name.len();
            if bytes
                .get(after)
                .is_some_and(|&b| b.is_ascii_whitespace() || b == b'/' || b == b'>')
            {
                let close_end = scan_close_tag_after_name(bytes, after).unwrap_or(bytes.len());
                return Some((i, close_end));
            }
        }
        i += 1;
    }
    None
}

pub(super) fn find_rawtext_close(bytes: &[u8], start: usize, tag_name: &[u8]) -> usize {
    find_rawtext_close_opt(bytes, start, tag_name).map_or(bytes.len(), |(_, end)| end)
}

#[derive(Clone, Copy)]
enum ScriptState {
    Data,
    Escaped,
    EscDash,
    EscDashDash,
    DoubleEscaped,
    DoubleEscDash,
    DoubleEscDashDash,
}

fn script_tag_boundary(b: Option<&u8>) -> bool {
    b.is_some_and(|&c| c.is_ascii_whitespace() || c == b'/' || c == b'>')
}

fn matches_script_end(bytes: &[u8], i: usize) -> Option<usize> {
    if i + 8 <= bytes.len()
        && bytes[i] == b'<'
        && bytes[i + 1] == b'/'
        && bytes[i + 2..i + 8].eq_ignore_ascii_case(b"script")
        && script_tag_boundary(bytes.get(i + 8))
    {
        Some(scan_close_tag_after_name(bytes, i + 8).unwrap_or(bytes.len()))
    } else {
        None
    }
}

fn matches_script_start(bytes: &[u8], i: usize) -> bool {
    i + 7 <= bytes.len()
        && bytes[i] == b'<'
        && bytes[i + 1..i + 7].eq_ignore_ascii_case(b"script")
        && script_tag_boundary(bytes.get(i + 7))
}

pub(super) fn scan_script_close(bytes: &[u8], start: usize) -> usize {
    let mut state = ScriptState::Data;
    let mut i = start;
    while i < bytes.len() {
        match state {
            ScriptState::Data => {
                if bytes[i..].starts_with(b"<!--") {
                    state = ScriptState::EscDashDash;
                    i += 4;
                } else if let Some(end) = matches_script_end(bytes, i) {
                    return end;
                } else {
                    i += 1;
                }
            }
            ScriptState::Escaped | ScriptState::EscDash | ScriptState::EscDashDash => {
                if matches!(state, ScriptState::EscDashDash) && bytes[i] == b'>' {
                    state = ScriptState::Data;
                    i += 1;
                } else if let Some(end) = matches_script_end(bytes, i) {
                    return end;
                } else if matches_script_start(bytes, i) {
                    state = ScriptState::DoubleEscaped;
                    i += 7;
                } else if bytes[i] == b'-' {
                    state = match state {
                        ScriptState::Escaped => ScriptState::EscDash,
                        _ => ScriptState::EscDashDash,
                    };
                    i += 1;
                } else {
                    state = ScriptState::Escaped;
                    i += 1;
                }
            }
            ScriptState::DoubleEscaped
            | ScriptState::DoubleEscDash
            | ScriptState::DoubleEscDashDash => {
                if matches!(state, ScriptState::DoubleEscDashDash) && bytes[i] == b'>' {
                    state = ScriptState::Data;
                    i += 1;
                } else if i + 8 <= bytes.len()
                    && bytes[i] == b'<'
                    && bytes[i + 1] == b'/'
                    && bytes[i + 2..i + 8].eq_ignore_ascii_case(b"script")
                    && script_tag_boundary(bytes.get(i + 8))
                {
                    state = ScriptState::Escaped;
                    i += 8;
                } else if bytes[i] == b'-' {
                    state = match state {
                        ScriptState::DoubleEscaped => ScriptState::DoubleEscDash,
                        _ => ScriptState::DoubleEscDashDash,
                    };
                    i += 1;
                } else {
                    state = ScriptState::DoubleEscaped;
                    i += 1;
                }
            }
        }
    }
    bytes.len()
}
