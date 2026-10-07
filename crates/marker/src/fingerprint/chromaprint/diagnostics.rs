use std::io::Read;

const MAX_STDERR_TAIL_BYTES: usize = 16 * 1024;
const MAX_LOG_EXCERPT_CHARS: usize = 2_000;

#[derive(Default)]
pub(super) struct FfmpegBenchmark {
    pub(super) user_cpu_ms: Option<u128>,
    pub(super) system_cpu_ms: Option<u128>,
    pub(super) real_ms: Option<u128>,
    pub(super) max_rss_kb: Option<u64>,
}

pub(super) struct CapturedStderr {
    pub(super) text: String,
    pub(super) total_bytes: u64,
    pub(super) truncated: bool,
}

pub(super) fn read_stderr_tail(mut stderr: impl Read) -> Result<CapturedStderr, String> {
    let mut tail = Vec::with_capacity(MAX_STDERR_TAIL_BYTES);
    let mut buffer = [0u8; 4096];
    let mut total_bytes = 0u64;

    loop {
        let length = stderr
            .read(&mut buffer)
            .map_err(|error| format!("failed to read ffmpeg stderr: {error}"))?;
        if length == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(length as u64);
        append_tail(&mut tail, &buffer[..length]);
    }

    let truncated = total_bytes > tail.len() as u64;
    Ok(CapturedStderr {
        text: String::from_utf8_lossy(&tail).into_owned(),
        total_bytes,
        truncated,
    })
}

pub(super) fn parse_benchmark(stderr: &str) -> Option<FfmpegBenchmark> {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with("bench:"))?;
    let mut benchmark = FfmpegBenchmark::default();
    for field in line.split_whitespace().skip(1) {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        match name {
            "utime" => benchmark.user_cpu_ms = parse_seconds(value),
            "stime" => benchmark.system_cpu_ms = parse_seconds(value),
            "rtime" => benchmark.real_ms = parse_seconds(value),
            "maxrss" => {
                benchmark.max_rss_kb = value.strip_suffix("kB").and_then(|rss| rss.parse().ok())
            }
            _ => {}
        }
    }
    Some(benchmark)
}

pub(super) fn sanitized_excerpt(stderr: &str) -> String {
    let headers_redacted = redact_sensitive_headers(stderr);
    let urls_redacted = redact_urls(headers_redacted.trim());
    let sanitized = redact_sensitive_parameters(&urls_redacted);
    sanitized.chars().take(MAX_LOG_EXCERPT_CHARS).collect()
}

fn append_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= MAX_STDERR_TAIL_BYTES {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - MAX_STDERR_TAIL_BYTES..]);
        return;
    }

    let overflow = tail
        .len()
        .saturating_add(chunk.len())
        .saturating_sub(MAX_STDERR_TAIL_BYTES);
    if overflow > 0 {
        tail.drain(..overflow);
    }
    tail.extend_from_slice(chunk);
}

fn parse_seconds(value: &str) -> Option<u128> {
    let seconds = value.strip_suffix('s')?.parse::<f64>().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some((seconds * 1000.0).round() as u128)
}

fn redact_urls(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some((start, scheme)) = next_url(remaining) {
        output.push_str(&remaining[..start]);
        output.push_str("<remote-url>");
        let url_start = start + scheme.len();
        let url_end = remaining[url_start..]
            .find(|character: char| is_url_delimiter(character))
            .map(|offset| url_start + offset)
            .unwrap_or(remaining.len());
        remaining = &remaining[url_end..];
    }
    output.push_str(remaining);
    output
}

fn redact_sensitive_headers(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some((header, _)) = line.split_once(':') else {
                return line.to_string();
            };
            let name = header
                .trim()
                .rsplit([' ', ']'])
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if is_sensitive_header(&name) {
                format!("{header}: <redacted>")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_sensitive_parameters(text: &str) -> String {
    const KEYS: [&str; 10] = [
        "access_token=",
        "api_key=",
        "apikey=",
        "token=",
        "signature=",
        "sig=",
        "key=",
        "password=",
        "secret=",
        "auth=",
    ];
    let lowercase = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;

    while let Some((position, key)) = KEYS
        .iter()
        .filter_map(|key| {
            lowercase[offset..]
                .find(key)
                .map(|relative| (offset + relative, *key))
        })
        .min_by_key(|(position, _)| *position)
    {
        let value_start = position + key.len();
        output.push_str(&text[offset..value_start]);
        output.push_str("<redacted>");
        let value_end = text[value_start..]
            .find(|character: char| is_parameter_delimiter(character))
            .map(|relative| value_start + relative)
            .unwrap_or(text.len());
        offset = value_end;
    }

    output.push_str(&text[offset..]);
    output
}

fn is_sensitive_header(name: &str) -> bool {
    name.contains("authorization")
        || name.contains("cookie")
        || name.contains("token")
        || name.contains("secret")
        || name.contains("api-key")
}

fn is_parameter_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '&' | '#' | '\'' | '"' | ')' | ']' | ',')
}

fn next_url(text: &str) -> Option<(usize, &'static str)> {
    ["https://", "http://"]
        .into_iter()
        .filter_map(|scheme| text.find(scheme).map(|position| (position, scheme)))
        .min_by_key(|(position, _)| *position)
}

fn is_url_delimiter(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\'' | '"' | ')' | ']' | '>' | ',' | ';')
}
