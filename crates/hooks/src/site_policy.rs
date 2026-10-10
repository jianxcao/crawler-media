use domain::Site;
use scraper::{ElementRef, Html, Selector};

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
        // Keep inline markup in its containing clause; a child span is not independent evidence.
        let document = Html::parse_document(body);
        let containers = document
            .tree
            .root()
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(|element| is_clause_container(element.value().name()))
            .filter(|element| !excluded(*element))
            .map(|element| (element, normalize(&visible_text(element))))
            .collect::<Vec<_>>();
        if containers.iter().any(|(_, text)| {
            [
                "签到失败",
                "签到未成功",
                "未签到成功",
                "登录失败",
                "Attendance failed",
                "Check-in failed",
                "Login required",
            ]
            .iter()
            .any(|failure| text.contains(&normalize(failure)))
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
            containers.iter().any(|(element, text)| {
                // Wrappers cannot launder confirmation from a nested help/result block.
                !element
                    .descendants()
                    .skip(1)
                    .filter_map(ElementRef::wrap)
                    .any(|child| is_clause_container(child.value().name()) || help_semantics(child))
                    && !instructional_context(*element)
                    && markers.iter().any(|marker| {
                        text.strip_prefix(&normalize(marker))
                            .is_some_and(confirmation_suffix)
                    })
            })
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

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_clause_container(name: &str) -> bool {
    matches!(
        name,
        "body"
            | "div"
            | "p"
            | "td"
            | "th"
            | "li"
            | "section"
            | "article"
            | "main"
            | "aside"
            | "h1"
            | "h2"
            | "h3"
            | "blockquote"
            | "pre"
    )
}

fn excluded(element: ElementRef<'_>) -> bool {
    std::iter::once(element)
        .chain(element.ancestors().filter_map(ElementRef::wrap))
        .any(|ancestor| {
            let element = ancestor.value();
            let style = normalize(element.attr("style").unwrap_or_default());
            matches!(element.name(), "script" | "style" | "noscript" | "template")
                || element.attr("hidden").is_some()
                || element
                    .attr("aria-hidden")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
                || style.contains("display:none")
                || style.contains("visibility:hidden")
        })
}

fn visible_text(element: ElementRef<'_>) -> String {
    element
        .descendants()
        .filter(|node| !node.ancestors().filter_map(ElementRef::wrap).any(excluded))
        .filter_map(|node| node.value().as_text())
        .map(|text| &**text)
        .collect()
}

fn context_text(element: ElementRef<'_>) -> String {
    element
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            for ancestor in node.ancestors().filter_map(ElementRef::wrap) {
                if ancestor.id() == element.id() {
                    break;
                }
                if is_clause_container(ancestor.value().name())
                    || matches!(ancestor.value().name(), "footer" | "nav" | "header")
                    || matches!(normalize(&visible_text(ancestor)).as_str(), "帮助" | "help")
                    || excluded(ancestor)
                {
                    return None;
                }
            }
            Some(&**text)
        })
        .collect()
}

fn help_semantics(element: ElementRef<'_>) -> bool {
    let semantic = format!(
        "{} {} {}",
        element.value().name(),
        element.value().attr("class").unwrap_or_default(),
        element.value().attr("id").unwrap_or_default(),
    )
    .to_lowercase();
    ["help", "instruction", "example", "tooltip", "faq"]
        .iter()
        .any(|marker| semantic.contains(marker))
}

fn instructional_context(element: ElementRef<'_>) -> bool {
    // Explicit semantics only: arbitrary prose cannot reliably be classified without a
    // profile contract. Ancestors matter when an instruction wraps a nested result block.
    std::iter::once(element)
        .chain(element.ancestors().filter_map(ElementRef::wrap))
        .any(|ancestor| {
            // Parent context uses only its own prose/inline text, not sibling
            // result blocks or footer navigation elsewhere on the page.
            let text = if ancestor.id() == element.id() {
                normalize(&visible_text(ancestor))
            } else {
                normalize(&context_text(ancestor))
            };
            help_semantics(ancestor)
                || text.starts_with("if")
                || [
                    "如果",
                    "若",
                    "假如",
                    "帮助",
                    "說明",
                    "说明",
                    "示例",
                    "例如",
                    "成功后",
                    "成功後",
                    "表示",
                    "将会",
                    "將會",
                    "会显示",
                    "會顯示",
                    "ifattendance",
                    "ifcheck-in",
                    "ifyou",
                    "help",
                    "example",
                    "instructions",
                    "would",
                    "willshow",
                    "willsee",
                    "notattendance",
                    "notcheck-in",
                    "未签到",
                    "未簽到",
                ]
                .iter()
                .any(|marker| text.contains(marker))
        })
}

fn confirmation_suffix(suffix: &str) -> bool {
    let punctuation =
        |character| matches!(character, '！' | '!' | '，' | ',' | '。' | '.' | '：' | ':');
    if !suffix.is_empty() && !suffix.starts_with(punctuation) {
        return false;
    }
    let suffix = suffix.trim_matches(punctuation);
    if suffix.is_empty() {
        return true;
    }
    // Do not accept arbitrary prose after punctuation: only a bounded reward fact.
    ["获得", "獲得", "已获得", "已獲得", "奖励", "獎勵"]
        .iter()
        .filter_map(|prefix| suffix.strip_prefix(prefix))
        .any(|reward| {
            let number_end = reward
                .find(|character: char| !character.is_ascii_digit() && character != '.')
                .unwrap_or(reward.len());
            let (number, unit) = reward.split_at(number_end);
            !number.is_empty()
                && number.chars().any(|character| character.is_ascii_digit())
                && number.parse::<f64>().is_ok_and(|value| value.is_finite())
                && matches!(
                    unit,
                    "魔力" | "魔力值" | "积分" | "積分" | "点魔力" | "點魔力"
                )
        })
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
