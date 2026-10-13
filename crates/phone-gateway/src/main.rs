use base64::{engine::general_purpose::STANDARD, Engine};
use clap::{Parser, Subcommand};
use phone_gateway::output::{render, OutputFormat};
use phone_gateway::{
    config::{self, Config},
    gateway::{valid_phase_two_args, Gateway, Timing},
    Result,
};
use serde_json::{json, Value};
use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Pair {
        #[arg(long, default_value = "localhost")]
        host: String,
        #[arg(long, default_value_t = 7381)]
        port: u16,
        #[arg(long)]
        rotate: bool,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    Serve {
        #[arg(long, default_value = "0.0.0.0:7381")]
        bind: SocketAddr,
        #[arg(long, default_value = "127.0.0.1:7382")]
        http: SocketAddr,
    },
    Cmd {
        action: String,
        #[arg(long, value_name = "HOST:PORT")]
        endpoint: Option<String>,
        #[arg(long, value_name = "foreground|system")]
        target: Option<String>,
        #[arg(long)]
        args: Option<String>,
        #[arg(long)]
        deadline_ms: Option<u64>,
        #[arg(long)]
        save_screenshot: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
}
fn usage_error(message: impl std::fmt::Display) -> ! {
    eprintln!("error: {message}");
    std::process::exit(64);
}

fn resolve_endpoint(endpoint: &str) -> std::result::Result<Vec<SocketAddr>, String> {
    let addresses: Vec<_> = endpoint
        .to_socket_addrs()
        .map_err(|e| format!("invalid --endpoint: {e}"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !address.ip().is_loopback()) {
        return Err("--endpoint must resolve exclusively to loopback addresses".into());
    }
    Ok(addresses)
}

fn command_args(
    action: &str,
    args: Option<&str>,
    target: Option<&str>,
) -> std::result::Result<Value, String> {
    let mut args = args
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|e| format!("invalid --args JSON: {e}"))?
        .unwrap_or(json!({}));
    if let Some(target) = target {
        if !matches!(action, "read" | "wait_for") {
            return Err("--target is only supported for read/wait_for".into());
        }
        if !matches!(target, "foreground" | "system") {
            return Err("--target must be foreground or system".into());
        }
        let object = args
            .as_object_mut()
            .ok_or("--target requires --args to be a JSON object")?;
        if object.get("target").is_some_and(|value| value != target) {
            return Err("--target conflicts with target in --args".into());
        }
        object.insert("target".into(), json!(target));
    }
    Ok(args)
}

// Resolve and validate first; connect only to the checked loopback addresses.
async fn call(endpoint: &[SocketAddr], token: &str, body: &Value) -> Result<Value> {
    let bytes = serde_json::to_vec(body)?;
    let mut stream = TcpStream::connect(endpoint).await?;
    stream.write_all(format!("POST /v1/command HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await?;
    stream.write_all(&bytes).await?;
    let mut response = Vec::new();
    stream
        .take(32 * 1024 * 1024)
        .read_to_end(&mut response)
        .await?;
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("invalid HTTP response")?;
    let headers = std::str::from_utf8(&response[..split])?;
    if !headers.starts_with("HTTP/1.1 200 ") {
        return Err("HTTP command rejected".into());
    }
    let mut bytes = response[split + 4..].to_vec();
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut pos = 0;
        loop {
            let end = bytes[pos..]
                .windows(2)
                .position(|w| w == b"\r\n")
                .ok_or("bad chunk")?
                + pos;
            let n = usize::from_str_radix(
                std::str::from_utf8(&bytes[pos..end])?
                    .split(';')
                    .next()
                    .unwrap_or(""),
                16,
            )?;
            pos = end + 2;
            if n == 0 {
                break;
            }
            let chunk = bytes.get(pos..pos + n).ok_or("short chunk")?;
            decoded.extend_from_slice(chunk);
            pos += n + 2;
        }
        bytes = decoded;
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let cli = Cli::try_parse().unwrap_or_else(|error| {
        if error.use_stderr() {
            usage_error(error);
        }
        error.exit();
    });
    let dir = config::home()?;
    match cli.command {
        Commands::Pair {
            host,
            port,
            rotate,
            out,
        } => {
            let out = out.unwrap_or_else(|| dir.join("gateway-pair.json"));
            config::pair(&dir, rotate, host, port, &out)?;
            println!("{}", json!({"paired":true,"out":out}));
        }
        Commands::Serve { bind, http } => {
            Gateway::new(Config::load(&dir)?, Timing::default())
                .serve(bind, http)
                .await?
        }
        Commands::Cmd {
            action,
            endpoint,
            target,
            args,
            deadline_ms,
            save_screenshot,
            format,
        } => {
            let endpoint = endpoint.unwrap_or_else(|| {
                std::env::var("PHONE_GATEWAY_ENDPOINT").unwrap_or_else(|error| match error {
                    std::env::VarError::NotPresent => "127.0.0.1:7382".into(),
                    _ => usage_error("PHONE_GATEWAY_ENDPOINT is not valid UTF-8"),
                })
            });
            let endpoint = resolve_endpoint(&endpoint).unwrap_or_else(|error| usage_error(error));
            let args = command_args(&action, args.as_deref(), target.as_deref())
                .unwrap_or_else(|error| usage_error(error));
            let token = std::fs::read_to_string(dir.join("client-token"))?;
            // Validate before embedding in HTTP headers.
            if STANDARD.decode(token.trim())?.len() != 32 {
                return Err("invalid client token".into());
            }
            if !valid_phase_two_args(&action, &args) {
                let response = json!({"ok":false,"error":{"code":"invalid_args"}});
                println!("{}", render(&action, response, format, 0, None)?);
                return Ok(());
            }
            let body = json!({"action":action,"args":args,"deadline_ms":deadline_ms});
            let started = std::time::Instant::now();
            let response = call(&endpoint, token.trim(), &body).await?;
            let ms = started.elapsed().as_millis();
            println!(
                "{}",
                render(&action, response, format, ms, save_screenshot.as_deref())?
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_loopback_only() {
        for endpoint in [
            "127.0.0.1:0",
            "127.0.0.2:7382",
            "[::1]:7382",
            "localhost:7382",
        ] {
            assert!(resolve_endpoint(endpoint).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "0.0.0.0:7382",
            "192.0.2.1:7382",
            "[::]:7382",
            "[2001:db8::1]:7382",
            "127.0.0.1",
            "http://127.0.0.1:7382",
        ] {
            assert!(resolve_endpoint(endpoint).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn target_merges_without_overwriting_args() {
        assert_eq!(
            command_args("read", None, Some("foreground")).unwrap(),
            json!({"target":"foreground"})
        );
        assert_eq!(
            command_args(
                "wait_for",
                Some(r#"{"label":"Ready","target":"system"}"#),
                Some("system")
            )
            .unwrap(),
            json!({"label":"Ready","target":"system"})
        );
        assert_eq!(
            command_args("wait_for", Some(r#"{"label":"Ready"}"#), Some("foreground")).unwrap(),
            json!({"label":"Ready","target":"foreground"})
        );
        assert_eq!(
            command_args("read", Some(r#"{"target":"system"}"#), None).unwrap(),
            json!({"target":"system"})
        );
    }

    #[test]
    fn target_rejects_conflicts_and_unsupported_usage() {
        for (action, args, target) in [
            ("read", Some(r#"{"target":"foreground"}"#), "system"),
            ("read", Some(r#"{"target":null}"#), "system"),
            ("read", Some("[]"), "system"),
            ("read", Some("null"), "system"),
            ("read", None, "invalid"),
            ("status", None, "system"),
        ] {
            assert!(command_args(action, args, Some(target)).is_err());
        }
    }
}
