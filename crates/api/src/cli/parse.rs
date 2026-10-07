#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Serve,
    Sites,
    SitesAdd {
        name: String,
        url: String,
        profile_id: String,
        cookie: Option<String>,
        api_key: Option<String>,
    },
    SitesDisable {
        id: String,
    },
    SitesEnable {
        id: String,
    },
    Subscribe {
        title: String,
        kind: String,
    },
    Subscribes,
    Filters,
    FiltersAdd {
        name: String,
        atoms: Vec<super::filters::FilterAtomSpec>,
    },
    FiltersDefault {
        id: String,
    },
    Downloaders,
    DownloadersAdd {
        name: String,
        kind: String,
        url: String,
        username: Option<String>,
        password: Option<String>,
        is_default: bool,
    },
    DownloadersDefault {
        id: String,
    },
    Users,
    UsersAdd {
        login: String,
        token: String,
    },
    Jobs,
    JobsTick {
        now: Option<i64>,
    },
    Search {
        query: String,
    },
    CatalogSearch {
        query: String,
    },
    Catalog,
    CatalogDelete {
        source: String,
        cache_key: String,
    },
    Admit {
        subscribe_id: String,
        enclosure: String,
    },
    Library,
    Ledger,
    Directory,
    DirectoryAddRoot {
        kind: String,
        path: String,
    },
    DirectoryRemoveRoot {
        id: String,
    },
    DirectoryWatchInplace {
        path: String,
    },
    DirectoryWatchIntake {
        path: String,
    },
    DirectoryTransferMode {
        mode: String,
    },
    DirectoryScrape {
        enabled: bool,
    },
    DirectoryMovieNaming {
        pattern: String,
    },
    DirectoryTvNaming {
        pattern: String,
    },
    Downloads,
    Unidentified,
    ClaimUnidentified {
        path: String,
        title: String,
        kind: String,
        year: Option<u16>,
    },
}

impl Command {
    pub fn parse(args: &[impl AsRef<str>]) -> Result<Self, String> {
        let args: Vec<&str> = args.iter().map(|arg| arg.as_ref()).collect();
        match args.first().copied() {
            None | Some("serve") => Ok(Self::Serve),
            Some("sites") => super::sites::parse_sites(&args[1..]),
            Some("subscribe") => parse_subscribe(&args[1..]),
            Some("filters") => super::filters::parse_filters(&args[1..]),
            Some("jobs") => parse_jobs(&args[1..]),
            Some("search") => parse_search(&args[1..]),
            Some("catalog") => parse_catalog(&args[1..]),
            Some("library") if args.len() == 1 => Ok(Self::Library),
            Some("library") => Err("usage: library".into()),
            Some("ledger") if args.len() == 1 => Ok(Self::Ledger),
            Some("ledger") => Err("usage: ledger".into()),
            Some("directory") => super::directory::parse_directory(&args[1..]),
            Some("downloaders") => super::downloaders::parse_downloaders(&args[1..]),
            Some("downloads") if args.len() == 1 => Ok(Self::Downloads),
            Some("downloads") => Err("usage: downloads".into()),
            Some("unidentified") => parse_unidentified(&args[1..]),
            Some("users") => super::users::parse_users(&args[1..]),
            Some(other) => Err(format!("unknown command {other}")),
        }
    }
}

fn parse_subscribe(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Subscribes);
    }
    let mut title = None;
    let mut kind = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--title" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--title needs a value".to_string())?;
                title = Some((*value).to_string());
                rest = next;
            }
            "--kind" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--kind needs a value".to_string())?;
                kind = Some((*value).to_string());
                rest = next;
            }
            other => return Err(format!("unknown subscribe flag {other}")),
        }
    }
    Ok(Command::Subscribe {
        title: title.ok_or_else(|| "subscribe needs --title".to_string())?,
        kind: kind.ok_or_else(|| "subscribe needs --kind".to_string())?,
    })
}

fn parse_jobs(args: &[&str]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Jobs),
        ["tick"] => Ok(Command::JobsTick { now: None }),
        ["tick", "--now", now] => {
            let now = now
                .parse::<i64>()
                .map_err(|_| "--now needs a unix timestamp".to_string())?;
            Ok(Command::JobsTick { now: Some(now) })
        }
        _ => Err("usage: jobs | jobs tick [--now <unix>]".into()),
    }
}

fn parse_search(args: &[&str]) -> Result<Command, String> {
    match args {
        ["--query", query] if !query.is_empty() => Ok(Command::Search {
            query: (*query).to_string(),
        }),
        [
            "--admit",
            "--subscribe",
            subscribe_id,
            "--enclosure",
            enclosure,
        ] if !subscribe_id.is_empty() && !enclosure.is_empty() => Ok(Command::Admit {
            subscribe_id: (*subscribe_id).to_string(),
            enclosure: (*enclosure).to_string(),
        }),
        _ => Err(
            "usage: search --query <keyword> | search --admit --subscribe <id> --enclosure <url>"
                .into(),
        ),
    }
}

fn parse_catalog(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Catalog);
    }
    match args {
        ["--query", query] if !query.is_empty() => Ok(Command::CatalogSearch {
            query: (*query).to_string(),
        }),
        ["--delete", "--source", source, "--key", key]
            if !source.is_empty() && !key.is_empty() =>
        {
            Ok(Command::CatalogDelete {
                source: (*source).to_string(),
                cache_key: (*key).to_string(),
            })
        }
        _ => Err("usage: catalog | catalog --query <keyword> | catalog --delete --source <source> --key <cache_key>".into()),
    }
}

fn parse_unidentified(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Unidentified);
    }
    let mut claim = false;
    let mut path = None;
    let mut title = None;
    let mut kind = None;
    let mut year = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--claim" => {
                claim = true;
                rest = tail;
            }
            "--path" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--path needs a value".to_string())?;
                path = Some((*value).to_string());
                rest = next;
            }
            "--title" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--title needs a value".to_string())?;
                title = Some((*value).to_string());
                rest = next;
            }
            "--kind" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--kind needs a value".to_string())?;
                kind = Some((*value).to_string());
                rest = next;
            }
            "--year" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--year needs a value".to_string())?;
                year = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| "--year needs a number".to_string())?,
                );
                rest = next;
            }
            other => return Err(format!("unknown unidentified flag {other}")),
        }
    }
    if !claim {
        return Err("usage: unidentified | unidentified --claim --path --title --kind".into());
    }
    Ok(Command::ClaimUnidentified {
        path: path.ok_or_else(|| "claim needs --path".to_string())?,
        title: title.ok_or_else(|| "claim needs --title".to_string())?,
        kind: kind.ok_or_else(|| "claim needs --kind".to_string())?,
        year,
    })
}
