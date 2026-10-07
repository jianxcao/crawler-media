use std::process::Command;

fn main() {
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
