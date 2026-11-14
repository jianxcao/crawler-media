use std::process::Command;

fn main() {
    for (key, name) in [
        ("PROFILE", "CM_BUILD_PROFILE"),
        ("OPT_LEVEL", "CM_BUILD_OPT_LEVEL"),
        ("DEBUG", "CM_BUILD_DEBUG"),
        ("TARGET", "CM_BUILD_TARGET"),
        ("CARGO_ENCODED_RUSTFLAGS", "CM_BUILD_RUSTFLAGS"),
    ] {
        println!(
            "cargo:rustc-env={name}={}",
            std::env::var(key).unwrap_or_default()
        );
    }
    // Embed the short git commit so /health and the UI can show which
    // code revision the server is running.
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(hash) = git_hash {
        println!("cargo:rustc-env=GIT_HASH={hash}");
    } else {
        println!("cargo:rustc-env=GIT_HASH=unknown");
    }
}
