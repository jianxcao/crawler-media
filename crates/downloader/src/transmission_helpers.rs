use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::{PathMap, apply_maps};

#[derive(Deserialize)]
pub(super) struct RpcResponse {
    pub(super) result: String,
    #[serde(default)]
    pub(super) arguments: Value,
}

pub(super) fn base64_basic(user: &str, pass: &str) -> String {
    encode(format!("{user}:{pass}").as_bytes())
}

pub(super) fn encode(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(T[(a >> 2) as usize] as char);
        out.push(T[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(c & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub(super) fn is_transmission_post_send_err(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Io(io_err) => matches!(
            io_err.kind(),
            std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::TimedOut
        ),
        ureq::Error::Timeout(_) => true,
        ureq::Error::StatusCode(code) => matches!(code, 502 | 504),
        _ => false,
    }
}

pub(super) fn extract_item_files(item: &Value, path_maps: &[PathMap], files: &mut Vec<PathBuf>) {
    let dir = item
        .get("downloadDir")
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(listed) = item.get("files").and_then(Value::as_array) {
        let file_stats = item.get("fileStats").and_then(Value::as_array);
        for (i, file) in listed.iter().enumerate() {
            let bytes_completed = file
                .get("bytesCompleted")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let length = file.get("length").and_then(Value::as_u64).unwrap_or(0);
            let wanted = file_stats
                .and_then(|stats| stats.get(i))
                .and_then(|stat| stat.get("wanted"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !wanted || (length > 0 && bytes_completed < length) {
                continue;
            }
            let name = file.get("name").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() {
                files.push(apply_maps(&Path::new(dir).join(name), path_maps));
            }
        }
    }
}
