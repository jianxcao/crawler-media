use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use crate::ProbeTarget;

/// 尝试通过 ffmpeg 提取媒体容器中的内封字幕轨到输出路径。
/// `media_path`: 视频源文件路径（本地文件或 STRM 虚拟流地址）
/// `stream_index`: 字幕轨在容器中的 stream index（如 2）
/// `codec`: 探测到的编码名称（如 subrip, ass, mov_text 等）
/// `wants_vtt`: 请求方是否期望转换为 WebVTT
/// `cache_dir`: 缓存目录
pub fn extract_embedded_subtitle(
    media_path: &Path,
    stream_index: u32,
    codec: Option<&str>,
    wants_vtt: bool,
    cache_dir: &Path,
) -> Result<(PathBuf, &'static str), String> {
    extract_embedded_subtitle_with_program(
        media_path,
        stream_index,
        codec,
        wants_vtt,
        cache_dir,
        "ffmpeg",
    )
}

pub fn extract_embedded_subtitle_with_program(
    media_path: &Path,
    stream_index: u32,
    codec: Option<&str>,
    wants_vtt: bool,
    cache_dir: &Path,
    program: impl AsRef<std::ffi::OsStr>,
) -> Result<(PathBuf, &'static str), String> {
    let raw_codec = codec.unwrap_or("srt").to_lowercase();
    let is_vtt = wants_vtt || raw_codec == "webvtt" || raw_codec == "vtt";
    let is_ass = raw_codec == "ass" || raw_codec == "ssa";
    let is_sup = raw_codec == "hdmv_pgs_subtitle" || raw_codec == "pgs" || raw_codec == "sup";

    let ext = if is_vtt {
        "vtt"
    } else if is_ass {
        "ass"
    } else if is_sup {
        "sup"
    } else {
        "srt"
    };

    let filename = format!("sub_{stream_index}.{ext}");
    let out_path = cache_dir.join(&filename);

    if out_path.is_file() && std::fs::metadata(&out_path).map(|m| m.len() > 0).unwrap_or(false) {
        let content_type = match ext {
            "vtt" => "text/vtt; charset=utf-8",
            "ass" => "text/x-ssa; charset=utf-8",
            "sup" => "application/octet-stream",
            _ => "text/plain; charset=utf-8",
        };
        return Ok((out_path, content_type));
    }

    std::fs::create_dir_all(cache_dir).map_err(|e| format!("failed to create cache dir: {e}"))?;

    let target = ProbeTarget::from_path(media_path);
    let mut cmd = Command::new(program);
    cmd.arg("-y");
    target.apply_ffmpeg_input(&mut cmd);

    cmd.args(["-map", &format!("0:{stream_index}")]);

    if is_vtt {
        cmd.args(["-c:s", "webvtt", "-f", "webvtt"]);
    } else if is_ass {
        cmd.args(["-c:s", "copy", "-f", "ass"]);
    } else if is_sup {
        cmd.args(["-c:s", "copy", "-f", "sup"]);
    } else {
        cmd.args(["-c:s", "subrip", "-f", "srt"]);
    }

    let temp_out = cache_dir.join(format!("sub_{stream_index}.tmp.{}", std::process::id()));
    cmd.arg(&temp_out);
    cmd.stderr(Stdio::piped());

    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_out);
            return Err(format!("ffmpeg spawn failed: {error}"));
        }
    };

    if !output.status.success() {
        let _ = std::fs::remove_file(&temp_out);
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = detail.chars().take(500).collect::<String>();
        return Err(format!("ffmpeg subtitle extraction failed: {detail}"));
    }

    if let Err(e) = std::fs::rename(&temp_out, &out_path) {
        if let Err(err) = std::fs::copy(&temp_out, &out_path) {
            let _ = std::fs::remove_file(&temp_out);
            return Err(format!("failed to move extracted subtitle: {e}, copy: {err}"));
        }
        let _ = std::fs::remove_file(&temp_out);
    }

    let content_type = match ext {
        "vtt" => "text/vtt; charset=utf-8",
        "ass" => "text/x-ssa; charset=utf-8",
        "sup" => "application/octet-stream",
        _ => "text/plain; charset=utf-8",
    };

    Ok((out_path, content_type))
}
