use domain::Site;
use scraper::{Html, Selector};

use crate::plugins::PluginError;

/// Successful attendance and a verified idempotent repeat are both business success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckInOutcome {
    Completed,
    AlreadyCompleted,
}

/// Per-profile evidence, independent of HTTP transport or Browser page acquisition.
pub trait SiteResponsePolicy {
    fn authenticated(&self, site: &Site, body: &str) -> Result<(), PluginError>;
    fn check_in(&self, site: &Site, body: &str) -> Result<CheckInOutcome, PluginError>;
}

/// Conservative NexusPHP policy. A profile can provide its existing login-success CSS.
#[derive(Default)]
pub struct NexusPhpPolicy {
    login_success_css: Option<String>,
}

impl NexusPhpPolicy {
    pub const fn new(login_success_css: Option<String>) -> Self {
        Self { login_success_css }
    }
}

impl SiteResponsePolicy for NexusPhpPolicy {
    fn authenticated(&self, _: &Site, body: &str) -> Result<(), PluginError> {
        reject_challenge(body)?;
        let css = self
            .login_success_css
            .as_deref()
            .unwrap_or("a[href*='logout.php'], a[data-url*='logout.php'], #logout-confirm");
        if has_element(body, css)? {
            Ok(())
        } else {
            Err(PluginError::Fetch(
                "无法验证 Site 登录状态；请更新 Cookie 或人工登录后重试".into(),
            ))
        }
    }

    fn check_in(&self, _: &Site, body: &str) -> Result<CheckInOutcome, PluginError> {
        reject_challenge(body)?;
        // Only visible content counts: script/attribute/help-text keywords are not confirmation.
        let document = Html::parse_document(body);
        let text = document
            .tree
            .root()
            .descendants()
            .filter_map(|node| {
                if node.ancestors().any(|ancestor| {
                    ancestor.value().as_element().is_some_and(|element| {
                        matches!(element.name(), "script" | "style" | "noscript")
                    })
                }) {
                    return None;
                }
                node.value().as_text().map(|text| text.to_string())
            })
            .collect::<Vec<_>>();
        if text.iter().any(|text| {
            [
                "签到失败",
                "签到未成功",
                "登录失败",
                "Attendance failed",
                "Check-in failed",
                "Login required",
            ]
            .iter()
            .any(|failure| text.contains(failure))
        }) {
            return Err(PluginError::Fetch(
                "Site 返回认证或 Check-in 失败；请检查 Cookie 后重试".into(),
            ));
        }
        let already = [
            "您今天已经签到过了",
            "您今天已签到",
            "今天已经签到",
            "已经签到过",
            "已簽到",
            "You have already attended",
        ];
        let success = [
            "签到成功",
            "簽到成功",
            "Attendance successful",
            "Check-in successful",
        ];
        let confirmed = |markers: &[&str]| {
            text.iter().any(|text| markers.iter().any(|marker| text.contains(marker)))
        };
        if confirmed(&already) {
            Ok(CheckInOutcome::AlreadyCompleted)
        } else if confirmed(&success) {
            Ok(CheckInOutcome::Completed)
        } else {
            Err(PluginError::Fetch(
                "无法验证 Site Check-in 成功或已签到；请检查 Cookie 与站点签到响应".into(),
            ))
        }
    }
}

pub(crate) fn reject_challenge(body: &str) -> Result<(), PluginError> {
    let css = "input[type='password' i], input[name='password' i], form[action*='login' i], input[name*='captcha' i], input[id*='captcha' i], img[src*='captcha' i], iframe[src*='captcha' i], .g-recaptcha, .h-captcha, #challenge-form, #cf-challenge-running";
    if has_element(body, css)?
        || [
            "cf-chl-",
            "Checking your browser",
            "Just a moment",
            "请输入验证码",
            "验证码错误",
        ]
        .iter()
        .any(|marker| body.contains(marker))
    {
        return Err(PluginError::Fetch(
            "Site 返回登录表单或验证码；请更新 Cookie 或完成人工认证".into(),
        ));
    }
    Ok(())
}

fn has_element(body: &str, css: &str) -> Result<bool, PluginError> {
    let selector = Selector::parse(css)
        .map_err(|e| PluginError::Fetch(format!("invalid Site success selector: {e}")))?;
    Ok(Html::parse_document(body)
        .select(&selector)
        .next()
        .is_some())
}
