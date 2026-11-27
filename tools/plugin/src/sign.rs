use crate::error::CliError;
use crate::validate::ensure_no_symlink_components;
use plugin_package::{
    PackagePolicy, parse_signing_key, sign_cmplugin_bytes, validate_cmplugin_bytes,
};
use std::fs;
use std::path::{Path, PathBuf};

pub fn execute_sign(package: &Path, key_path: &Path) -> Result<String, CliError> {
    let clean_pkg = ensure_no_symlink_components(package, true)?;
    let clean_key = ensure_no_symlink_components(key_path, true)?;
    let sig_path = PathBuf::from(format!("{}.minisig", package.display()));
    let clean_sig = ensure_no_symlink_components(&sig_path, false)?;

    if !fs::symlink_metadata(&clean_pkg)?.is_file() {
        return Err(CliError::Validation(format!(
            "plugin package file does not exist: {}",
            package.display()
        )));
    }
    if !fs::symlink_metadata(&clean_key)?.is_file() {
        return Err(CliError::Validation(format!(
            "signing key file does not exist: {}",
            key_path.display()
        )));
    }

    let pkg_bytes = fs::read(&clean_pkg)?;
    validate_cmplugin_bytes(
        &pkg_bytes,
        &PackagePolicy {
            dev_mode: true,
            ..PackagePolicy::default()
        },
    )?;

    let key_raw = fs::read_to_string(&clean_key)?;
    let signing_key = parse_signing_key(&key_raw)?;
    let filename_hint = clean_pkg
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin.cmplugin");
    let sig_text = sign_cmplugin_bytes(&pkg_bytes, &signing_key, filename_hint)?;

    let _ = ensure_no_symlink_components(&sig_path, false)?;
    fs::write(&clean_sig, sig_text.as_bytes())?;

    Ok(format!(
        "Signed {} (key_id: {}) -> {}\n",
        package.display(),
        signing_key.key_id_hex(),
        sig_path.display()
    ))
}
