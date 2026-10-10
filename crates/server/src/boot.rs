//! Building and running the daemon: the shared `AppState` both entry points
//! use, and the embedded serve loop the device runner starts on the phone
//! itself (see `crates/ios-embed`).

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::config::Config;
use crate::http::{self, AppState};

/// What `app_state` needs beyond the config.
pub struct StateInputs<'a> {
    pub cfg: &'a Config,
    pub secret: Vec<u8>,
    pub wda_url: &'a str,
    pub mjpeg_url: String,
    pub managed_wda: bool,
    pub managed_wda_pending: bool,
}

/// The daemon's state, as `iphone-use serve` and the embedded daemon build it.
pub fn app_state(inputs: StateInputs<'_>) -> Arc<AppState> {
    Arc::new(AppState {
        password: inputs.cfg.password.clone(),
        secret: inputs.secret,
        session_ttl_secs: inputs.cfg.session_ttl_secs,
        // The daemon always serves plain HTTP; whether the cookie is marked
        // `Secure` is decided per-request from `X-Forwarded-Proto` (the
        // Cloudflare tunnel sets it to https). Binding to a LAN IP (0.0.0.0) is
        // still plain HTTP, so forcing Secure here would make browsers reject
        // the cookie over LAN. Keep this `false`; per-request HTTPS is detected
        // in http.rs.
        cookie_secure: false,
        auth_limiter: Arc::new(Mutex::new(http::AuthLimiter::new())),
        agent_token: inputs.cfg.agent_token.clone(),
        device_udid: inputs.cfg.device_udid.clone(),
        inbox: std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        wda: match crate::wda::WdaClient::new(inputs.wda_url) {
            Ok(c) => {
                tracing::info!("device control via WDA at {}", inputs.wda_url);
                Some(Arc::new(tokio::sync::Mutex::new(c)))
            }
            Err(e) => {
                tracing::warn!("PHONE_REMOTE_WDA_URL set but client failed: {e:#}");
                None
            }
        },
        managed_wda: inputs.managed_wda,
        managed_wda_pending: inputs.managed_wda_pending,
        latest_release: Arc::new(Mutex::new(None)),
        // WDA's MJPEG stream (see /agent/mjpeg); the relay forwards it to
        // 127.0.0.1:9100 (override with PHONE_REMOTE_WDA_MJPEG_URL).
        video: crate::video::VideoHub::supported()
            .then(|| crate::video::VideoHub::new(inputs.mjpeg_url.clone())),
        mjpeg_url: Some(inputs.mjpeg_url),
        wda_actionable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        wda_read_degraded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        pairing: Arc::new(
            crate::pairing::Pairing::new(!crate::pairing::is_loopback_host(&inputs.cfg.host))
                .with_port(inputs.cfg.port),
        ),
        wda_health: Arc::new(Mutex::new(crate::wda::WdaHealth::down())),
        wda_death: Arc::new(Mutex::new(Default::default())),
        wda_health_probe: Arc::new(Mutex::new(None)),
        settled_frame_prefetch: Arc::new(Mutex::new(None)),
        wda_control_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        // Idle auto-release: start the clock now so a daemon that boots with no
        // one driving releases the phone after the first idle window.
        last_activity: Arc::new(Mutex::new(std::time::Instant::now())),
        released: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        wda_lifecycle: Arc::new(http::WdaLifecycle::new()),
        live_streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        mjpeg_stream_activity: Arc::new(Mutex::new(std::collections::HashMap::new())),
        element_snapshots: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        owner_snapshots: Arc::new(Mutex::new(std::collections::HashMap::new())),
        flow_trail: Arc::new(Mutex::new(crate::flows::FlowTrail::default())),
        agent_focus: Arc::new(Mutex::new(crate::focus::AgentFocus::load(
            crate::focus::state_path(),
        ))),
        hold_until: Arc::new(Mutex::new(None)),
        owner: Arc::new(Mutex::new(None)),
        owner_lease_secs: inputs.cfg.owner_lease_secs,
        wda_bootstrap: crate::http::write_and_bootstrap_wda_agent,
    })
}

/// How the device runner configures the daemon it hosts on the phone.
pub struct EmbeddedOptions {
    /// Address the daemon binds: the phone's own 127.0.0.1 unless a developer
    /// opens it up to reach it from a Mac (`/agent/*` still needs the token).
    pub host: String,
    pub port: u16,
    /// The runner's command and MJPEG ports, on the same phone.
    pub runner_port: u16,
    pub runner_mjpeg_port: u16,
    /// The runner's per-launch request-signing token, written where `WdaClient`
    /// reads it so the daemon's calls to its own runner are signed.
    pub runner_token: Option<String>,
    /// Bearer token for `/agent/*` (the on-phone app sends it).
    pub agent_token: Option<String>,
    /// Control password for the web page and `/login`.
    pub password: Option<String>,
}

/// The daemon as the device runner hosts it on the phone: same router and
/// state as `iphone-use serve`, bound to loopback, pointed at the runner it
/// lives in. Never manages that runner (no launchctl, no idle release: the
/// runner is the process this runs in). Blocks until the listener fails; call
/// it from a thread of its own, never the runner's main thread, which serves
/// every runner command the daemon will send.
pub fn serve_embedded(options: EmbeddedOptions) -> Result<()> {
    let port = options.port.to_string();
    let host = options.host.clone();
    let password = options.password.clone();
    let agent_token = options.agent_token.clone();
    let cfg = Config::from_map(|key| match key {
        "PHONE_REMOTE_HOST" => Some(host.clone()),
        "PHONE_REMOTE_PORT" => Some(port.clone()),
        "PHONE_REMOTE_PASSWORD" => password.clone(),
        "PHONE_REMOTE_AGENT_TOKEN" => agent_token.clone(),
        _ => std::env::var(key).ok(),
    });
    sandbox_dirs()?;
    let instance = crate::instance::install(
        crate::instance::Instance::from_env().map_err(|error| anyhow::anyhow!(error))?,
    );
    std::fs::create_dir_all(&instance.state_dir)
        .with_context(|| format!("create {}", instance.state_dir.display()))?;
    instance.verify_on_disk().map_err(|error| anyhow::anyhow!(error))?;
    if let Some(token) = &options.runner_token {
        crate::runner_token::write(&instance.state_dir, token).map_err(|error| anyhow::anyhow!(error))?;
    }
    crate::timing::set_log_path(instance.state_dir.join("agent-timing.jsonl"));
    crate::metrics::set_runs_log(instance.state_dir.join("agent-runs.jsonl"));

    let mut secret = vec![0_u8; 32];
    getrandom::fill(&mut secret).map_err(|error| anyhow::anyhow!("CSPRNG failed: {error}"))?;
    let wda_url = format!("http://127.0.0.1:{}", options.runner_port);
    let state = app_state(StateInputs {
        cfg: &cfg,
        secret,
        wda_url: &wda_url,
        mjpeg_url: format!("http://127.0.0.1:{}", options.runner_mjpeg_port),
        managed_wda: false,
        managed_wda_pending: false,
    });

    // The runner mints pairing codes for the iPhone app on this same phone
    // through `embedded_pair_code`, in-process: the app is a loopback client,
    // so this works although the daemon is not LAN-reachable.
    let _ = EMBEDDED_PAIRING.set(state.pairing.clone());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind((cfg.host.as_str(), cfg.port))
            .await
            .with_context(|| format!("bind {}:{}", cfg.host, cfg.port))?;
        tracing::info!("embedded daemon on {}:{} → runner {wda_url}", cfg.host, cfg.port);
        crate::schedules::start(
            "127.0.0.1",
            cfg.port,
            cfg.agent_token.clone().or_else(|| cfg.password.clone()),
            &instance.state_dir,
        );
        crate::keep_awake::spawn(state.clone());
        crate::lock_readiness::spawn(state.clone());
        axum::serve(listener, http::router(state))
            .await
            .context("axum serve")
    })
}

/// The embedded daemon's pairing, once it is built.
static EMBEDDED_PAIRING: std::sync::OnceLock<Arc<crate::pairing::Pairing>> =
    std::sync::OnceLock::new();

/// A fresh one-time pairing code from the daemon the runner hosts, for the
/// iPhone app on the same phone (`iphoneuse://pair?u=http://127.0.0.1:<port>&c=<code>`).
/// `None` until [`serve_embedded`] has built its state.
pub fn embedded_pair_code() -> Option<String> {
    EMBEDDED_PAIRING.get().map(|pairing| pairing.issue())
}

/// An iOS app container's root is not writable, so `~/.iphone-use` cannot be
/// created there: the state and the flow store go under the container's
/// `Library/Application Support` instead, unless already set.
fn sandbox_dirs() -> Result<()> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    let support = std::path::Path::new(&home).join("Library/Application Support/iphone-use");
    for (key, dir) in [
        ("PHONE_REMOTE_STATE_DIR", support.join("state")),
        ("IPHONE_USE_FLOWS_DIR", support.join("flows")),
    ] {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            continue;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        // Set once, on the daemon's thread before it starts any other.
        std::env::set_var(key, &dir);
    }
    Ok(())
}
