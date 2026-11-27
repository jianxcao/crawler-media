use crate::error::CliError;
use crate::validate::ensure_no_symlink_components;
use plugin_package::{
    PackagePolicy, parse_signing_key, sign_cmplugin_bytes, validate_cmplugin_bytes,
};
use ring::rand::{SecureRandom, SystemRandom};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub fn execute_keygen(key_path: &Path, pubkey_path: &Path) -> Result<String, CliError> {
    let clean_key = ensure_no_symlink_components(key_path, false)?;
    let clean_pubkey = ensure_no_symlink_components(pubkey_path, false)?;
    if clean_key == clean_pubkey {
        return Err(CliError::Validation(
            "secret key and public key paths must be different".into(),
        ));
    }
    ensure_target_absent(&clean_key, key_path, "secret key")?;
    ensure_target_absent(&clean_pubkey, pubkey_path, "public key")?;

    create_parent_dir(&clean_key)?;
    create_parent_dir(&clean_pubkey)?;
    let clean_key = ensure_no_symlink_components(&clean_key, false)?;
    let clean_pubkey = ensure_no_symlink_components(&clean_pubkey, false)?;
    ensure_target_absent(&clean_key, key_path, "secret key")?;
    ensure_target_absent(&clean_pubkey, pubkey_path, "public key")?;

    let rng = SystemRandom::new();
    let mut seed = [0u8; 32];
    rng.fill(&mut seed)
        .map_err(|_| CliError::CommandFailed("failed to generate random signing key".into()))?;
    let mut hex_seed = String::with_capacity(64);
    for byte in seed {
        let _ = write!(&mut hex_seed, "{byte:02x}");
    }
    seed.fill(0);

    let signing_key = parse_signing_key(&hex_seed)?;
    let pubkey_text = signing_key.public_key_text();
    let key_id = signing_key.key_id_hex();

    write_new_secret_file(&clean_key, hex_seed.as_bytes())?;
    drop(hex_seed);

    if let Err(err) = write_new_file(&clean_pubkey, pubkey_text.as_bytes()) {
        let _ = fs::remove_file(&clean_key);
        return Err(err);
    }

    Ok(format!(
        "Generated signing key (key_id: {key_id}) -> {}, pubkey -> {}\n",
        key_path.display(),
        pubkey_path.display()
    ))
}

fn ensure_target_absent(clean: &Path, display_path: &Path, label: &str) -> Result<(), CliError> {
    match fs::symlink_metadata(clean) {
        Ok(_) => Err(CliError::Validation(format!(
            "{label} path already exists: {}",
            display_path.display()
        ))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(CliError::Io(err)),
    }
}

fn create_parent_dir(target: &Path) -> Result<(), CliError> {
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_new_secret_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_secret_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    write_new_file(path, contents)
}

fn write_new_file(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
}

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
