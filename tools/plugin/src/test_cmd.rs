use crate::error::CliError;
use crate::validate::{ensure_safe_output_subpath_no_symlinks, validate_project_dir};
use plugin_package::PackageError;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn execute_test(dir: &Path, offline: bool) -> Result<String, CliError> {
    let manifest = validate_project_dir(dir, false)?;

    let cargo_toml = ensure_safe_output_subpath_no_symlinks(dir, "Cargo.toml")?;
    let has_cargo_toml = match fs::symlink_metadata(&cargo_toml) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: cargo_toml.display().to_string(),
                reason: "symlinks are forbidden in plugin project".into(),
            }));
        }
        Ok(meta) => meta.is_file(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(CliError::Io(err)),
    };

    if has_cargo_toml {
        let mut cmd = Command::new("cargo");
        cmd.arg("test").arg("--manifest-path").arg(&cargo_toml);
        if offline {
            cmd.arg("--offline");
            cmd.env("CARGO_NET_OFFLINE", "true");
        }
        let output = cmd.output()?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CliError::CommandFailed(format!(
                "cargo test failed:\n{stdout}\n{stderr}"
            )));
        }
    }

    Ok(format!(
        "Plugin tests passed for {}@{}\n",
        manifest.id, manifest.version
    ))
}
