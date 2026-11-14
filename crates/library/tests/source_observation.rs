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
