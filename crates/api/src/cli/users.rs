use serde_json::json;

use super::Transport;
use super::parse::Command;

pub fn parse_users(args: &[&str]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Users);
    }
    let mut add = false;
    let mut login = None;
    let mut token = None;
    let mut rest = args;
    while let Some((flag, tail)) = rest.split_first() {
        match *flag {
            "--add" => {
                add = true;
                rest = tail;
            }
            "--login" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--login needs a value".to_string())?;
                login = Some((*value).to_string());
                rest = next;
            }
            "--token" => {
                let (value, next) = tail
                    .split_first()
                    .ok_or_else(|| "--token needs a value".to_string())?;
                token = Some((*value).to_string());
                rest = next;
            }
            other => return Err(format!("unknown users flag {other}")),
        }
    }
    if !add {
        return Err("usage: users | users --add --login <login> --token <token>".into());
    }
    Ok(Command::UsersAdd {
        login: login.ok_or_else(|| "add needs --login".to_string())?,
        token: token.ok_or_else(|| "add needs --token".to_string())?,
    })
}

pub async fn create_user(
    transport: &impl Transport,
    login: &str,
    token: &str,
) -> Result<String, String> {
    if login.trim().is_empty() || token.trim().is_empty() {
        return Err("login and token are required".into());
    }
    let body = json!({ "login": login, "password": token });
    let (status, body) = transport.send("POST", "/api/v1/users", Some(body)).await?;
    if !(200..300).contains(&status) {
        return Err(format!("users add failed: {status} {body}"));
    }
    let login = body["login"].as_str().unwrap_or(login);
    Ok(format!("login={login}\n"))
}
