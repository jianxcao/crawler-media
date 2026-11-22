#[test]
fn source_observation_keeps_available_creation_time() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Movie.mkv");
    std::fs::write(&path, b"movie").unwrap();
    let created = std::fs::metadata(&path)
        .unwrap()
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok());
    let observation = library::observe_source(&path).unwrap();
    assert_eq!(observation.created_ns, created);
    assert_eq!(observation.size_bytes, Some(5));
    assert!(observation.modified_ns.is_some());
}

#[test]
fn strm_observation_streams_exact_sha256_for_large_and_open_files() {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let tmp = tempfile::tempdir().unwrap();
    let strm = tmp.path().join("Movie.strm");
    let payload = format!(
        "https://example.invalid/stream?pad={}\n",
        "x".repeat(32_773)
    );
    std::fs::write(&strm, payload.as_bytes()).unwrap();

    let expected_digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
    let expected_version = format!("strm:{expected_digest}");

    let observed = library::observe_source(&strm).unwrap();
    assert_eq!(observed.source_version, expected_version);
    assert_eq!(
        observed.content_digest.as_deref(),
        Some(expected_digest.as_str())
    );
    assert_eq!(library::current_source_version(&strm), expected_version);

    let mut file = std::fs::File::open(&strm).unwrap();
    let mut prefix = [0u8; 16];
    file.read_exact(&mut prefix).unwrap();
    let metadata = file.metadata().unwrap();
    let open_observed = library::observe_open_source(&strm, &file, &metadata).unwrap();
    assert_eq!(open_observed.source_version, expected_version);
    assert_eq!(
        open_observed.content_digest.as_deref(),
        Some(expected_digest.as_str())
    );
}
