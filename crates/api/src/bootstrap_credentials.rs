use domain::UserId;
use std::str::FromStr;
use store::{Store, StoreError};

pub const STATIC_ADMIN_UUID_STR: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapMode {
    Production,
    TestFixture,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminBootstrapAction {
    Seed { password: String, cli_token: String },
    Rotate { password: String, cli_token: String },
    Keep { cli_token: String },
}

#[derive(Debug, thiserror::Error)]
pub enum BootstrapCredentialsError {
    #[error(
        "首次启动缺少必要的环境变量 CRAWLER_MEDIA_ADMIN_PASSWORD。安全要求：管理员密码必须独立配置且不可与 CLI Token 相同"
    )]
    MissingAdminPassword,
    #[error(
        "CRAWLER_MEDIA_ADMIN_PASSWORD 不得与 CRAWLER_MEDIA_TOKEN 相同。安全要求：初始口令必须与 CLI 凭据分离"
    )]
    PasswordEqualsToken,
    #[error(
        "检测到已有管理员口令与 CLI Token 相同（旧版本不安全凭据）。生产启动必须设置独立的 CRAWLER_MEDIA_ADMIN_PASSWORD 以完成安全升级"
    )]
    InsecureLegacyNeedsPassword,
    #[error(
        "管理员密码不得与任何已知系统 CLI Bearer 凭据相同。安全要求：登录密码必须与自动化密钥严格分离"
    )]
    PasswordEqualsKnownBearer,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn prepare_fresh_admin(
    store: &Store,
    admin_id: UserId,
    token: &str,
    admin_password: Option<&str>,
    mode: BootstrapMode,
) -> Result<AdminBootstrapAction, BootstrapCredentialsError> {
    match mode {
        BootstrapMode::Production => {
            let Some(pw) = admin_password.filter(|p| !p.trim().is_empty()) else {
                return Err(BootstrapCredentialsError::MissingAdminPassword);
            };
            if pw == token {
                return Err(BootstrapCredentialsError::PasswordEqualsToken);
            }
            if store.admin_password_matches_known_bearer(admin_id, pw, token)? {
                return Err(BootstrapCredentialsError::PasswordEqualsKnownBearer);
            }
            Ok(AdminBootstrapAction::Seed {
                password: pw.to_string(),
                cli_token: token.to_string(),
            })
        }
        BootstrapMode::TestFixture => {
            let pw = admin_password
                .filter(|p| !p.trim().is_empty())
                .unwrap_or(token);
            Ok(AdminBootstrapAction::Seed {
                password: pw.to_string(),
                cli_token: token.to_string(),
            })
        }
    }
}

fn prepare_legacy_admin(
    store: &Store,
    admin_id: UserId,
    token: &str,
    admin_password: Option<&str>,
    mode: BootstrapMode,
) -> Result<AdminBootstrapAction, BootstrapCredentialsError> {
    match mode {
        BootstrapMode::Production => {
            let Some(new_pw) = admin_password.filter(|p| !p.trim().is_empty()) else {
                return Err(BootstrapCredentialsError::InsecureLegacyNeedsPassword);
            };
            if new_pw == token {
                return Err(BootstrapCredentialsError::PasswordEqualsToken);
            }
            if store.admin_password_matches_known_bearer(admin_id, new_pw, token)? {
                return Err(BootstrapCredentialsError::PasswordEqualsKnownBearer);
            }
            Ok(AdminBootstrapAction::Rotate {
                password: new_pw.to_string(),
                cli_token: token.to_string(),
            })
        }
        BootstrapMode::TestFixture => {
            if let Some(new_pw) = admin_password.filter(|p| !p.trim().is_empty()) {
                if new_pw != token {
                    return Ok(AdminBootstrapAction::Rotate {
                        password: new_pw.to_string(),
                        cli_token: token.to_string(),
                    });
                }
            }
            Ok(AdminBootstrapAction::Keep {
                cli_token: token.to_string(),
            })
        }
    }
}

pub fn prepare_admin_credentials(
    store: &Store,
    token: &str,
    admin_password: Option<&str>,
    mode: BootstrapMode,
) -> Result<AdminBootstrapAction, BootstrapCredentialsError> {
    let admin_id = UserId::from_str(STATIC_ADMIN_UUID_STR).map_err(StoreError::from)?;
    if store.get_user(admin_id)?.is_none() || !store.admin_password_is_set(admin_id)? {
        prepare_fresh_admin(store, admin_id, token, admin_password, mode)
    } else if store.legacy_admin_password_matches_known_token(admin_id, token)? {
        prepare_legacy_admin(store, admin_id, token, admin_password, mode)
    } else {
        Ok(AdminBootstrapAction::Keep {
            cli_token: token.to_string(),
        })
    }
}
