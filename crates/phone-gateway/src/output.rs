//! Bounded CLI output for LLM tools; the wire response remains unchanged.
use crate::{config, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::ValueEnum;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Compact,
}

const NODE_LIMIT: usize = 400;
const IMAGE_KEYS: [&str; 4] = ["image", "base64", "png", "data"];

// Unicode General_Category=Cf, plus zero-width grapheme/variation selectors.
fn invisible(c: char) -> bool {
    matches!(c as u32, 0x00ad | 0x034f | 0x061c | 0x06dd | 0x070f | 0x0890..=0x0891 |
        0x08e2 | 0x180b..=0x180f | 0x200b..=0x200f | 0x202a..=0x202e |
        0x2060..=0x2064 | 0x2066..=0x206f | 0xfe00..=0xfe0f | 0xfeff |
        0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f |
        0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f |
        0xe0100..=0xe01ef)
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !invisible(*c))
        .map(|c| {
            if c.is_control() || c.is_whitespace() {
                ' '
            } else {
                c
            }
        })
        .take(80)
        .collect()
}

fn text(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        clean(
            &value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        )
    }
}

fn truncated(value: &Value) -> &'static str {
    match value {
        Value::Bool(true) => "true",
        Value::Bool(false) => "false",
        Value::Number(n) if n.as_f64() == Some(1.0) => "true",
        Value::Number(n) if n.as_f64() == Some(0.0) => "false",
        _ => "unknown",
    }
}

fn element_type(value: &Value) -> String {
    // XCUIAutomation.framework/Headers/XCUIElementTypes.h (local Xcode SDK).
    let n = value.as_u64().unwrap_or(0);
    match n {
        1 => "Other",
        2 => "Application",
        4 => "Window",
        9 => "Button",
        21 => "NavigationBar",
        40 => "Switch",
        43 => "Image",
        45 => "SearchField",
        48 => "StaticText",
        49 => "TextField",
        50 => "SecureTextField",
        75 => "Cell",
        _ => return format!("t{n}"),
    }
    .into()
}

fn node_line(index: usize, node: &Value) -> Option<String> {
    let label = text(&node["label"]);
    let identifier = text(&node["identifier"]);
    let value = text(&node["value"]);
    if label.is_empty() && identifier.is_empty() && value.is_empty() {
        return None;
    }
    let frame =
        ["x", "y", "w", "h"].map(|k| node["frame"][k].as_f64().unwrap_or(0.0).round() as i64);
    let i = node["i"].as_u64().unwrap_or(index as u64);
    Some(format!(
        "[{i}] {} {} id={} val={} @{},{},{},{}",
        element_type(&node["type"]),
        json!(label),
        identifier,
        json!(value),
        frame[0],
        frame[1],
        frame[2],
        frame[3]
    ))
}

fn default_screenshot_path() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("phone-gateway");
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Do not follow a symlink for the automatically managed directory.
            if !fs::symlink_metadata(&dir)?.file_type().is_dir() {
                return Err("invalid screenshot directory".into());
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(dir.join(format!("shot-{}.png", uuid::Uuid::new_v4())))
}

pub fn render(
    action: &str,
    mut response: Value,
    format: OutputFormat,
    ms: u128,
    save: Option<&Path>,
) -> Result<String> {
    let ok = response["ok"] == true;
    if action == "screenshot"
        && ((format == OutputFormat::Json && save.is_some())
            || (format == OutputFormat::Compact && ok))
    {
        let path = match save {
            Some(path) => path.to_owned(),
            None => default_screenshot_path()?,
        };
        let result = response["result"]
            .as_object_mut()
            .ok_or("missing screenshot result")?;
        let key = IMAGE_KEYS
            .into_iter()
            .find(|k| result.get(*k).is_some_and(Value::is_string))
            .ok_or("missing base64 image")?;
        let bytes = STANDARD.decode(result[key].as_str().unwrap())?;
        config::private_write(&path, &bytes)?;
        if format == OutputFormat::Compact {
            return Ok(format!(
                "ok screenshot {ms}ms saved={} {}x{}",
                path.display(),
                result.get("width").unwrap_or(&Value::Null),
                result.get("height").unwrap_or(&Value::Null)
            ));
        }
        result.insert(key.into(), json!({"saved":path,"bytes":bytes.len()}));
    }
    if format == OutputFormat::Json {
        return Ok(response.to_string());
    }
    let mut lines = vec![if ok {
        format!("ok {action} {ms}ms")
    } else {
        format!("error {} {ms}ms", text(&response["error"]["code"]))
    }];
    if let Some(result) = response["result"].as_object() {
        let mut small = result.clone();
        small.remove("nodes");
        for key in IMAGE_KEYS {
            small.remove(key);
        }
        if matches!(action, "read" | "wait_for") {
            // The phone's wait_for returns a singular `node`, not a `nodes` array.
            let nodes: Vec<&Value> = match result.get("nodes").and_then(Value::as_array) {
                Some(nodes) => nodes.iter().collect(),
                None => result.get("node").into_iter().collect(),
            };
            small.remove("node");
            let rendered: Vec<_> = nodes
                .iter()
                .enumerate()
                .filter_map(|(i, node)| node_line(i, node))
                .collect();
            lines.push(format!(
                "pid={} snapshot={} truncated={} nodes={} shown={}",
                response["result"]["pid"],
                text(&response["result"]["snapshot_id"]),
                truncated(&response["result"]["truncated"]),
                nodes.len(),
                rendered.len().min(NODE_LIMIT)
            ));
            for key in ["pid", "snapshot_id", "truncated"] {
                small.remove(key);
            }
            if !small.is_empty() {
                lines.push(json!(small).to_string().chars().take(2000).collect());
            }
            lines.extend(rendered.iter().take(NODE_LIMIT).cloned());
            if rendered.len() > NODE_LIMIT {
                lines.push(format!("... {} more", rendered.len() - NODE_LIMIT));
            }
        } else if !small.is_empty() {
            lines.push(json!(small).to_string().chars().take(2000).collect());
        }
    }
    Ok(lines.join("\n"))
}
