use marker::ProbeTarget;
use std::ffi::OsStr;
use std::process::Command;

#[test]
fn remote_probe_commands_explicitly_bypass_all_proxy_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let strm = tmp.path().join("episode.strm");
    std::fs::write(&strm, "https://media.example/episode.mkv\n").unwrap();
    let target = ProbeTarget::from_path(&strm);

    let mut ffmpeg = Command::new("ffmpeg");
    target.apply_ffmpeg_input(&mut ffmpeg);
    assert_direct_http_options(&ffmpeg);

    let mut ffprobe = Command::new("ffprobe");
    target.apply_input(&mut ffprobe);
    assert_direct_http_options(&ffprobe);
}

fn assert_direct_http_options(command: &Command) {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let proxy_flag = args.iter().position(|arg| arg == "-http_proxy");
    assert!(
        proxy_flag.is_some(),
        "remote media requests must set an empty http_proxy"
    );
    assert_eq!(args[proxy_flag.unwrap() + 1], "");

    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == OsStr::new(name) && value.is_none()),
            "{name} must be removed from the child process environment"
        );
    }
}
