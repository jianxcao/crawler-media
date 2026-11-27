use crate::error::CliError;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliCommand {
    Help(String),
    Scaffold(ScaffoldArgs),
    Validate {
        path: PathBuf,
        trust_key: Option<String>,
    },
    Test {
        dir: PathBuf,
        offline: bool,
    },
    Build {
        dir: PathBuf,
    },
    Pack {
        dir: PathBuf,
        output: PathBuf,
    },
    Sign {
        package: PathBuf,
        key: PathBuf,
    },
    Dev(DevArgs),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaffoldArgs {
    pub world: Option<String>,
    pub api: String,
    pub ui: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevArgs {
    pub dir: PathBuf,
    pub instance: String,
    pub token: Option<String>,
    pub watch: bool,
}

pub const MAIN_USAGE: &str = "\
crawler-media plugin <subcommand> [options]

Subcommands:
  scaffold   Create a new plugin project
  validate   Validate a plugin project directory or .cmplugin archive
  test       Run offline contract and unit tests for a plugin project
  build      Build WASM component and React federation UI assets
  pack       Package a built plugin project into a .cmplugin archive
  sign       Sign a .cmplugin package using an Ed25519 seed or minisign key
  dev        Build and sideload a development package onto a running instance";

pub fn parse_args(args: &[String]) -> Result<CliCommand, CliError> {
    let Some(first) = args.first() else {
        return Err(CliError::Usage(MAIN_USAGE.to_string()));
    };
    if is_help_flag(first) {
        return Ok(CliCommand::Help(MAIN_USAGE.to_string()));
    }
    let rest = &args[1..];
    match first.as_str() {
        "scaffold" => parse_scaffold(rest),
        "validate" => parse_validate(rest),
        "test" => parse_test(rest),
        "build" => parse_build(rest),
        "pack" => parse_pack(rest),
        "sign" => parse_sign(rest),
        "dev" => parse_dev(rest),
        other => Err(CliError::Usage(format!(
            "unknown plugin subcommand `{other}`\n\n{MAIN_USAGE}"
        ))),
    }
}

fn is_help_flag(arg: &str) -> bool {
    arg == "--help" || arg == "-h" || arg == "help"
}

fn has_help_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

fn next_val<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, CliError> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| CliError::Usage(format!("missing value for {flag}")))
}

fn parse_scaffold(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin scaffold [--world <world>] [--api 1.0.0] [--ui react] [--id <id>] [--name <name>] <dir>";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut world = None;
    let mut api = plugin_api::API_VERSION.to_string();
    let mut ui = None;
    let mut id = None;
    let mut name = None;
    let mut dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--world" => world = Some(next_val(args, &mut i, "--world")?.to_string()),
            "--api" => api = next_val(args, &mut i, "--api")?.to_string(),
            "--ui" => ui = Some(next_val(args, &mut i, "--ui")?.to_string()),
            "--id" => id = Some(next_val(args, &mut i, "--id")?.to_string()),
            "--name" => name = Some(next_val(args, &mut i, "--name")?.to_string()),
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if dir.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                dir = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }

    let dir = dir.ok_or_else(|| CliError::Usage(format!("missing target directory\n{HELP}")))?;
    Ok(CliCommand::Scaffold(ScaffoldArgs {
        world,
        api,
        ui,
        id,
        name,
        dir,
    }))
}

fn parse_validate(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin validate <path> [--trust-key <pubkey_or_file>]";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut path: Option<PathBuf> = None;
    let mut trust_key: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--trust-key" => {
                let val = next_val(args, &mut i, "--trust-key")?;
                if trust_key.is_some() {
                    return Err(CliError::Usage(format!(
                        "duplicate --trust-key flag\n{HELP}"
                    )));
                }
                trust_key = Some(val.to_string());
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if path.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                path = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }
    let path = path.ok_or_else(|| CliError::Usage(HELP.to_string()))?;
    Ok(CliCommand::Validate { path, trust_key })
}

fn parse_test(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin test <dir> [--offline]";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut dir: Option<PathBuf> = None;
    let mut offline = false;
    for arg in args {
        match arg.as_str() {
            "--offline" => offline = true,
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if dir.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                dir = Some(PathBuf::from(positional));
            }
        }
    }
    let dir = dir.ok_or_else(|| CliError::Usage(HELP.to_string()))?;
    Ok(CliCommand::Test { dir, offline })
}

fn parse_build(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin build <dir>";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    if args.len() != 1 || args[0].starts_with('-') {
        return Err(CliError::Usage(HELP.to_string()));
    }
    Ok(CliCommand::Build {
        dir: PathBuf::from(&args[0]),
    })
}

fn parse_pack(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin pack <dir> --output <out.cmplugin>";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut dir: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--output" | "-o" => {
                output = Some(PathBuf::from(next_val(args, &mut i, "--output")?));
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if dir.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                dir = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }
    match (dir, output) {
        (Some(d), Some(o)) => Ok(CliCommand::Pack { dir: d, output: o }),
        _ => Err(CliError::Usage(HELP.to_string())),
    }
}

fn parse_sign(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str = "Usage: crawler-media plugin sign <pkg.cmplugin> --key <key_file>";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut package: Option<PathBuf> = None;
    let mut key: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--key" | "-k" => {
                key = Some(PathBuf::from(next_val(args, &mut i, "--key")?));
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if package.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                package = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }
    match (package, key) {
        (Some(p), Some(k)) => Ok(CliCommand::Sign { package: p, key: k }),
        _ => Err(CliError::Usage(HELP.to_string())),
    }
}

fn parse_dev(args: &[String]) -> Result<CliCommand, CliError> {
    const HELP: &str =
        "Usage: crawler-media plugin dev <dir> --instance <url> [--token <token>] [--once|--watch]";
    if has_help_flag(args) {
        return Ok(CliCommand::Help(HELP.to_string()));
    }
    let mut dir: Option<PathBuf> = None;
    let mut instance: Option<String> = None;
    let mut token: Option<String> = None;
    let mut watch = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--instance" => instance = Some(next_val(args, &mut i, "--instance")?.to_string()),
            "--token" => token = Some(next_val(args, &mut i, "--token")?.to_string()),
            "--once" => watch = false,
            "--watch" => watch = true,
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!("unknown flag `{other}`\n{HELP}")));
            }
            positional => {
                if dir.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra argument `{positional}`\n{HELP}"
                    )));
                }
                dir = Some(PathBuf::from(positional));
            }
        }
        i += 1;
    }
    match (dir, instance) {
        (Some(d), Some(inst)) => Ok(CliCommand::Dev(DevArgs {
            dir: d,
            instance: inst,
            token,
            watch,
        })),
        _ => Err(CliError::Usage(HELP.to_string())),
    }
}
