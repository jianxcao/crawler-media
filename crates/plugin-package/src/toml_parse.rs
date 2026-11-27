use crate::PackageError;
use crate::toml_scalars::{
    insert_into_inline_map, is_bare_key_byte, is_delim, parse_number_literal, toml_err,
};
use serde_json::{Map, Value};
use std::collections::HashSet;

const MAX_TOML_BYTES: usize = 512 * 1024;
const MAX_DEPTH: usize = 32;

pub(crate) fn parse_toml_to_json(input: &str) -> Result<Value, PackageError> {
    if input.len() > MAX_TOML_BYTES {
        return Err(PackageError::InvalidManifest(
            "plugin.toml exceeds 512 KiB limit".into(),
        ));
    }
    let mut parser = Parser::new(input);
    parser.parse_document()
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
enum ScopedPart {
    Key(String),
    ArrayElem(String, usize),
}

#[derive(Default)]
struct TableTracker {
    defined_tables: HashSet<Vec<ScopedPart>>,
    dotted_tables: HashSet<Vec<ScopedPart>>,
    array_tables: HashSet<Vec<ScopedPart>>,
    inline_tables: HashSet<Vec<ScopedPart>>,
}

struct Parser<'a> {
    bytes: &'a [u8],
    src: &'a str,
    pos: usize,
    root: Map<String, Value>,
    tracker: TableTracker,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            bytes: src.as_bytes(),
            src,
            pos: 0,
            root: Map::new(),
            tracker: TableTracker::default(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> PackageError {
        PackageError::InvalidManifest(format!(
            "TOML parse error at byte {}: {}",
            self.pos,
            msg.into()
        ))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, pat: &str) -> bool {
        self.src[self.pos..].starts_with(pat)
    }

    fn skip_ws_inline(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while let Some(b) = self.peek() {
                if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.peek() == Some(b'#') {
                while let Some(b) = self.peek() {
                    if b == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn expect_line_end(&mut self) -> Result<(), PackageError> {
        self.skip_ws_inline();
        if self.peek() == Some(b'#') {
            while let Some(b) = self.peek() {
                if b == b'\n' {
                    break;
                }
                self.pos += 1;
            }
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.pos += 1;
                Ok(())
            }
            Some(b'\r') => {
                self.pos += 1;
                if self.peek() == Some(b'\n') {
                    self.pos += 1;
                }
                Ok(())
            }
            Some(c) => Err(self.err(format!("unexpected trailing character '{}'", c as char))),
        }
    }

    fn parse_document(&mut self) -> Result<Value, PackageError> {
        let mut current_path: Vec<String> = Vec::new();
        loop {
            self.skip_ws_and_comments();
            let Some(b) = self.peek() else {
                break;
            };
            if b == b'[' {
                let (is_array, path) = self.parse_table_header()?;
                self.expect_line_end()?;
                self.register_header(&path, is_array)?;
                current_path = path;
            } else {
                let key_path = self.parse_dotted_key()?;
                self.skip_ws_inline();
                if self.peek() != Some(b'=') {
                    return Err(self.err("expected '=' after key"));
                }
                self.pos += 1;
                self.skip_ws_inline();
                let val = self.parse_value(0)?;
                self.expect_line_end()?;
                self.insert_kv(&current_path, &key_path, val)?;
            }
        }
        Ok(Value::Object(std::mem::take(&mut self.root)))
    }

    fn parse_table_header(&mut self) -> Result<(bool, Vec<String>), PackageError> {
        let is_array = self.starts_with("[[");
        self.pos += if is_array { 2 } else { 1 };
        let path = self.parse_dotted_key()?;
        self.skip_ws_inline();
        let closing = if is_array { "]]" } else { "]" };
        if !self.starts_with(closing) {
            return Err(self.err(format!("expected '{closing}'")));
        }
        self.pos += closing.len();
        Ok((is_array, path))
    }

    fn parse_dotted_key(&mut self) -> Result<Vec<String>, PackageError> {
        let mut parts = Vec::new();
        loop {
            self.skip_ws_inline();
            let key = self.parse_single_key()?;
            parts.push(key);
            if parts.len() > MAX_DEPTH {
                return Err(self.err("key nesting exceeds maximum depth"));
            }
            self.skip_ws_inline();
            if self.peek() == Some(b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(parts)
    }

    fn parse_single_key(&mut self) -> Result<String, PackageError> {
        match self.peek() {
            Some(b'"') => self.parse_basic_string(),
            Some(b'\'') => self.parse_literal_string(),
            Some(b) if is_bare_key_byte(b) => {
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if is_bare_key_byte(c) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(self.src[start..self.pos].to_string())
            }
            _ => Err(self.err("expected key")),
        }
    }

    fn register_header(&mut self, path: &[String], is_array: bool) -> Result<(), PackageError> {
        let pos = self.pos;
        if path.is_empty() {
            return Err(toml_err(pos, "empty table header"));
        }
        let mut cur = &mut self.root;
        let tracker = &mut self.tracker;
        let mut scoped: Vec<ScopedPart> = Vec::with_capacity(path.len());
        for (idx, seg) in path.iter().enumerate() {
            let is_last = idx + 1 == path.len();
            let mut key_scoped = scoped.clone();
            key_scoped.push(ScopedPart::Key(seg.clone()));
            if tracker.inline_tables.contains(&key_scoped) {
                return Err(toml_err(
                    pos,
                    format!("cannot reopen or extend inline table '{}'", path.join(".")),
                ));
            }
            if is_last {
                return register_leaf_header(tracker, cur, path, seg, key_scoped, is_array, pos);
            }
            let entry = cur
                .entry(seg.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            cur = match entry {
                Value::Object(m) => {
                    scoped = key_scoped;
                    m
                }
                Value::Array(arr) => {
                    if !tracker.array_tables.contains(&key_scoped) {
                        return Err(toml_err(
                            pos,
                            format!("cannot define table inside static array '{seg}'"),
                        ));
                    }
                    let elem_idx = arr.len().saturating_sub(1);
                    let elem = arr
                        .last_mut()
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| toml_err(pos, "invalid array table parent"))?;
                    scoped.push(ScopedPart::ArrayElem(seg.clone(), elem_idx));
                    elem
                }
                _ => return Err(toml_err(pos, format!("key '{seg}' conflicts with table"))),
            };
        }
        Ok(())
    }

    fn insert_kv(
        &mut self,
        current_path: &[String],
        key_path: &[String],
        val: Value,
    ) -> Result<(), PackageError> {
        let pos = self.pos;
        if key_path.is_empty() {
            return Err(toml_err(pos, "empty key path"));
        }
        let mut cur = &mut self.root;
        let tracker = &mut self.tracker;
        let mut scoped: Vec<ScopedPart> = Vec::with_capacity(current_path.len() + key_path.len());
        for seg in current_path {
            let mut key_scoped = scoped.clone();
            key_scoped.push(ScopedPart::Key(seg.clone()));
            cur = match cur.get_mut(seg) {
                Some(Value::Object(m)) => {
                    scoped = key_scoped;
                    m
                }
                Some(Value::Array(arr)) if tracker.array_tables.contains(&key_scoped) => {
                    let elem_idx = arr.len().saturating_sub(1);
                    let elem = arr
                        .last_mut()
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| toml_err(pos, "invalid array table parent"))?;
                    scoped.push(ScopedPart::ArrayElem(seg.clone(), elem_idx));
                    elem
                }
                _ => {
                    return Err(toml_err(
                        pos,
                        format!("invalid current table segment '{seg}'"),
                    ));
                }
            };
        }

        for (idx, seg) in key_path.iter().enumerate() {
            scoped.push(ScopedPart::Key(seg.clone()));
            if idx + 1 == key_path.len() {
                if cur.contains_key(seg)
                    || tracker.defined_tables.contains(&scoped)
                    || tracker.array_tables.contains(&scoped)
                    || tracker.dotted_tables.contains(&scoped)
                {
                    return Err(toml_err(
                        pos,
                        format!("duplicate key '{}'", key_path.join(".")),
                    ));
                }
                if val.is_object() {
                    record_inline_object_paths(&scoped, &val, &mut tracker.inline_tables);
                }
                cur.insert(seg.clone(), val);
                return Ok(());
            }
            if tracker.inline_tables.contains(&scoped) {
                return Err(toml_err(
                    pos,
                    format!("cannot mutate inline table '{}'", seg),
                ));
            }
            if tracker.defined_tables.contains(&scoped) || tracker.array_tables.contains(&scoped) {
                return Err(toml_err(
                    pos,
                    format!("cannot modify defined table '{seg}' via dotted key"),
                ));
            }
            tracker.dotted_tables.insert(scoped.clone());
            let entry = cur
                .entry(seg.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            cur = entry
                .as_object_mut()
                .ok_or_else(|| toml_err(pos, format!("key '{seg}' conflicts with table")))?;
        }
        Ok(())
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, PackageError> {
        if depth > MAX_DEPTH {
            return Err(self.err("value nesting exceeds maximum depth"));
        }
        self.skip_ws_inline();
        if self.starts_with("\"\"\"") {
            return Ok(Value::String(self.parse_multiline_basic_string()?));
        }
        if self.starts_with("'''") {
            return Ok(Value::String(self.parse_multiline_literal_string()?));
        }
        match self.peek() {
            Some(b'"') => Ok(Value::String(self.parse_basic_string()?)),
            Some(b'\'') => Ok(Value::String(self.parse_literal_string()?)),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'{') => self.parse_inline_table(depth + 1),
            Some(b't')
                if self.starts_with("true") && is_delim(self.bytes.get(self.pos + 4).copied()) =>
            {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            Some(b'f')
                if self.starts_with("false") && is_delim(self.bytes.get(self.pos + 5).copied()) =>
            {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            Some(b'0'..=b'9' | b'+' | b'-') => self.parse_number(),
            _ => Err(self.err("unexpected token while parsing value")),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, PackageError> {
        self.pos += 1;
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Array(items));
            }
            let val = self.parse_value(depth)?;
            items.push(val);
            self.skip_ws_and_comments();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_inline_table(&mut self, depth: usize) -> Result<Value, PackageError> {
        self.pos += 1;
        let mut map = Map::new();
        let mut local_inline = HashSet::new();
        self.skip_ws_inline();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(map));
        }
        loop {
            self.skip_ws_inline();
            let key_path = self.parse_dotted_key()?;
            self.skip_ws_inline();
            if self.peek() != Some(b'=') {
                return Err(self.err("expected '=' in inline table"));
            }
            self.pos += 1;
            let val = self.parse_value(depth)?;
            insert_into_inline_map(&mut map, &key_path, val, &mut local_inline)
                .map_err(|e| self.err(e))?;
            self.skip_ws_inline();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws_inline();
                    if self.peek() == Some(b'}') {
                        return Err(self.err("trailing comma not allowed in inline table"));
                    }
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(map));
                }
                _ => return Err(self.err("expected ',' or '}' in inline table")),
            }
        }
    }

    fn parse_basic_string(&mut self) -> Result<String, PackageError> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.src[self.pos..].chars().next() else {
                return Err(self.err("unterminated basic string"));
            };
            self.pos += ch.len_utf8();
            match ch {
                '"' => return Ok(out),
                '\n' | '\r' => return Err(self.err("newline inside single-line string")),
                '\\' => out.push(self.parse_escape()?),
                c if c.is_control() && c != '\t' => {
                    return Err(self.err("unescaped control character in string"));
                }
                c => out.push(c),
            }
        }
    }

    fn parse_multiline_basic_string(&mut self) -> Result<String, PackageError> {
        self.pos += 3;
        if self.peek() == Some(b'\r') && self.bytes.get(self.pos + 1) == Some(&b'\n') {
            self.pos += 2;
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let mut out = String::new();
        loop {
            if self.starts_with("\"\"\"") {
                self.pos += 3;
                let mut extra = 0usize;
                while self.peek() == Some(b'"') && extra < 2 && out.len() < MAX_TOML_BYTES {
                    out.push('"');
                    self.pos += 1;
                    extra += 1;
                }
                return Ok(out);
            }
            let Some(ch) = self.src[self.pos..].chars().next() else {
                return Err(self.err("unterminated multiline basic string"));
            };
            self.pos += ch.len_utf8();
            if ch == '\\' {
                if matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                    while let Some(b) = self.peek() {
                        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    out.push(self.parse_escape()?);
                }
            } else if ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r' {
                return Err(self.err("unescaped control character in multiline basic string"));
            } else {
                out.push(ch);
            }
        }
    }

    fn parse_literal_string(&mut self) -> Result<String, PackageError> {
        self.pos += 1;
        let start = self.pos;
        while let Some(ch) = self.src[self.pos..].chars().next() {
            if ch == '\'' {
                let s = self.src[start..self.pos].to_string();
                self.pos += 1;
                return Ok(s);
            }
            if ch == '\n' || ch == '\r' || (ch.is_control() && ch != '\t') {
                return Err(self.err("invalid character in literal string"));
            }
            self.pos += ch.len_utf8();
        }
        Err(self.err("unterminated literal string"))
    }

    fn parse_multiline_literal_string(&mut self) -> Result<String, PackageError> {
        self.pos += 3;
        if self.peek() == Some(b'\r') && self.bytes.get(self.pos + 1) == Some(&b'\n') {
            self.pos += 2;
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let start = self.pos;
        while self.pos < self.bytes.len() {
            if self.starts_with("'''") {
                let mut s = self.src[start..self.pos].to_string();
                self.pos += 3;
                let mut extra = 0usize;
                while self.peek() == Some(b'\'') && extra < 2 {
                    s.push('\'');
                    self.pos += 1;
                    extra += 1;
                }
                return Ok(s);
            }
            let Some(ch) = self.src[self.pos..].chars().next() else {
                break;
            };
            if ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r' {
                return Err(self.err("unescaped control character in multiline literal string"));
            }
            self.pos += ch.len_utf8();
        }
        Err(self.err("unterminated multiline literal string"))
    }

    fn parse_escape(&mut self) -> Result<char, PackageError> {
        let Some(esc) = self.peek() else {
            return Err(self.err("incomplete escape sequence"));
        };
        self.pos += 1;
        match esc {
            b'b' => Ok('\u{0008}'),
            b't' => Ok('\t'),
            b'n' => Ok('\n'),
            b'f' => Ok('\u{000C}'),
            b'r' => Ok('\r'),
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'u' => self.parse_unicode_escape(4),
            b'U' => self.parse_unicode_escape(8),
            other => Err(self.err(format!("invalid escape '\\{}'", other as char))),
        }
    }

    fn parse_unicode_escape(&mut self, len: usize) -> Result<char, PackageError> {
        if self.pos + len > self.bytes.len() {
            return Err(self.err("incomplete unicode escape"));
        }
        let raw_bytes = &self.bytes[self.pos..self.pos + len];
        if !raw_bytes.iter().all(u8::is_ascii_hexdigit) {
            return Err(self.err("invalid hex in unicode escape"));
        }
        let hex = &self.src[self.pos..self.pos + len];
        self.pos += len;
        let code =
            u32::from_str_radix(hex, 16).map_err(|_| self.err("invalid hex in unicode escape"))?;
        char::from_u32(code).ok_or_else(|| self.err("invalid unicode scalar"))
    }

    fn parse_number(&mut self) -> Result<Value, PackageError> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_delim(Some(b)) {
                break;
            }
            self.pos += 1;
        }
        let raw = &self.src[start..self.pos];
        parse_number_literal(raw, start)
    }
}

fn register_leaf_header(
    tracker: &mut TableTracker,
    cur: &mut Map<String, Value>,
    path: &[String],
    seg: &str,
    key_scoped: Vec<ScopedPart>,
    is_array: bool,
    pos: usize,
) -> Result<(), PackageError> {
    if is_array {
        if tracker.defined_tables.contains(&key_scoped)
            || tracker.dotted_tables.contains(&key_scoped)
        {
            return Err(toml_err(
                pos,
                format!(
                    "cannot reopen table '{}' as array of tables",
                    path.join(".")
                ),
            ));
        }
        match cur.get_mut(seg) {
            None => {
                tracker.array_tables.insert(key_scoped);
                cur.insert(
                    seg.to_string(),
                    Value::Array(vec![Value::Object(Map::new())]),
                );
                Ok(())
            }
            Some(Value::Array(arr)) => {
                if !tracker.array_tables.contains(&key_scoped) {
                    return Err(toml_err(
                        pos,
                        format!(
                            "cannot append to static array '{}' with [[...]]",
                            path.join(".")
                        ),
                    ));
                }
                arr.push(Value::Object(Map::new()));
                Ok(())
            }
            Some(_) => Err(toml_err(
                pos,
                format!("key '{seg}' is not an array of tables"),
            )),
        }
    } else {
        if tracker.array_tables.contains(&key_scoped)
            || tracker.dotted_tables.contains(&key_scoped)
            || !tracker.defined_tables.insert(key_scoped)
        {
            return Err(toml_err(
                pos,
                format!("duplicate or conflicting table '[{}]'", path.join(".")),
            ));
        }
        let entry = cur
            .entry(seg.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            return Err(toml_err(pos, format!("key '{seg}' is not a table")));
        }
        Ok(())
    }
}

fn record_inline_object_paths(
    prefix: &[ScopedPart],
    val: &Value,
    inline_tables: &mut HashSet<Vec<ScopedPart>>,
) {
    if let Value::Object(map) = val {
        inline_tables.insert(prefix.to_vec());
        for (k, child) in map {
            if child.is_object() {
                let mut next = prefix.to_vec();
                next.push(ScopedPart::Key(k.clone()));
                record_inline_object_paths(&next, child, inline_tables);
            }
        }
    }
}
