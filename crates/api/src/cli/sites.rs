use super::parse::Command;

pub fn parse_sites(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Sites);
    }
    let mut add = false;
    let mut disable = false;
    let mut enable = false;
    let mut name = None;
    let mut url = None;
    let mut profile_id = None;
    let mut cookie = None;
    let mut api_key = None;
    let mut id = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--add" => {
                add = true;
                rest = tail;
            }
            "--disable" => {
                disable = true;
                rest = tail;
            }
            "--enable" => {
                enable = true;
                rest = tail;
            }
            "--name" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--name needs a value".to_string())?;
                name = Some((*value).to_string());
                rest = next;
            }
            "--url" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--url needs a value".to_string())?;
                url = Some((*value).to_string());
                rest = next;
            }
            "--profile" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--profile needs a value".to_string())?;
                profile_id = Some((*value).to_string());
                rest = next;
            }
            "--cookie" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--cookie needs a value".to_string())?;
                cookie = Some((*value).to_string());
                rest = next;
            }
            "--api-key" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--api-key needs a value".to_string())?;
                api_key = Some((*value).to_string());
                rest = next;
            }
            "--id" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--id needs a value".to_string())?;
                id = Some((*value).to_string());
                rest = next;
            }
            other => return Err(format!("unknown sites flag {other}")),
        }
    }
    if [add, disable, enable].iter().filter(|flag| **flag).count() > 1 {
        return Err("use --add, --enable, or --disable, not more than one".into());
    }
    if disable {
        return Ok(Command::SitesDisable {
            id: id.ok_or_else(|| "disable needs --id".to_string())?,
        });
    }
    if enable {
        return Ok(Command::SitesEnable {
            id: id.ok_or_else(|| "enable needs --id".to_string())?,
        });
    }
    if !add {
        return Err(
            "usage: sites | sites --add --name --url --profile [--cookie] [--api-key] | sites --enable|--disable --id <id>"
                .into(),
        );
    }
    Ok(Command::SitesAdd {
        name: name.ok_or_else(|| "add needs --name".to_string())?,
        url: url.ok_or_else(|| "add needs --url".to_string())?,
        profile_id: profile_id.ok_or_else(|| "add needs --profile".to_string())?,
        cookie,
        api_key,
    })
}
