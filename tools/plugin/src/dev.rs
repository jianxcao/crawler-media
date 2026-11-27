use crate::build_cmd::execute_build;
use crate::cli::DevArgs;
use crate::error::CliError;
use crate::pack::pack_project_to_bytes;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub fn execute_dev(args: &DevArgs) -> Result<String, CliError> {
    let token = resolve_token(args.token.as_deref())?;
    let summary = upload_dev_package(&args.dir, &args.instance, &token)?;
    if !args.watch {
        return Ok(summary);
    }

    let mut snapshot = collect_watch_mtimes(&args.dir)?;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let next = collect_watch_mtimes(&args.dir)?;
        if next != snapshot {
            snapshot = next;
            if let Err(err) = upload_dev_package(&args.dir, &args.instance, &token) {
                eprintln!("dev reload failed: {err}");
            }
        }
    }
}

fn resolve_token(explicit: Option<&str>) -> Result<String, CliError> {
    let tok = explicit
        .map(str::to_string)
        .or_else(|| std::env::var("CRAWLER_MEDIA_TOKEN").ok())
        .unwrap_or_default();
    let trimmed = tok.trim();
    if trimmed.is_empty() {
        return Err(CliError::Usage(
            "dev upload requires --token <token> or CRAWLER_MEDIA_TOKEN".into(),
        ));
    }
    Ok(trimmed.to_string())
}

fn upload_dev_package(dir: &Path, instance: &str, token: &str) -> Result<String, CliError> {
    let _ = execute_build(dir)?;
    let (manifest, pkg_bytes) = pack_project_to_bytes(dir)?;

    let url = format!("{}/api/v1/plugins/dev", instance.trim_end_matches('/'));
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut resp = agent
        .post(&url)
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/octet-stream")
        .send(&pkg_bytes[..])
        .map_err(|e| CliError::CommandFailed(format!("failed contacting `{url}`: {e}")))?;

    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|_| String::new());

    if !(200..300).contains(&status) {
        return Err(CliError::RemoteHttp { status, body });
    }

    Ok(format!(
        "Sideloaded dev plugin {}@{} -> {}\n",
        manifest.id, manifest.version, body
    ))
}

fn collect_watch_mtimes(dir: &Path) -> Result<BTreeMap<String, SystemTime>, CliError> {
    let clean_dir: std::path::PathBuf = dir.components().collect();
    let root = if clean_dir.as_os_str().is_empty() {
        dir
    } else {
        clean_dir.as_path()
    };
    if fs::symlink_metadata(root)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(true)
    {
        return Ok(BTreeMap::new());
    }
    let mut map = BTreeMap::new();
    for rel in ["plugin.toml", "schemas", "src", "ui/src", "sites"] {
        let Some((p, meta)) = non_symlink_subpath(root, rel) else {
            continue;
        };
        if meta.is_file() {
            if let Ok(m) = meta.modified() {
                map.insert(rel.to_string(), m);
            }
        } else if meta.is_dir() {
            walk_mtimes(root, &p, &mut map)?;
        }
    }
    Ok(map)
}

fn non_symlink_subpath(root: &Path, rel: &str) -> Option<(std::path::PathBuf, fs::Metadata)> {
    let mut cur = root.to_path_buf();
    let mut last_meta = None;
    for comp in Path::new(rel).components() {
        cur.push(comp);
        let meta = fs::symlink_metadata(&cur).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        last_meta = Some(meta);
    }
    last_meta.map(|m| (cur, m))
}

fn walk_mtimes(
    root: &Path,
    dir: &Path,
    out: &mut BTreeMap<String, SystemTime>,
) -> Result<(), CliError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_mtimes(root, &path, out)?;
        } else if meta.is_file()
            && let Ok(modified) = meta.modified()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.insert(rel.to_string_lossy().into_owned(), modified);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::collect_watch_mtimes;
    use std::fs;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[test]
    fn collect_watch_mtimes_does_not_follow_symlinks() {
        let tmp = TempDir::new().expect("tmp");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(outside.join("ui_real/src")).expect("outside ui");
        fs::write(outside.join("secret.rs"), "pub fn secret() {}\n").expect("secret.rs");
        fs::write(outside.join("ui_real/src/Page.tsx"), "export {};\n").expect("Page.tsx");
        fs::write(outside.join("ext.toml"), "manifest_version = 1\n").expect("ext.toml");

        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join("schemas")).expect("schemas");
        fs::create_dir_all(proj.join("sites")).expect("sites");
        fs::write(proj.join("schemas/config.schema.json"), "{}\n").expect("schema");

        std::os::unix::fs::symlink(outside.join("ext.toml"), proj.join("plugin.toml"))
            .expect("symlink plugin.toml");
        std::os::unix::fs::symlink(&outside, proj.join("src")).expect("symlink src");
        std::os::unix::fs::symlink(outside.join("ui_real"), proj.join("ui")).expect("symlink ui");
        std::os::unix::fs::symlink("..", proj.join("sites/loop")).expect("symlink loop");

        let mtimes = collect_watch_mtimes(&proj).expect("collect mtimes");
        let keys: Vec<String> = mtimes.keys().cloned().collect();
        assert_eq!(keys, vec!["schemas/config.schema.json".to_string()]);
    }
}
