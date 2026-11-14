use super::capture_types::PcmTimeline;

const PREFIX: &str = "[ashowinfo@cm_timeline @ ";
const MAX_LINE: usize = 4096;

/// O(1) streaming observation of the exact final mono/s16/16000 PCM filter.
#[derive(Clone, Debug, Default)]
pub(crate) struct TimelineObserver {
    line: Vec<u8>,
    oversized: bool,
    incomplete: bool,
    pub(crate) invalid: Option<String>,
    pub(crate) first_pts: Option<i64>,
    next_pts: Option<i64>,
    frames: u64,
    samples: u64,
}

impl TimelineObserver {
    pub(crate) fn consume(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                if self.oversized {
                    self.incomplete = true;
                } else if let Ok(line) = std::str::from_utf8(&self.line) {
                    let line = line.to_owned();
                    self.record(&line);
                } else {
                    self.incomplete = true;
                }
                self.line.clear();
                self.oversized = false;
            } else if self.line.len() < MAX_LINE {
                self.line.push(byte);
            } else {
                self.oversized = true;
            }
        }
    }

    pub(crate) fn finish(&mut self) {
        if !self.line.is_empty() || self.oversized {
            self.consume(b"\n");
        }
    }

    pub(crate) fn state(&self, output_samples: u64, success: bool) -> PcmTimeline {
        if let Some(reason) = &self.invalid {
            return PcmTimeline::Invalid(reason.clone());
        }
        if !success || self.incomplete || self.frames == 0 || self.samples != output_samples {
            return PcmTimeline::Unknown;
        }
        PcmTimeline::Contiguous
    }

    fn record(&mut self, line: &str) {
        let Some(start) = line.find(PREFIX) else {
            return;
        };
        let Some((_, fields)) = line[start..].split_once(']') else {
            self.incomplete = true;
            return;
        };
        let mut fields = fields.split_whitespace();
        let frame = fields
            .next()
            .and_then(|v| v.strip_prefix("n:"))
            .and_then(|v| v.parse::<u64>().ok());
        let pts = fields
            .next()
            .and_then(|v| v.strip_prefix("pts:"))
            .and_then(|v| v.parse::<i64>().ok());
        let time = fields.next().is_some_and(|v| v.starts_with("pts_time:"));
        let valid_format = fields.next() == Some("fmt:s16")
            && fields.next() == Some("channels:1")
            && fields.next() == Some("chlayout:mono")
            && fields.next() == Some("rate:16000");
        let count = fields
            .next()
            .and_then(|v| v.strip_prefix("nb_samples:"))
            .and_then(|v| v.parse::<u64>().ok());
        let (Some(frame), Some(pts), Some(count)) = (frame, pts, count) else {
            self.incomplete = true;
            return;
        };
        if !time || !valid_format || count == 0 || frame != self.frames {
            self.incomplete = true;
            return;
        }
        // The final resampler's time base is exactly 1/16000. No millisecond tolerance.
        if self.next_pts.is_some_and(|expected| expected != pts) {
            self.invalid.get_or_insert_with(|| {
                format!(
                    "decoded_pts_discontinuity: expected {:?}, observed {pts}",
                    self.next_pts
                )
            });
        }
        self.first_pts.get_or_insert(pts);
        self.next_pts = i64::try_from(count)
            .ok()
            .and_then(|count| pts.checked_add(count));
        if self.next_pts.is_none() {
            self.incomplete = true;
        }
        self.samples = match self.samples.checked_add(count) {
            Some(n) => n,
            None => {
                self.incomplete = true;
                self.samples
            }
        };
        self.frames += 1;
    }
}
