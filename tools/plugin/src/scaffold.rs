use crate::cli::ScaffoldArgs;
use crate::error::CliError;
use crate::path_guard::ensure_no_symlink_components;
use crate::scaffold_templates::{
    DEFAULT_CONFIG_SCHEMA, EMPTY_INPUT_SCHEMA, GITIGNORE, UI_APP_PAGE_TSX, UI_CONFIG_TSX,
    UI_DASHBOARD_TSX, UI_ENTRY_TS, UI_PAGE_TSX, UI_TSCONFIG_JSON, WIT_SERVICES, WIT_TYPES,
    WIT_WORLDS, build_cargo_lock, build_cargo_toml, build_guest_lib_rs, build_manifest_toml,
    build_ui_package_json, build_ui_vite_config_mjs,
};
use plugin_package::{PackageError, validate_manifest_toml_with_policy};
use std::fs;
use std::path::{Path, PathBuf};

const SHORT_WORLDS: &[&str] = &[
    "hook-plugin",
    "task-plugin",
    "endpoint-plugin",
    "indexer-plugin",
    "maintenance-plugin",
];

pub fn execute_scaffold(args: &ScaffoldArgs) -> Result<String, CliError> {
    if args.api != plugin_api::API_VERSION {
        return Err(CliError::Usage(format!(
            "unsupported --api version `{}`: only `{}` is supported",
            args.api,
            plugin_api::API_VERSION
        )));
    }

    let with_ui = match args.ui.as_deref() {
        None => false,
        Some("react") => true,
        Some(other) => {
            return Err(CliError::Usage(format!(
                "unsupported --ui framework `{other}`: only `react` is supported"
            )));
        }
    };

    let world_short = resolve_world(args.world.as_deref(), with_ui)?;
    let target_dir = ensure_scaffold_target_no_symlinks(&args.dir)?;
    let raw_dir_slug = dir_slug(&target_dir);
    let plugin_id = match &args.id {
        Some(explicit) => explicit.clone(),
        None => format!("org.example.{raw_dir_slug}"),
    };
    let display_name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{} Plugin", to_title(&raw_dir_slug)));
    let remote_name = format!("cm_{}", raw_dir_slug.replace('-', "_"));

    let manifest_toml = build_manifest_toml(
        &plugin_id,
        &display_name,
        world_short,
        with_ui,
        &remote_name,
    );
    validate_manifest_toml_with_policy(&manifest_toml, &plugin_package::PackagePolicy::default())?;

    let target_dir = ensure_no_symlink_components(&target_dir, false)?;
    write_scaffold_files(
        &target_dir,
        &raw_dir_slug,
        world_short,
        with_ui,
        &remote_name,
        &manifest_toml,
    )?;

    Ok(format!(
        "Scaffolded plugin `{plugin_id}` in {}\n",
        args.dir.display()
    ))
}

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

fn ensure_scaffold_target_no_symlinks(dir: &Path) -> Result<PathBuf, CliError> {
    let target_dir = ensure_no_symlink_components(dir, false)?;
    match fs::symlink_metadata(&target_dir) {
        Ok(meta) if meta.file_type().is_symlink() && !is_allowed_os_root_symlink(&target_dir) => {
            return Err(CliError::Package(PackageError::UnsafePath {
                path: target_dir.display().to_string(),
                reason: "symlinks are forbidden in plugin project".into(),
            }));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(CliError::Validation(format!(
                "path `{}` already exists and is not a directory",
                target_dir.display()
            )));
        }
        Ok(meta) if meta.is_dir() && fs::read_dir(&target_dir)?.next().is_some() => {
            return Err(CliError::Validation(format!(
                "target directory `{}` already exists and is not empty",
                dir.display()
            )));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(CliError::Io(err)),
    }
    Ok(target_dir)
}

fn resolve_world(raw: Option<&str>, with_ui: bool) -> Result<Option<&'static str>, CliError> {
    match raw {
        None if with_ui => Ok(None),
        None => Ok(Some("hook-plugin")),
        Some(val) => {
            let normalized = val
                .strip_prefix("crawler:plugin/")
                .and_then(|s| s.split('@').next())
                .unwrap_or(val);
            SHORT_WORLDS
                .iter()
                .copied()
                .find(|&w| w == normalized)
                .map(Some)
                .ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown --world `{val}`; expected one of {}",
                        SHORT_WORLDS.join(", ")
                    ))
                })
        }
    }
}

fn write_scaffold_files(
    dir: &Path,
    slug: &str,
    world_short: Option<&str>,
    with_ui: bool,
    remote_name: &str,
    manifest_toml: &str,
) -> Result<(), CliError> {
    fs::create_dir_all(dir.join("schemas"))?;
    fs::write(dir.join(".gitignore"), GITIGNORE)?;
    fs::write(dir.join("plugin.toml"), manifest_toml)?;
    fs::write(
        dir.join("schemas/config.schema.json"),
        DEFAULT_CONFIG_SCHEMA,
    )?;

    if let Some(world) = world_short {
        let crate_name = format!("cm_plugin_{}", slug.replace('-', "_"));
        fs::create_dir_all(dir.join("components"))?;
        fs::create_dir_all(dir.join("wit"))?;
        fs::create_dir_all(dir.join("src"))?;
        fs::write(dir.join("wit/types.wit"), WIT_TYPES)?;
        fs::write(dir.join("wit/services.wit"), WIT_SERVICES)?;
        fs::write(dir.join("wit/worlds.wit"), WIT_WORLDS)?;
        fs::write(dir.join("Cargo.toml"), build_cargo_toml(&crate_name))?;
        fs::write(dir.join("Cargo.lock"), build_cargo_lock(&crate_name))?;
        fs::write(dir.join("src/lib.rs"), build_guest_lib_rs(world))?;

        match world {
            "task-plugin" => fs::write(
                dir.join("schemas/task-input.schema.json"),
                EMPTY_INPUT_SCHEMA,
            )?,
            "endpoint-plugin" => fs::write(
                dir.join("schemas/endpoint-input.schema.json"),
                EMPTY_INPUT_SCHEMA,
            )?,
            "indexer-plugin" => {
                fs::write(dir.join("schemas/site.schema.json"), EMPTY_INPUT_SCHEMA)?
            }
            _ => {}
        }
    }

    if with_ui {
        let ui_src = dir.join("ui/src");
        fs::create_dir_all(&ui_src)?;
        fs::write(dir.join("ui/package.json"), build_ui_package_json(slug))?;
        fs::write(dir.join("ui/tsconfig.json"), UI_TSCONFIG_JSON)?;
        fs::write(
            dir.join("ui/vite.config.mjs"),
            build_ui_vite_config_mjs(remote_name),
        )?;
        fs::write(ui_src.join("entry.ts"), UI_ENTRY_TS)?;
        fs::write(ui_src.join("Config.tsx"), UI_CONFIG_TSX)?;
        fs::write(ui_src.join("Page.tsx"), UI_PAGE_TSX)?;
        fs::write(ui_src.join("AppPage.tsx"), UI_APP_PAGE_TSX)?;
        fs::write(ui_src.join("Dashboard.tsx"), UI_DASHBOARD_TSX)?;
    }

    Ok(())
}

fn dir_slug(dir: &Path) -> String {
    let raw = dir.file_name().and_then(|s| s.to_str()).unwrap_or("plugin");
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            slug.push(lower);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let truncated: String = slug.trim_matches('-').chars().take(32).collect();
    let cleaned = truncated.trim_end_matches('-');
    if cleaned.is_empty() {
        "plugin".to_string()
    } else {
        cleaned.to_string()
    }
}

fn to_title(slug: &str) -> String {
    slug.split('-')
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut word = first.to_ascii_uppercase().to_string();
                    word.push_str(chars.as_str());
                    word
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
