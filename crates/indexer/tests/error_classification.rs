use indexer::IndexerError;

#[test]
fn only_parse_is_a_definitive_source_rejection() {
    let parse_err = IndexerError::Parse("missing download link".into());
    assert!(parse_err.is_definitive_source_rejection());

    let yaml_err = serde_yaml::from_str::<u32>("not-a-number").unwrap_err();
    let io_err = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
    let non_definitive = [
        IndexerError::BrowserBusy,
        IndexerError::BrowserSetup("cdp closed".into()),
        IndexerError::Fetch("503 service unavailable".into()),
        IndexerError::UnknownProfile("custom".into()),
        IndexerError::Io(io_err),
        IndexerError::Yaml(yaml_err),
    ];

    for err in non_definitive {
        assert!(!err.is_definitive_source_rejection(), "unexpected: {err:?}");
    }
}
