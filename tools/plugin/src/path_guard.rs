use crate::error::CliError;
use plugin_package::{PackageError, validate_safe_package_path};
use std::fs;
use std::path::{Component, Path, PathBuf};

fn is_allowed_os_root_symlink(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(path.as_os_str().to_str(), Some("/var" | "/tmp" | "/etc"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        false
    }
}

fn check_acc_metadata(acc: &Path, orig: &Path, must_exist: bool) -> Result<(), CliError> {
    match fs::symlink_metadata(acc) {
        Ok(meta) if meta.file_type().is_symlink() && !is_allowed_os_root_symlink(acc) => {
            Err(CliError::Package(PackageError::UnsafePath {
                path: acc.display().to_string(),
                reason: "symlinks are forbidden in plugin path".into(),
            }))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if must_exist {
                Err(CliError::Validation(format!(
                    "path does not exist: {}",
                    orig.display()
                )))
            } else {
                Ok(())
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => {
            if must_exist {
                Err(CliError::Validation(format!(
                    "path does not exist: {}",
                    orig.display()
                )))
            } else {
                Err(CliError::Validation(format!(
                    "parent path is not a directory: {}",
                    orig.display()
                )))
            }
        }
        Err(err) => Err(CliError::Io(err)),
    }
}

fn step_no_symlink_component(
    acc: &mut PathBuf,
    comp: Component<'_>,
    orig: &Path,
    must_exist: bool,
) -> Result<(), CliError> {
    match comp {
        Component::Prefix(p) => {
            acc.push(p.as_os_str());
            Ok(())
        }
        Component::RootDir => {
            acc.push(comp.as_os_str());
            Ok(())
        }
        Component::CurDir => {
            if !acc.as_os_str().is_empty() {
                check_acc_metadata(acc, orig, must_exist)?;
            }
            Ok(())
        }
        Component::ParentDir => {
            match fs::symlink_metadata(&*acc) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(CliError::Package(PackageError::UnsafePath {
                        path: acc.display().to_string(),
                        reason: "symlinks are forbidden in plugin path".into(),
                    }));
                }
                Ok(meta) if !meta.is_dir() => {
                    return Err(CliError::Validation(format!(
                        "parent path `{}` is not a directory",
                        acc.display()
                    )));
                }
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    if must_exist {
                        return Err(CliError::Validation(format!(
                            "path does not exist: {}",
                            orig.display()
                        )));
                    }
                }
                Err(err) => return Err(CliError::Io(err)),
            }
            acc.pop();
            if !acc.as_os_str().is_empty() {
                check_acc_metadata(acc, orig, must_exist)?;
            }
            Ok(())
        }
        Component::Normal(seg) => {
            acc.push(seg);
            check_acc_metadata(acc, orig, must_exist)
        }
    }
}

pub(crate) fn ensure_no_symlink_components(
    path: &Path,
    must_exist: bool,
) -> Result<PathBuf, CliError> {
    if path.as_os_str().is_empty() {
        return Err(CliError::Validation("path does not exist: <empty>".into()));
    }
    let mut acc = PathBuf::new();
    if path.is_relative() {
        let cwd = std::env::current_dir()?;
        for comp in cwd.components() {
            step_no_symlink_component(&mut acc, comp, path, true)?;
        }
    }
    for comp in path.components() {
        step_no_symlink_component(&mut acc, comp, path, must_exist)?;
    }
    check_acc_metadata(&acc, path, must_exist)?;
    Ok(acc)
}

pub(crate) fn ensure_project_root_dir_no_symlink(dir: &Path) -> Result<(), CliError> {
    let clean_dir = ensure_no_symlink_components(dir, true)?;
    let meta = fs::symlink_metadata(&clean_dir).map_err(|_| {
        CliError::Validation(format!(
            "plugin directory does not exist: {}",
            dir.display()
        ))
    })?;
    if meta.file_type().is_symlink() {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: dir.display().to_string(),
            reason: "symlinks are forbidden in plugin project".into(),
        }));
    }
    if !meta.is_dir() {
        return Err(CliError::Validation(format!(
            "plugin directory does not exist: {}",
            dir.display()
        )));
    }
    Ok(())
}

pub(crate) fn ensure_safe_output_subpath_no_symlinks(
    project_root: &Path,
    rel_path: &str,
) -> Result<PathBuf, CliError> {
    ensure_project_root_dir_no_symlink(project_root)?;
    validate_safe_package_path(rel_path)?;
    let comps: Vec<Component<'_>> = Path::new(rel_path).components().collect();
    if comps.is_empty() {
        return Err(CliError::Package(PackageError::UnsafePath {
            path: rel_path.to_string(),
            reason: "empty project subpath".into(),
        }));
    }
    let mut cur = project_root.to_path_buf();
    for (idx, comp) in comps.iter().enumerate() {
        let Component::Normal(seg) = comp else {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: rel_path.to_string(),
                reason: "non-normal path component in project subpath".into(),
            }));
        };
        cur.push(seg);
        let is_last = idx + 1 == comps.len();
        match fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(CliError::Package(PackageError::UnsafePath {
                    path: cur.display().to_string(),
                    reason: "symlinks are forbidden in plugin project".into(),
                }));
            }
            Ok(meta) if !is_last && !meta.is_dir() => {
                return Err(CliError::Validation(format!(
                    "parent path `{}` is not a directory",
                    cur.display()
                )));
            }
            Ok(meta) if is_last && meta.is_dir() => {
                return Err(CliError::Validation(format!(
                    "file path `{}` is a directory",
                    cur.display()
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(CliError::Io(err)),
        }
    }
    Ok(cur)
}

pub(crate) fn read_project_file_no_symlinks(
    project_root: &Path,
    rel_path: &str,
) -> Result<Vec<u8>, CliError> {
    read_project_file_no_symlinks_optional(project_root, rel_path)?
        .ok_or_else(|| CliError::Validation(format!("missing or unreadable file `{rel_path}`")))
}

pub(crate) fn read_project_file_no_symlinks_optional(
    project_root: &Path,
    rel_path: &str,
) -> Result<Option<Vec<u8>>, CliError> {
    let target = ensure_safe_output_subpath_no_symlinks(project_root, rel_path)?;
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(CliError::Package(PackageError::UnsafePath {
                path: target.display().to_string(),
                reason: "symlinks are forbidden in plugin project".into(),
            }))
        }
        Ok(meta) if meta.is_file() => Ok(Some(fs::read(&target)?)),
        Ok(_) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(CliError::Io(err)),
    }
}
