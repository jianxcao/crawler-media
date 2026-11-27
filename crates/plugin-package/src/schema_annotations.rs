use crate::schema::escape_json_pointer_seg;
use crate::{PackageError, SecretFieldSpec};
use serde_json::{Map, Value};

const ALLOWED_UI_WIDGETS: &[&str] = &[
    "text",
    "textarea",
    "password",
    "select",
    "multi-select",
    "site",
    "downloader",
    "library",
    "subscribe",
    "interval",
    "bytes",
];

const ALLOWED_X_UI_KEYS: &[&str] = &[
    "widget",
    "secret",
    "placeholder",
    "help",
    "options",
    "labels",
    "unit",
    "group",
    "order",
    "visibleWhen",
    "visible_when",
];

pub(crate) fn node_has_secret_flag(val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    let write_only = obj.get("writeOnly").and_then(Value::as_bool) == Some(true);
    let fmt_pw = obj.get("format").and_then(Value::as_str) == Some("password");
    let xui_pw = obj
        .get("x-ui")
        .and_then(Value::as_object)
        .and_then(|x| x.get("widget"))
        .and_then(Value::as_str)
        == Some("password");
    let xui_sec = obj
        .get("x-ui")
        .and_then(Value::as_object)
        .and_then(|x| x.get("secret"))
        .and_then(Value::as_bool)
        == Some(true);
    write_only || fmt_pw || xui_pw || xui_sec
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_annotations_and_secret(
    obj: &Map<String, Value>,
    effective_ty: Option<&str>,
    target_chain: &[&Value],
    inside_array: bool,
    secrets: &mut Vec<SecretFieldSpec>,
    path_segments: Option<&[String]>,
    is_required: bool,
    secrets_before: usize,
) -> Result<(), PackageError> {
    if let Some(fmt) = obj.get("format") {
        let fmt_str = fmt
            .as_str()
            .ok_or_else(|| PackageError::InvalidSchema("format must be a string".into()))?;
        if fmt_str == "cron" {
            return Err(PackageError::InvalidSchema(
                "format `cron` is not supported".into(),
            ));
        }
    }

    let (ui_is_password, ui_is_secret) = if let Some(xui) = obj.get("x-ui") {
        validate_x_ui(xui)?
    } else {
        (false, false)
    };

    let write_only = obj
        .get("writeOnly")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| PackageError::InvalidSchema("writeOnly must be boolean".into()))
        })
        .transpose()?
        .unwrap_or(false);
    let fmt_password = obj.get("format").and_then(Value::as_str) == Some("password");
    let obj_is_secret = write_only || ui_is_password || ui_is_secret || fmt_password;
    let chain_is_secret = obj_is_secret || target_chain.iter().any(|n| node_has_secret_flag(n));

    if chain_is_secret {
        if effective_ty != Some("string") {
            return Err(PackageError::InvalidSchema(
                "secret fields must have type \"string\"".into(),
            ));
        }
        if inside_array {
            return Err(PackageError::InvalidSchema(
                "secret fields are not allowed inside arrays".into(),
            ));
        }
        if obj.contains_key("default") || target_chain.iter().any(|n| n.get("default").is_some()) {
            return Err(PackageError::InvalidSchema(
                "secret fields must not define a plaintext default".into(),
            ));
        }
        if obj_is_secret
            && secrets.len() == secrets_before
            && let Some(segs) = path_segments
            && !segs.is_empty()
        {
            let key = segs.join(".");
            if key.len() > 256 {
                return Err(PackageError::InvalidSchema(
                    "secret field key exceeds 256 UTF-8 bytes".into(),
                ));
            }
            if secrets.iter().any(|s| s.key == key) {
                return Err(PackageError::InvalidSchema(format!(
                    "duplicate secret field key `{key}`"
                )));
            }
            let pointer = format!(
                "/values/{}",
                segs.iter()
                    .map(|s| escape_json_pointer_seg(s))
                    .collect::<Vec<_>>()
                    .join("/")
            );
            secrets.push(SecretFieldSpec {
                key,
                path_segments: segs.to_vec(),
                pointer,
                required: is_required,
            });
        }
    }
    Ok(())
}

fn validate_x_ui(xui: &Value) -> Result<(bool, bool), PackageError> {
    let xmap = xui
        .as_object()
        .ok_or_else(|| PackageError::InvalidSchema("x-ui must be an object".into()))?;
    for k in xmap.keys() {
        if !ALLOWED_X_UI_KEYS.contains(&k.as_str()) {
            return Err(PackageError::InvalidSchema(format!(
                "unsupported x-ui key `{k}`"
            )));
        }
    }
    let mut ui_is_password = false;
    if let Some(w) = xmap.get("widget") {
        let ws = w
            .as_str()
            .ok_or_else(|| PackageError::InvalidSchema("x-ui.widget must be a string".into()))?;
        if !ALLOWED_UI_WIDGETS.contains(&ws) {
            return Err(PackageError::InvalidSchema(format!(
                "unsupported x-ui.widget `{ws}`"
            )));
        }
        ui_is_password = ws == "password";
    }
    let mut ui_is_secret = false;
    if let Some(sec) = xmap.get("secret") {
        ui_is_secret = sec
            .as_bool()
            .ok_or_else(|| PackageError::InvalidSchema("x-ui.secret must be boolean".into()))?;
    }
    if let Some(vw) = xmap.get("visibleWhen").or_else(|| xmap.get("visible_when")) {
        validate_visible_when(vw)?;
    }
    Ok((ui_is_password, ui_is_secret))
}

fn validate_visible_when(vw: &Value) -> Result<(), PackageError> {
    let map = vw
        .as_object()
        .ok_or_else(|| PackageError::InvalidSchema("x-ui.visibleWhen must be an object".into()))?;
    for k in map.keys() {
        if !matches!(k.as_str(), "field" | "eq" | "equals" | "in") {
            return Err(PackageError::InvalidSchema(format!(
                "unsupported x-ui.visibleWhen key `{k}`"
            )));
        }
    }
    if map
        .get("field")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(PackageError::InvalidSchema(
            "x-ui.visibleWhen.field must be a non-empty string".into(),
        ));
    }
    Ok(())
}
