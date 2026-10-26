use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::{FileQuality, LibraryError, ProbeTarget, WorkControl, classify_resolution};

pub trait MediaProbe: Send + Sync {
    fn probe(&self, path: &Path) -> Result<FileQuality, LibraryError>;
    fn probe_controlled(
        &self,
        path: &Path,
        control: &WorkControl,
    ) -> Result<FileQuality, LibraryError> {
        control.check()?;
        let result = self.probe(path);
        control.check()?;
        result
    }
}

pub struct Ffprobe {
    program: PathBuf,
}

impl Default for Ffprobe {
    fn default() -> Self {
        Self::with_program("ffprobe")
    }
}

impl Ffprobe {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl MediaProbe for Ffprobe {
    fn probe(&self, path: &Path) -> Result<FileQuality, LibraryError> {
        self.probe_controlled(path, &WorkControl::default())
    }
    fn probe_controlled(
        &self,
        path: &Path,
        control: &WorkControl,
    ) -> Result<FileQuality, LibraryError> {
        let _permit = marker::media_read::acquire_media_read(path, control.signal())
            .map_err(|_| LibraryError::Cancelled)?;
        let mut command = Command::new(&self.program);
        command.args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,color_transfer:stream_side_data",
            "-of",
            "json",
        ]);
        ProbeTarget::from_path(path).apply_input(&mut command);
        let output = control.output(&mut command)?;
        if !output.status.success() {
            return Err(LibraryError::Probe(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        let parsed: ProbeOutput = serde_json::from_slice(&output.stdout)
            .map_err(|error| LibraryError::Probe(error.to_string()))?;
        let stream = parsed
            .streams
            .into_iter()
            .next()
            .ok_or_else(|| LibraryError::Probe("no video stream".into()))?;
        let hdr = hdr(&stream);
        Ok(FileQuality {
            resolution: resolution(stream.width, stream.height),
            codec: stream.codec_name,
            hdr,
        })
    }
}

#[derive(Deserialize)]
struct ProbeOutput {
    streams: Vec<ProbeStream>,
}

#[derive(Deserialize)]
struct ProbeStream {
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    color_transfer: Option<String>,
    #[serde(default)]
    side_data_list: Vec<ProbeSideData>,
}

#[derive(Deserialize)]
struct ProbeSideData {
    #[serde(rename = "side_data_type")]
    kind: Option<String>,
}

fn resolution(width: Option<u32>, height: Option<u32>) -> Option<String> {
    classify_resolution(width, height)
}

fn hdr(stream: &ProbeStream) -> Option<String> {
    let transfer = stream.color_transfer.as_deref().unwrap_or_default();
    if transfer.eq_ignore_ascii_case("smpte2084") {
        return Some("HDR10".into());
    }
    if transfer.eq_ignore_ascii_case("arib-std-b67") {
        return Some("HLG".into());
    }
    stream.side_data_list.iter().find_map(|side| {
        side.kind
            .as_deref()
            .is_some_and(|kind| kind.contains("DOVI") || kind.contains("Dolby Vision"))
            .then(|| "Dolby Vision".into())
    })
}
