//! `iphone-use-mcp skill …`: learned skills from the terminal, over the
//! daemon's `/agent/skills` (see the daemon's `skills` module).

use anyhow::{bail, Context, Result};
use reqwest::Method;

use crate::client::{DaemonClient, DaemonResponse};

fn answer(response: DaemonResponse) -> Result<serde_json::Value> {
    let json = response.json.clone().unwrap_or_default();
    if response.status == reqwest::StatusCode::NOT_FOUND && response.json.is_none() {
        bail!(
            "this daemon has no skills API (it predates them); upgrade with `iphone-use upgrade`"
        );
    }
    if !response.status.is_success() {
        let why = json
            .get("hint")
            .or_else(|| json.get("error"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| response.preview());
        bail!("{why} (HTTP {})", response.status.as_u16());
    }
    Ok(json)
}

fn print(json: &serde_json::Value, as_json: bool) {
    if as_json {
        println!("{}", serde_json::to_string_pretty(json).unwrap_or_default());
    } else if let Some(text) = crate::device::skills(json) {
        println!("{text}");
    }
}

pub async fn list(daemon: &DaemonClient, bundle: Option<&str>, as_json: bool) -> Result<()> {
    let path = match bundle {
        Some(bundle) => format!(
            "/agent/skills?bundle={}",
            crate::device::query_escape(bundle)
        ),
        None => "/agent/skills".to_string(),
    };
    print(
        &answer(daemon.request_json(Method::GET, &path, None).await?)?,
        as_json,
    );
    Ok(())
}

pub async fn show(daemon: &DaemonClient, name: &str, as_json: bool) -> Result<()> {
    let path = format!("/agent/skills?name={}", crate::device::query_escape(name));
    print(
        &answer(daemon.request_json(Method::GET, &path, None).await?)?,
        as_json,
    );
    Ok(())
}

/// Any `POST /agent/skills` op; `draft` reads its markdown from `file`.
pub async fn op(
    daemon: &DaemonClient,
    op: &str,
    name: Option<&str>,
    extra: serde_json::Value,
    as_json: bool,
) -> Result<()> {
    let mut body = serde_json::json!({ "op": op });
    if let Some(name) = name {
        body["name"] = serde_json::json!(name);
    }
    if let serde_json::Value::Object(extra) = extra {
        for (key, value) in extra {
            body[key] = value;
        }
    }
    print(
        &answer(
            daemon
                .request_json(Method::POST, "/agent/skills", Some(&body))
                .await?,
        )?,
        as_json,
    );
    Ok(())
}

pub fn read_draft(file: &str) -> Result<String> {
    if file == "-" {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        return Ok(text);
    }
    std::fs::read_to_string(file).with_context(|| format!("reading {file}"))
}
