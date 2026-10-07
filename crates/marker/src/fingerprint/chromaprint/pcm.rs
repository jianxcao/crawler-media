use std::io::Read;
use std::time::Instant;

use rusty_chromaprint::Fingerprinter;

use super::timing::ExtractionTimings;

pub(super) fn consume_pcm(
    mut stdout: impl Read,
    fingerprinter: &mut Fingerprinter,
    process_started: Instant,
    timings: &mut ExtractionTimings,
) -> Result<(), String> {
    let started = Instant::now();
    let mut bytes = [0u8; 4096];
    let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut pending_low_byte = None;

    loop {
        let read_started = Instant::now();
        let read_result = stdout.read(&mut bytes);
        timings.pcm_read_wait += read_started.elapsed();
        let length = read_result.map_err(|error| error.to_string())?;
        if length == 0 {
            break;
        }
        record_pcm_read(length, process_started, timings);
        decode_pcm_chunk(&bytes[..length], &mut pending_low_byte, &mut samples);
        let consume_started = Instant::now();
        fingerprinter.consume(&samples);
        timings.chromaprint_consume += consume_started.elapsed();
        timings.sample_count += samples.len() as u64;
    }

    timings.pcm_stream_elapsed_ms = started.elapsed().as_millis();
    if pending_low_byte.is_some() {
        return Err("ffmpeg produced an incomplete 16-bit PCM sample".into());
    }
    Ok(())
}

fn record_pcm_read(length: usize, process_started: Instant, timings: &mut ExtractionTimings) {
    timings
        .time_to_first_pcm_ms
        .get_or_insert_with(|| process_started.elapsed().as_millis());
    timings.pcm_bytes += length as u64;
}

fn decode_pcm_chunk(bytes: &[u8], pending_low_byte: &mut Option<u8>, samples: &mut Vec<i16>) {
    samples.clear();
    let mut start = 0;
    if let Some(low_byte) = pending_low_byte.take() {
        samples.push(i16::from_le_bytes([low_byte, bytes[0]]));
        start = 1;
    }
    let aligned_length = (bytes.len() - start) & !1;
    for sample in bytes[start..start + aligned_length].chunks_exact(2) {
        samples.push(i16::from_le_bytes([sample[0], sample[1]]));
    }
    if start + aligned_length < bytes.len() {
        *pending_low_byte = Some(bytes[bytes.len() - 1]);
    }
}
