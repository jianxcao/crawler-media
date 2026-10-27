use super::parse::Command;

pub fn parse_directory(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Directory);
    }
    if args[0] == "--add-root" || args[0] == "--remove-root" {
        return parse_root(args);
    }
    let [flag, value] = args else {
        return Err("use one directory mutation at a time".into());
    };
    match *flag {
        "--watch-inplace" => Ok(Command::DirectoryWatchInplace {
            path: (*value).into(),
        }),
        "--watch-intake" => Ok(Command::DirectoryWatchIntake {
            path: (*value).into(),
        }),
        "--transfer-mode" if matches!(*value, "hardlink" | "copy" | "move") => {
            Ok(Command::DirectoryTransferMode {
                mode: (*value).into(),
            })
        }
        "--scrape" => crate::config::parse_bool(value)
            .map(|enabled| Command::DirectoryScrape { enabled })
            .ok_or_else(|| "scrape must be on or off".into()),
        "--naming-entry-dir"
        | "--naming-movie-file"
        | "--naming-season-dir"
        | "--naming-episode-file" => {
            if value.trim().is_empty() {
                return Err("Naming template must not be empty".into());
            }
            Ok(Command::DirectoryNaming {
                field: flag.trim_start_matches("--").replace('-', "_"),
                pattern: (*value).into(),
            })
        }
        _ => Err(format!("unknown or invalid directory flag {flag}")),
    }
}

fn parse_root(args: &[&str]) -> Result<Command, String> {
    let mut id = None;
    let mut path = None;
    let mut rest = &args[1..];
    while let [flag, value, tail @ ..] = rest {
        match *flag {
            "--id" if id.is_none() => id = Some((*value).to_string()),
            "--path" if path.is_none() => path = Some((*value).to_string()),
            _ => return Err(format!("unknown or repeated root flag {flag}")),
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err("root flag needs a value".into());
    }
    let id = id
        .filter(|id| !id.trim().is_empty())
        .ok_or("root mutation needs --id <Library ID>")?;
    let path = path
        .filter(|path| !path.trim().is_empty())
        .ok_or("root mutation needs --path")?;
    if args[0] == "--add-root" {
        Ok(Command::DirectoryAddRoot { id, path })
    } else {
        Ok(Command::DirectoryRemoveRoot { id, path })
    }
}
