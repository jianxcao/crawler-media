use crate::PackageError;
use serde_json::{Map, Number, Value};
use std::collections::HashSet;

pub(crate) fn toml_err(pos: usize, msg: impl Into<String>) -> PackageError {
    PackageError::InvalidManifest(format!("TOML parse error at byte {pos}: {}", msg.into()))
}

pub(crate) fn parse_number_literal(raw: &str, pos: usize) -> Result<Value, PackageError> {
    if !raw.is_ascii() {
        return Err(toml_err(
            pos,
            format!("invalid non-ASCII characters in number '{raw}'"),
        ));
    }
    if let Some(rest) = raw.strip_prefix("0x") {
        return parse_radix_int(raw, rest, 16, |c| c.is_ascii_hexdigit(), pos);
    }
    if let Some(rest) = raw.strip_prefix("0o") {
        return parse_radix_int(raw, rest, 8, |c| matches!(c, '0'..='7'), pos);
    }
    if let Some(rest) = raw.strip_prefix("0b") {
        return parse_radix_int(raw, rest, 2, |c| matches!(c, '0' | '1'), pos);
    }
    let (sign, unsigned) = if let Some(s) = raw.strip_prefix('+') {
        ("", s)
    } else if let Some(s) = raw.strip_prefix('-') {
        ("-", s)
    } else {
        ("", raw)
    };
    if unsigned.contains('.') || unsigned.contains('e') || unsigned.contains('E') {
        return parse_toml_float(raw, sign, unsigned, pos);
    }
    let digits = strip_strict_digits(unsigned, |c| c.is_ascii_digit())
        .map_err(|e| toml_err(pos, format!("{e} in '{raw}'")))?;
    if digits.len() > 1 && digits.starts_with('0') {
        return Err(toml_err(
            pos,
            format!("leading zeros not allowed in '{raw}'"),
        ));
    }
    let n: i64 = format!("{sign}{digits}")
        .parse()
        .map_err(|_| toml_err(pos, format!("invalid integer '{raw}'")))?;
    Ok(Value::Number(Number::from(n)))
}

fn parse_radix_int(
    raw: &str,
    rest: &str,
    radix: u32,
    is_digit: fn(char) -> bool,
    pos: usize,
) -> Result<Value, PackageError> {
    let digits = strip_strict_digits(rest, is_digit)
        .map_err(|e| toml_err(pos, format!("{e} in '{raw}'")))?;
    let n = i64::from_str_radix(&digits, radix)
        .map_err(|_| toml_err(pos, format!("invalid integer '{raw}'")))?;
    Ok(Value::Number(Number::from(n)))
}

fn parse_toml_float(
    raw: &str,
    sign: &str,
    unsigned: &str,
    pos: usize,
) -> Result<Value, PackageError> {
    let (mantissa, opt_exp) = match unsigned.find(['e', 'E']) {
        Some(idx) => (&unsigned[..idx], Some(&unsigned[idx + 1..])),
        None => (unsigned, None),
    };
    let (int_part, opt_frac) = match mantissa.find('.') {
        Some(idx) => (&mantissa[..idx], Some(&mantissa[idx + 1..])),
        None => (mantissa, None),
    };
    let int_digits = strip_strict_digits(int_part, |c| c.is_ascii_digit())
        .map_err(|e| toml_err(pos, format!("{e} in '{raw}'")))?;
    if int_digits.len() > 1 && int_digits.starts_with('0') {
        return Err(toml_err(
            pos,
            format!("leading zeros not allowed in '{raw}'"),
        ));
    }
    let mut assembled = format!("{sign}{int_digits}");
    if let Some(frac) = opt_frac {
        let frac_digits = strip_strict_digits(frac, |c| c.is_ascii_digit())
            .map_err(|e| toml_err(pos, format!("{e} in '{raw}'")))?;
        assembled.push('.');
        assembled.push_str(&frac_digits);
    }
    if let Some(exp) = opt_exp {
        let (exp_sign, exp_body) = if let Some(s) = exp.strip_prefix('+') {
            ("+", s)
        } else if let Some(s) = exp.strip_prefix('-') {
            ("-", s)
        } else {
            ("", exp)
        };
        let exp_digits = strip_strict_digits(exp_body, |c| c.is_ascii_digit())
            .map_err(|e| toml_err(pos, format!("{e} in '{raw}'")))?;
        assembled.push('e');
        assembled.push_str(exp_sign);
        assembled.push_str(&exp_digits);
    }
    let f: f64 = assembled
        .parse()
        .map_err(|_| toml_err(pos, format!("invalid float '{raw}'")))?;
    let num =
        Number::from_f64(f).ok_or_else(|| toml_err(pos, format!("non-finite float '{raw}'")))?;
    Ok(Value::Number(num))
}

pub(crate) fn insert_into_inline_map(
    map: &mut Map<String, Value>,
    path: &[String],
    val: Value,
    local_inline: &mut HashSet<Vec<String>>,
) -> Result<(), String> {
    let mut cur = map;
    let mut prefix = Vec::with_capacity(path.len());
    for (idx, seg) in path.iter().enumerate() {
        prefix.push(seg.clone());
        if idx + 1 == path.len() {
            if cur.contains_key(seg) {
                return Err(format!("duplicate key '{}'", path.join(".")));
            }
            if val.is_object() {
                local_inline.insert(prefix);
            }
            cur.insert(seg.clone(), val);
            return Ok(());
        }
        if local_inline.contains(&prefix) {
            return Err(format!("cannot mutate inline table '{seg}'"));
        }
        let entry = cur
            .entry(seg.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        cur = entry
            .as_object_mut()
            .ok_or_else(|| format!("key '{seg}' conflicts with table"))?;
    }
    Ok(())
}

fn strip_strict_digits(s: &str, is_digit: fn(char) -> bool) -> Result<String, String> {
    if s.is_empty() || s.starts_with('_') || s.ends_with('_') || s.contains("__") {
        return Err("invalid numeric literal".into());
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '_' {
            continue;
        }
        if !is_digit(ch) {
            return Err(format!("unexpected character '{ch}' in number"));
        }
        out.push(ch);
    }
    if out.is_empty() {
        return Err("missing digits in number".into());
    }
    Ok(out)
}

pub(crate) fn is_bare_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

pub(crate) fn is_delim(b: Option<u8>) -> bool {
    matches!(
        b,
        None | Some(b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b'#')
    )
}
