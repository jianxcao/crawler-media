use std::process::{Command, Stdio};

use crate::ProbeTarget;

/// Extract a frame at `ms` into `out` (JPEG, 960px wide) via ffmpeg.
pub fn extract_frame(path: &std::path::Path, ms: i64, out: &std::path::Path) -> Result<(), String> {
    extract_frame_with_ffmpeg(path, ms, out, "ffmpeg")
}

/// 同 `extract_frame`，但 ffmpeg 程序路径可替换。测试传入假程序，避免依赖 PATH。
pub fn extract_frame_with_ffmpeg(
    path: &std::path::Path,
    ms: i64,
    out: &std::path::Path,
    ffmpeg: &str,
) -> Result<(), String> {
    let _permit = marker::media_read::acquire_media_read(path, &marker::WorkControl::default())?;
    let target = ProbeTarget::from_path(path);
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-ss", &format!("{:.3}", ms as f64 / 1000.0)]);
    target.apply_ffmpeg_input(&mut cmd);
    cmd.args([
        "-frames:v",
        "1",
        "-vf",
        "scale=960:-2",
        "-q:v",
        "4",
        &out.display().to_string(),
    ]);
    cmd.stderr(Stdio::piped());
    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【抓帧】ffmpeg 进程启动失败"
            );
            return Err(format!("ffmpeg spawn failed: {error}"));
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = detail.chars().take(500).collect::<String>();
        tracing::warn!(
            path = %path.display(),
            ms,
            detail = %detail,
            "【抓帧】ffmpeg 抓帧失败"
        );
        return Err(format!("ffmpeg frame extraction failed: {detail}"));
    }
    Ok(())
}

/// Best-effort media duration in ms via ffprobe (format=duration).
pub fn probe_duration(path: &std::path::Path) -> Option<i64> {
    let _permit =
        marker::media_read::acquire_media_read(path, &marker::WorkControl::default()).ok()?;
    let target = ProbeTarget::from_path(path);
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "json",
    ]);
    target.apply_input(&mut cmd);
    let output = match cmd.output() {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【时长探测】ffprobe 进程启动失败"
            );
            return None;
        }
    };
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        tracing::warn!(
            path = %path.display(),
            detail = detail.chars().take(500).collect::<String>(),
            "【时长探测】ffprobe 探测失败"
        );
        return None;
    }
    #[derive(serde::Deserialize)]
    struct DurationOut {
        format: DurationFormat,
    }
    #[derive(serde::Deserialize)]
    struct DurationFormat {
        duration: Option<String>,
    }
    let parsed: DurationOut = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "【时长探测】ffprobe 输出解析失败"
            );
            return None;
        }
    };
    let seconds: f64 = parsed.format.duration?.parse().ok()?;
    Some((seconds * 1000.0) as i64)
}
