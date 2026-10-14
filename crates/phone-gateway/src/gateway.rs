use crate::{
    config::Config,
    protocol::{self, Challenge},
    Result,
};
use axum::{
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, Mutex, Notify, Semaphore},
    time::{timeout, Instant},
};
use tokio_rustls::TlsAcceptor;

#[derive(Clone)]
pub struct Timing {
    pub hello: Duration,
    pub ws_ping: Duration,
    pub ws_write: Duration,
    pub ping: Duration,
    pub offline: Duration,
    pub tunnel_open: Duration,
    pub tunnel_wait: Duration,
    pub tunnel_idle: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(5),
            ws_ping: Duration::from_secs(30),
            ws_write: Duration::from_secs(30),
            ping: Duration::from_secs(15),
            offline: Duration::from_secs(45),
            tunnel_open: Duration::from_secs(2),
            tunnel_wait: Duration::from_secs(5),
            tunnel_idle: Duration::from_secs(120),
        }
    }
}
struct Pending {
    ticket: uuid::Uuid,
    id: String,
    action: String,
    args: Value,
    deadline: Instant,
    reply: oneshot::Sender<Value>,
}
#[derive(Default)]
struct Inner {
    generation: u64,
    online: bool,
    link: Option<&'static str>,
    queue: VecDeque<Pending>,
    inflight: Option<Pending>,
    last_pong: Option<u128>,
}
type PasscodeReader = dyn Fn(&str) -> std::result::Result<String, ()> + Send + Sync;

// None means the listener cannot recover; Some is the delay before retrying.
fn accept_retry_delay(error: &io::Error) -> Option<Duration> {
    match error.kind() {
        io::ErrorKind::ConnectionAborted | io::ErrorKind::Interrupted => {
            return Some(Duration::ZERO);
        }
        io::ErrorKind::WouldBlock | io::ErrorKind::OutOfMemory => {
            return Some(Duration::from_millis(100));
        }
        _ => {}
    }
    // No libc dependency: ENOMEM=12, ENFILE=23, EMFILE=24 on macOS and Linux.
    // ENOBUFS differs: 55 on macOS, 105 on Linux (including Android).
    #[cfg(target_vendor = "apple")]
    let resource_errors = [12, 23, 24, 55];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let resource_errors = [12, 23, 24, 105];
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    let resource_errors: [i32; 0] = [];
    error
        .raw_os_error()
        .filter(|code| resource_errors.contains(code))
        .map(|_| Duration::from_millis(100))
}

#[derive(Clone)]
pub struct Gateway {
    config: Arc<Config>,
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    timing: Timing,
    passcode_reader: Arc<PasscodeReader>,
    tunnels: crate::tunnel::Tunnels,
    forwards: [Option<SocketAddr>; 2],
    ws_bind: Option<SocketAddr>,
}
// Never propagate security's stdout/stderr or an OS error: either can contain secrets.
fn keychain_passcode(device_id: &str) -> std::result::Result<String, ()> {
    let mut output = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "phone-gateway.passcode",
            "-a",
            device_id,
            "-w",
        ])
        .output()
        .map_err(|_| ())?;
    let result = if output.status.success() {
        // security -w appends one newline; other whitespace is an invalid password.
        let bytes = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        // Non-UTF8 contents are still a successful lookup, but fail digit validation.
        Ok(String::from_utf8_lossy(bytes).into_owned())
    } else {
        Err(())
    };
    output.stdout.fill(0);
    output.stderr.fill(0);
    result
}
fn error(id: &str, code: &str) -> Value {
    json!({"id":id,"ok":false,"error":{"code":code}})
}
// Unlock has a deliberately closed response schema; never forward phone text or AX nodes.
fn unlock_response(id: &str, frame: &Value) -> Value {
    if frame["ok"] == true {
        for key in ["unlocked", "already_unlocked"] {
            if frame["result"][key] == true {
                return json!({"id":id,"ok":true,"result":{key:true}});
            }
        }
    }
    let code = frame["error"]["code"]
        .as_str()
        .filter(|code| {
            [
                "keypad_not_found",
                "unlock_failed",
                "unlock_unconfirmed",
                "invalid_args",
                "expired",
                "read_failed",
                "target_unknown",
                "unsupported_orientation",
                "press_failed",
                "swipe_failed",
                "tap_failed",
                "busy",
                "not_ready",
                "unsupported",
            ]
            .contains(code)
        })
        .unwrap_or("unlock_unconfirmed");
    let mut response = error(id, code);
    response["result"] = json!({"retry_allowed":false});
    response
}
impl Gateway {
    pub fn new(config: Config, timing: Timing) -> Self {
        Self::with_passcode_reader(config, timing, keychain_passcode)
    }
    /// Injectable secret source; failures deliberately carry no diagnostic text.
    pub fn with_passcode_reader(
        config: Config,
        timing: Timing,
        reader: impl Fn(&str) -> std::result::Result<String, ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            config: Arc::new(config),
            inner: Arc::new(Mutex::new(Inner::default())),
            notify: Arc::new(Notify::new()),
            timing,
            passcode_reader: Arc::new(reader),
            tunnels: crate::tunnel::Tunnels::default(),
            forwards: [None, None],
            ws_bind: None,
        }
    }
    pub fn with_ws_bind(mut self, bind: Option<SocketAddr>) -> Result<Self> {
        if bind.is_some_and(|a| !a.ip().is_loopback()) {
            return Err("WS must bind a loopback address".into());
        }
        self.ws_bind = bind;
        Ok(self)
    }
    pub fn with_forwards(
        mut self,
        runner: Option<SocketAddr>,
        mjpeg: Option<SocketAddr>,
    ) -> Result<Self> {
        crate::tunnel::validate_forwards(runner, mjpeg)?;
        self.forwards = [runner, mjpeg];
        Ok(self)
    }
    /// Listen on a pre-bound loopback socket using the configured runner or MJPEG port.
    pub async fn listen_forward(&self, listener: TcpListener, port: u16) -> Result<()> {
        if !listener.local_addr()?.ip().is_loopback()
            || !matches!(port, 8100 | 9100)
            || self.forwards[usize::from(port == 9100)].is_none()
        {
            return Err("forward listener rejected".into());
        }
        self.tunnels
            .listen(listener, port, self.timing.clone())
            .await
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/command", post(command))
            .route("/v1/status", post(status))
            .with_state(self.clone())
    }
    pub async fn serve(self, phone: SocketAddr, http: SocketAddr) -> Result<()> {
        if !http.ip().is_loopback() {
            return Err("HTTP must bind a loopback address".into());
        }
        let phone = TcpListener::bind(phone).await?;
        let http = TcpListener::bind(http).await?;
        let mut forwards = Vec::new();
        for (address, port) in self.forwards.into_iter().zip([8100, 9100]) {
            if let Some(address) = address {
                forwards.push((TcpListener::bind(address).await?, port));
            }
        }
        let forwarding = async {
            let mut tasks = tokio::task::JoinSet::new();
            for (listener, port) in forwards {
                let gateway = self.clone();
                tasks.spawn(async move { gateway.listen_forward(listener, port).await });
            }
            if let Some(result) = tasks.join_next().await {
                result??;
            } else {
                std::future::pending::<()>().await;
            }
            Ok::<(), crate::Error>(())
        };
        let websocket = async {
            if let Some(bind) = self.ws_bind {
                self.listen_ws(TcpListener::bind(bind).await?).await
            } else {
                std::future::pending::<Result<()>>().await
            }
        };
        tokio::try_join!(forwarding, websocket, self.listen(phone), async {
            axum::serve(http, self.router()).await.map_err(Into::into)
        })?;
        Ok(())
    }
    pub async fn listen(&self, listener: TcpListener) -> Result<()> {
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(
            vec![self.config.cert_der.clone().into()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(self.config.key_der.clone()).into(),
        )?;
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let slots = Arc::new(Semaphore::new(2));
        // Bound source tracking; throttle connection attempts, including failed authentication.
        let mut sources: HashMap<IpAddr, Instant> = HashMap::new();
        loop {
            let (tcp, peer) = match listener.accept().await {
                Ok(connection) => connection,
                Err(error) => {
                    let Some(delay) = accept_retry_delay(&error) else {
                        return Err(error.into());
                    };
                    tracing::warn!(%error, retry_ms = delay.as_millis() as u64, "phone accept failed; retrying");
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    continue;
                }
            };
            if sources
                .get(&peer.ip())
                .is_some_and(|t| t.elapsed() < Duration::from_millis(100))
            {
                continue;
            }
            if sources.len() >= 1024 {
                sources.retain(|_, t| t.elapsed() < Duration::from_secs(5));
                if sources.len() >= 1024 {
                    continue;
                }
            }
            sources.insert(peer.ip(), Instant::now());
            let Ok(permit) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let gateway = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let handshake = async {
                    let stream = acceptor.accept(tcp).await?;
                    gateway.authenticate(Box::new(stream)).await
                };
                let result = timeout(gateway.timing.hello, handshake).await;
                drop(permit);
                if let Ok(Ok((stream, tunnel))) = result {
                    gateway.established(stream, tunnel, "lan").await;
                }
            });
        }
    }
    async fn authenticate(
        &self,
        mut stream: crate::tunnel::BoxStream,
    ) -> Result<(crate::tunnel::BoxStream, bool)> {
        let mut challenge = Challenge::new()?;
        protocol::write_frame(&mut stream, &challenge.frame()).await?;
        let hello = protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?;
        let tunnel = hello.get("role").is_some_and(|role| role == "tunnel");
        if (tunnel && self.forwards.iter().all(Option::is_none))
            || !challenge.verify(&hello, &self.config.psk, &self.config.device_id)
        {
            return Err("authentication rejected".into());
        }
        protocol::write_frame(&mut stream, &json!({"t":"welcome"})).await?;
        Ok((stream, tunnel))
    }
    async fn established(
        &self,
        stream: crate::tunnel::BoxStream,
        tunnel: bool,
        link: &'static str,
    ) {
        if tunnel {
            self.tunnels.insert(stream, self.timing.tunnel_idle).await;
        } else {
            self.connected_link(stream, link).await;
        }
    }
    // tungstenite's header callback must return the full http::Response as its Err.
    #[allow(clippy::result_large_err)]
    pub async fn listen_ws(&self, listener: TcpListener) -> Result<()> {
        if !listener.local_addr()?.ip().is_loopback() {
            return Err("WS must bind a loopback address".into());
        }
        let pending = Arc::new(Semaphore::new(16));
        let slots = Arc::new(Semaphore::new(4));
        loop {
            let (mut tcp, _) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    if let Some(d) = accept_retry_delay(&e) {
                        tokio::time::sleep(d).await;
                        continue;
                    }
                    return Err(e.into());
                }
            };
            let Ok(pre) = pending.clone().try_acquire_owned() else {
                continue;
            };
            let gateway = self.clone();
            let slots = slots.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Read exactly the headers: never consume a coalesced first WS frame.
                let headers = timeout(Duration::from_secs(3), async {
                    let mut h = Vec::new();
                    loop {
                        if h.len() == 8192 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "headers too large",
                            ));
                        }
                        h.push(tcp.read_u8().await?);
                        if h.ends_with(b"\r\n\r\n") {
                            return Ok(h);
                        }
                    }
                })
                .await;
                let Ok(Ok(headers)) = headers else {
                    return;
                };
                let expected = format!("/selfrunner/v1/{}", crate::e2e::route(&gateway.config.psk));
                let line = headers.split(|b| *b == b'\r').next().unwrap_or_default();
                let parts: Vec<_> = line.split(|b| *b == b' ').collect();
                let correct = parts.len() == 3
                    && parts[0] == b"GET"
                    && parts[2] == b"HTTP/1.1"
                    && std::str::from_utf8(parts[1])
                        .is_ok_and(|p| protocol::token_matches(&expected, p));
                let header_text = String::from_utf8_lossy(&headers);
                let has_header = |name: &str, value: &str| {
                    header_text.split("\r\n").skip(1).any(|line| {
                        line.split_once(':').is_some_and(|(n, v)| {
                            n.eq_ignore_ascii_case(name)
                                && v.split(',').any(|v| v.trim().eq_ignore_ascii_case(value))
                        })
                    })
                };
                let upgrade = has_header("Upgrade", "websocket")
                    && has_header("Connection", "upgrade")
                    && has_header("Sec-WebSocket-Version", "13")
                    && header_text.split("\r\n").any(|line| {
                        line.split_once(':').is_some_and(|(n, v)| {
                            n.eq_ignore_ascii_case("Sec-WebSocket-Key") && !v.trim().is_empty()
                        })
                    });
                if !correct || !upgrade {
                    let _=tcp.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                }
                let Ok(permit) = slots.try_acquire_owned() else {
                    let _=tcp.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    return;
                };
                let replay = crate::e2e::Prefixed::new(headers, tcp);
                let ws = timeout(
                    Duration::from_secs(3),
                    tokio_tungstenite::accept_hdr_async_with_config(
                        replay,
                        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                         response| {
                            if req.method() == "GET"
                                && protocol::token_matches(&expected, &req.uri().to_string())
                            {
                                Ok(response)
                            } else {
                                Err(tokio_tungstenite::tungstenite::http::Response::builder()
                                    .status(404)
                                    .body(None)
                                    .unwrap())
                            }
                        },
                        Some(crate::e2e::ws_config()),
                    ),
                )
                .await;
                drop(pre);
                let Ok(Ok(ws)) = ws else {
                    return;
                };
                let handshake = async {
                    let stream = crate::e2e::handshake(
                        ws,
                        &gateway.config.psk,
                        true,
                        gateway.timing.clone(),
                    )
                    .await?;
                    gateway.authenticate(Box::new(stream)).await
                };
                let result = timeout(gateway.timing.hello, handshake).await;
                drop(permit);
                if let Ok(Ok((stream, tunnel))) = result {
                    gateway.established(stream, tunnel, "wss").await;
                }
            });
        }
    }
    #[cfg(test)]
    async fn connected<S>(&self, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        self.connected_link(stream, "lan").await;
    }
    async fn connected_link<S>(&self, stream: S, link: &'static str)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let generation = {
            let mut s = self.inner.lock().await;
            if let Some(p) = s.inflight.take() {
                let _ = p.reply.send(error(&p.id, "unknown"));
            }
            s.generation += 1;
            s.online = true;
            s.link = Some(link);
            s.last_pong = None;
            s.generation
        };
        self.notify.notify_waiters();
        let (mut reader, mut writer) = tokio::io::split(stream);
        let (tx, mut rx) = mpsc::channel(2);
        // Keep frame reads intact across select cancellation (partial length/data reads matter).
        let reading = tokio::spawn(async move {
            loop {
                let frame = protocol::read_frame(&mut reader, protocol::FRAME_LIMIT).await;
                let failed = frame.is_err();
                if tx.send(frame).await.is_err() || failed {
                    break;
                }
            }
        });
        let mut heartbeat = tokio::time::interval(self.timing.ping);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_ack = Instant::now();
        let mut sequence = 0u64;
        let mut outstanding = VecDeque::new();
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let dispatch = {
                let mut s = self.inner.lock().await;
                if s.generation != generation {
                    break;
                }
                if s.inflight.is_none() {
                    while s
                        .queue
                        .front()
                        .is_some_and(|p| p.deadline <= Instant::now())
                    {
                        let p = s.queue.pop_front().unwrap();
                        let _ = p.reply.send(error(&p.id, "expired"));
                    }
                    if let Some(mut p) = s.queue.pop_front() {
                        // The in-flight record retains identity, never the injected secret.
                        let args = std::mem::take(&mut p.args);
                        let req = json!({"t":"req","id":p.id,"action":p.action,"args":args,"deadline_ms":p.deadline.saturating_duration_since(Instant::now()).as_millis()});
                        s.inflight = Some(p);
                        Some(req)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(mut req) = dispatch {
                if req["action"] == "unlock" {
                    let deadline = {
                        let s = self.inner.lock().await;
                        if s.generation != generation {
                            break;
                        }
                        s.inflight.as_ref().unwrap().deadline
                    };
                    let injection =
                        tokio::time::timeout_at(deadline, self.inject_passcode(&mut req))
                            .await
                            .unwrap_or(Err("expired"));
                    let mut s = self.inner.lock().await;
                    if s.generation != generation {
                        break;
                    }
                    if let Err(code) = injection {
                        if let Some(p) = s.inflight.take() {
                            let _ = p.reply.send(error(&p.id, code));
                        }
                        continue;
                    }
                    if s.inflight
                        .as_ref()
                        .is_none_or(|p| p.deadline <= Instant::now())
                    {
                        drop(s);
                        let mut s = self.inner.lock().await;
                        if let Some(p) = s.inflight.take() {
                            let _ = p.reply.send(error(&p.id, "expired"));
                        }
                        continue;
                    }
                    // Keychain interaction consumes the same request deadline.
                    req["deadline_ms"] = json!(s
                        .inflight
                        .as_ref()
                        .unwrap()
                        .deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis());
                }
                if timeout(
                    (last_ack + self.timing.offline).saturating_duration_since(Instant::now()),
                    protocol::write_frame(&mut writer, &req),
                )
                .await
                .map_or(true, |r| r.is_err())
                {
                    break;
                }
            }
            // Frames already received come first: a response that arrives together with the close of
            // the connection (or a heartbeat tick whose write then fails) must still be delivered.
            tokio::select! {
                biased;
                frame = rx.recv() => {
                    let Some(Ok(frame)) = frame else { break; };
                    match frame["t"].as_str() {
                        Some("pong") => {
                            if let Some(seq) = frame["seq"].as_u64() {
                                if let Some(index) = outstanding.iter().position(|s| *s == seq) {
                                    outstanding.drain(..=index); last_ack = Instant::now();
                                    let mut s = self.inner.lock().await;
                                    if s.generation == generation { s.last_pong = Some(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()); }
                                }
                            }
                        },
                        Some("ping") => { if timeout((last_ack + self.timing.offline).saturating_duration_since(Instant::now()), protocol::write_frame(&mut writer, &json!({"t":"pong","seq":frame["seq"]}))).await.map_or(true, |r| r.is_err()) { break; } },
                        Some("res") => {
                            let mut s = self.inner.lock().await;
                            if s.generation != generation { break; }
                            if s.inflight.as_ref().is_some_and(|p| frame["id"] == p.id) {
                                let p = s.inflight.take().unwrap();
                                let mut response = if p.action == "unlock" { unlock_response(&p.id, &frame) } else { frame };
                                response["id"] = json!(p.id); let _ = p.reply.send(response);
                            }
                        },
                        Some("event") => {},
                        _ => break,
                    }
                },
                _ = notified => {},
                _ = tokio::time::sleep_until(last_ack + self.timing.offline) => break,
                _ = heartbeat.tick() => {
                    sequence += 1; outstanding.push_back(sequence);
                    if timeout((last_ack + self.timing.offline).saturating_duration_since(Instant::now()), protocol::write_frame(&mut writer, &json!({"t":"ping","seq":sequence}))).await.map_or(true, |r| r.is_err()) { break; }
                },
            }
        }
        // A write (heartbeat, request) can fail while the response to the in-flight request is still
        // on its way through the reader; give the reader a moment to deliver it before calling the
        // action unknown.
        let waiting = {
            let s = self.inner.lock().await;
            s.generation == generation && s.inflight.is_some()
        };
        if waiting {
            let late = timeout(Duration::from_millis(500), async {
                while let Some(Ok(frame)) = rx.recv().await {
                    if frame["t"] == "res" {
                        return Some(frame);
                    }
                }
                None
            })
            .await;
            if let Ok(Some(frame)) = late {
                let mut s = self.inner.lock().await;
                if s.generation == generation
                    && s.inflight.as_ref().is_some_and(|p| frame["id"] == p.id)
                {
                    let p = s.inflight.take().unwrap();
                    let mut response = if p.action == "unlock" {
                        unlock_response(&p.id, &frame)
                    } else {
                        frame
                    };
                    response["id"] = json!(p.id);
                    let _ = p.reply.send(response);
                }
            }
        }
        reading.abort();
        let mut s = self.inner.lock().await;
        if s.generation == generation {
            s.online = false;
            if let Some(p) = s.inflight.take() {
                let _ = p.reply.send(error(&p.id, "unknown"));
            }
        }
    }
    async fn inject_passcode(&self, req: &mut Value) -> std::result::Result<(), &'static str> {
        let reader = self.passcode_reader.clone();
        let device_id = self.config.device_id.clone();
        let secret = tokio::task::spawn_blocking(move || reader(&device_id)).await;
        let passcode = match secret {
            Ok(Ok(passcode)) => passcode,
            _ => return Err("passcode_unavailable"),
        };
        if !(4..=6).contains(&passcode.len()) || !passcode.bytes().all(|b| b.is_ascii_digit()) {
            return Err("passcode_invalid");
        }
        if req["args"].is_null() {
            req["args"] = json!({});
        }
        req["args"]["passcode"] = Value::String(passcode);
        Ok(())
    }
    pub async fn snapshot(&self) -> Value {
        let s = self.inner.lock().await;
        let tunnels = self.tunnels.snapshot().await;
        json!({"link":if s.online {s.link} else {None},"tunnels":tunnels,"transport":if s.online {"online"} else {"offline"},"last_pong_at":s.last_pong,"device_id":&self.config.device_id[..8],"queue_length":s.queue.len()})
    }
}
fn authorized(g: &Gateway, r: &Request) -> bool {
    r.headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|t| protocol::token_matches(&g.config.token, t))
}
async fn status(State(g): State<Gateway>, req: Request) -> Response {
    if !authorized(&g, &req) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(g.snapshot().await).into_response()
}
#[derive(Deserialize)]
struct Command {
    action: String,
    #[serde(default)]
    args: Value,
    deadline_ms: Option<u64>,
    id: Option<String>,
}
async fn command(State(g): State<Gateway>, req: Request) -> Response {
    let started = Instant::now();
    if !authorized(&g, &req) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(bytes) = to_bytes(req.into_body(), 1024 * 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(c) = serde_json::from_slice::<Command>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !valid_phase_two_args(&c.action, &c.args) {
        return Json(error(c.id.as_deref().unwrap_or(""), "invalid_args")).into_response();
    }
    // Bound log fields to the protocol action vocabulary; never log arbitrary caller text.
    if ![
        "status",
        "read",
        "screenshot",
        "tap",
        "type",
        "swipe",
        "press",
        "open_url",
        "launch",
        "wait_for",
        "unlock",
    ]
    .contains(&c.action.as_str())
    {
        return Json(error(c.id.as_deref().unwrap_or(""), "unsupported")).into_response();
    }
    let id = c.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(deadline) =
        started.checked_add(Duration::from_millis(c.deadline_ms.unwrap_or(30_000)))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (tx, mut rx) = oneshot::channel();
    let ticket = uuid::Uuid::new_v4();
    {
        let mut s = g.inner.lock().await;
        if deadline <= Instant::now() {
            return Json(error(&id, "expired")).into_response();
        }
        if s.queue.len() >= 8 {
            return Json(error(&id, "queue_full")).into_response();
        }
        s.queue.push_back(Pending {
            ticket,
            id: id.clone(),
            action: c.action.clone(),
            args: c.args,
            deadline,
            reply: tx,
        });
    }
    g.notify.notify_waiters();
    let result = tokio::select! {
        res = &mut rx => res.unwrap_or_else(|_| error(&id,"unknown")),
        _ = tokio::time::sleep_until(deadline) => {
            let mut s = g.inner.lock().await;
            // Compare reply channels rather than id: duplicate ids are deliberately forwarded.
            let index = s.queue.iter().position(|p| p.ticket == ticket);
            if let Some(index) = index { s.queue.remove(index); error(&id,"expired") }
            else { drop(s); rx.await.unwrap_or_else(|_| error(&id,"unknown")) }
        }
    };
    let code = if result["ok"] == true {
        "ok"
    } else {
        result["error"]["code"].as_str().unwrap_or("error")
    };
    // Phone-supplied error text is never logged.
    let code = log_result_code(code);
    tracing::info!(action = %c.action, id = %id, ms = started.elapsed().as_millis() as u64, code);
    Json(result).into_response()
}

// Screen bounds and registered URL schemes are device facts, checked on the phone.
pub fn valid_phase_two_args(action: &str, args: &Value) -> bool {
    // Applies to every action, including phase one; callers cannot supply secrets.
    if args.get("passcode").is_some() {
        return false;
    }
    let integer = |key: &str, default: u64, min: u64, max: u64| {
        args.get(key)
            .map_or(Some(default), Value::as_u64)
            .is_some_and(|n| (min..=max).contains(&n))
    };
    let text = |key: &str, max: usize| {
        args[key]
            .as_str()
            .is_some_and(|s| s.encode_utf16().count() <= max)
    };
    match action {
        "unlock" => {
            (args.is_null()
                || args
                    .as_object()
                    .is_some_and(|m| m.keys().all(|k| k == "timeout_ms")))
                && integer("timeout_ms", 4000, 0, 10000)
        }
        "type" => args.is_object() && text("text", 1000),
        "swipe" => {
            args.is_object()
                && ["from", "to"].iter().all(|key| {
                    args[*key].is_object()
                        && ["x", "y"].iter().all(|axis| {
                            args[*key][*axis]
                                .as_f64()
                                .is_some_and(|n| n.is_finite() && n >= 0.0)
                        })
                })
                && integer("duration_ms", 300, 50, 5000)
        }
        "press" => {
            matches!(
                args["button"].as_str(),
                Some("home" | "volume_up" | "volume_down" | "lock")
            ) && args.get("strategy").is_none_or(|strategy| {
                args["button"] == "lock"
                    && matches!(
                        strategy.as_str(),
                        Some("auto" | "shortcut" | "hid" | "device_event" | "press_lock")
                    )
            }) && args.get("shortcut_name").is_none_or(|name| {
                args["button"] == "lock"
                    && name
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.encode_utf16().count() <= 50)
            })
        }
        "launch" => args["bundle_id"].as_str().is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 255
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b))
        }),
        "open_url" => {
            text("url", 2048)
                && args["url"].as_str().is_some_and(|s| {
                    !s.chars().any(|c| c.is_whitespace() || c.is_control())
                        && s.split_once(':').is_some_and(|(scheme, _)| {
                            !scheme.is_empty()
                                && scheme.as_bytes()[0].is_ascii_alphabetic()
                                && scheme
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
                        })
                })
        }
        "wait_for" => {
            args.is_object()
                && (args.get("label").is_some() ^ args.get("identifier").is_some())
                && args
                    .get("label")
                    .or_else(|| args.get("identifier"))
                    .is_some_and(Value::is_string)
                && integer("timeout_ms", 5000, 0, 30000)
        }
        _ => true, // Preserve phase-one validation and behavior.
    }
}

// Keep phone-supplied text out of logs while retaining known action outcomes.
fn log_result_code(code: &str) -> &str {
    if [
        "ok",
        "unknown",
        "expired",
        "unsupported",
        "busy",
        "not_found",
        "ambiguous",
        "occluded",
        "no_effect",
        "invalid_args",
        "no_focus",
        "type_failed",
        "swipe_failed",
        "press_failed",
        "launch_failed",
        "open_url_failed",
        "foreground_unconfirmed",
        "passcode_unavailable",
        "passcode_invalid",
        "keypad_not_found",
        "shortcut_not_found",
        "unlock_failed",
        "unlock_unconfirmed",
    ]
    .contains(&code)
    {
        code
    } else {
        "error"
    }
}

#[cfg(test)]
mod accept_error_tests {
    use super::accept_retry_delay;
    use std::{io, time::Duration};

    #[test]
    fn transient_error_kinds_retry_with_the_expected_delay() {
        for kind in [io::ErrorKind::ConnectionAborted, io::ErrorKind::Interrupted] {
            assert_eq!(accept_retry_delay(&kind.into()), Some(Duration::ZERO));
        }
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::OutOfMemory] {
            assert_eq!(
                accept_retry_delay(&kind.into()),
                Some(Duration::from_millis(100))
            );
        }
    }

    #[test]
    fn unrecoverable_error_kinds_are_propagated() {
        for kind in [
            io::ErrorKind::InvalidInput,
            io::ErrorKind::NotConnected,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Unsupported,
            io::ErrorKind::Other,
        ] {
            assert_eq!(accept_retry_delay(&kind.into()), None);
        }
    }

    #[test]
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    fn raw_os_errors_distinguish_transient_failures_from_invalid_listeners() {
        #[cfg(target_vendor = "apple")]
        let (aborted, no_buffers, not_socket) = (53, 55, 38);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let (aborted, no_buffers, not_socket) = (103, 105, 88);
        for code in [4, aborted] {
            // EINTR, ECONNABORTED
            assert_eq!(
                accept_retry_delay(&io::Error::from_raw_os_error(code)),
                Some(Duration::ZERO)
            );
        }
        for code in [12, 23, 24, no_buffers] {
            // ENOMEM, ENFILE, EMFILE, ENOBUFS
            assert_eq!(
                accept_retry_delay(&io::Error::from_raw_os_error(code)),
                Some(Duration::from_millis(100))
            );
        }
        for code in [9, 22, not_socket] {
            // EBADF, EINVAL, ENOTSOCK
            assert_eq!(
                accept_retry_delay(&io::Error::from_raw_os_error(code)),
                None
            );
        }
    }
}

#[cfg(test)]
mod result_code_tests {
    #[test]
    fn tap_outcomes_are_logged_without_arbitrary_phone_text() {
        for code in ["occluded", "no_effect"] {
            assert_eq!(super::log_result_code(code), code);
        }
        assert_eq!(super::log_result_code("PRIVATE_SCREEN_CONTENT"), "error");
    }
}

#[cfg(test)]
mod phase_two_tests {
    use super::valid_phase_two_args as valid;
    use serde_json::json;

    #[test]
    fn text_limits_use_utf16_and_reject_non_strings() {
        assert!(valid("type", &json!({"text": "a".repeat(1000)})));
        assert!(valid("type", &json!({"text": "😀".repeat(500)})));
        assert!(!valid("type", &json!({"text": "😀".repeat(501)})));
        for text in [
            json!(null),
            json!(true),
            json!(123),
            json!("a".repeat(1001)),
        ] {
            assert!(!valid("type", &json!({"text": text})));
        }
    }
    #[test]
    fn swipe_coordinates_and_duration_boundaries() {
        for duration in [50, 300, 5000] {
            assert!(valid(
                "swipe",
                &json!({"from":{"x":0,"y":1.5},"to":{"x":100,"y":200},"duration_ms":duration})
            ));
        }
        for duration in [
            json!(49),
            json!(5001),
            json!(true),
            json!(50.5),
            json!(null),
        ] {
            assert!(!valid(
                "swipe",
                &json!({"from":{"x":0,"y":1},"to":{"x":100,"y":200},"duration_ms":duration})
            ));
        }
        assert!(valid(
            "swipe",
            &json!({"from":{"x":0,"y":1},"to":{"x":100,"y":200}})
        ));
        assert!(!valid(
            "swipe",
            &json!({"from":{"x":-1,"y":1},"to":{"x":100,"y":200}})
        ));
        assert!(!valid(
            "swipe",
            &json!({"from":{"x":true,"y":1},"to":{"x":100,"y":200}})
        ));
    }
    #[test]
    fn lock_strategies() {
        assert!(valid("press", &json!({"button":"lock"})));
        for strategy in ["auto", "shortcut", "hid", "device_event", "press_lock"] {
            assert!(valid(
                "press",
                &json!({"button":"lock", "strategy":strategy})
            ));
            for button in ["home", "volume_up", "volume_down"] {
                assert!(!valid(
                    "press",
                    &json!({"button":button, "strategy":strategy})
                ));
            }
        }
        for name in ["锁屏", "A&B +?#", &"锁".repeat(50)] {
            assert!(valid(
                "press",
                &json!({"button":"lock", "strategy":"shortcut", "shortcut_name":name})
            ));
            assert!(!valid(
                "press",
                &json!({"button":"home", "shortcut_name":name})
            ));
        }
        for name in [
            json!(null),
            json!(1),
            json!(""),
            json!("锁".repeat(51)),
            json!("😀".repeat(26)),
        ] {
            assert!(!valid(
                "press",
                &json!({"button":"lock", "shortcut_name":name})
            ));
        }
        for strategy in [
            json!(null),
            json!(true),
            json!(1),
            json!({}),
            json!([]),
            json!(""),
            json!("power"),
            json!("HID"),
        ] {
            assert!(!valid(
                "press",
                &json!({"button":"lock", "strategy":strategy})
            ));
        }
    }
    #[test]
    fn buttons_launch_urls_and_wait_selectors() {
        for button in ["home", "volume_up", "volume_down", "lock"] {
            assert!(valid("press", &json!({"button":button})));
        }
        assert!(!valid("press", &json!({"button":"power"})));
        assert!(valid(
            "launch",
            &json!({"bundle_id":"com.apple.Preferences"})
        ));
        assert!(!valid("launch", &json!({"bundle_id":""})));
        for url in ["https://example.com", "prefs:root=General", "my-app://path"] {
            assert!(valid("open_url", &json!({"url":url})));
        }
        for url in ["relative/path", "1bad:x", "https://example.com/\n", ""] {
            assert!(!valid("open_url", &json!({"url":url})));
        }
        assert!(!valid(
            "open_url",
            &json!({"url":format!("https://x/{}", "x".repeat(2048))})
        ));
        assert!(valid("wait_for", &json!({"label":"目标"})));
        assert!(valid(
            "wait_for",
            &json!({"identifier":"id", "timeout_ms":30000})
        ));
        for args in [
            json!({}),
            json!({"label":"a","identifier":"b"}),
            json!({"label":null}),
            json!({"label":"a","timeout_ms":30001}),
            json!({"label":"a","timeout_ms":true}),
        ] {
            assert!(!valid("wait_for", &args));
        }
        assert!(valid("read", &json!({"anything":"unchanged"})));
    }
}

#[cfg(test)]
mod unlock_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn gateway(
        reader: impl Fn(&str) -> std::result::Result<String, ()> + Send + Sync + 'static,
    ) -> Gateway {
        let dir = std::env::temp_dir().join(format!("unlock-test-{}", uuid::Uuid::new_v4()));
        crate::config::pair(
            &dir,
            false,
            "localhost".into(),
            7381,
            &dir.join("pair.json"),
        )
        .unwrap();
        let config = Config::load(&dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        Gateway::with_passcode_reader(config, Timing::default(), reader)
    }
    async fn call(g: &Gateway, args: Value) -> Value {
        let response = g.router().oneshot(Request::post("/v1/command")
            .header("Authorization", format!("Bearer {}", g.config.token))
            .body(Body::from(json!({"action":"unlock","id":"unlock-test","args":args,"deadline_ms":1000}).to_string())).unwrap()).await.unwrap();
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }
    #[tokio::test]
    async fn caller_passcode_and_extra_args_rejected_before_secret_read() {
        let g = gateway(|_| panic!("must not read secret for invalid args"));
        for args in [
            json!({"passcode":"2580"}),
            json!({"timeout_ms":1,"extra":true}),
            json!([]),
            json!({"timeout_ms":10001}),
            json!({"timeout_ms":true}),
        ] {
            assert_eq!(call(&g, args).await["error"]["code"], "invalid_args");
            assert_eq!(g.snapshot().await["queue_length"], 0);
        }
        assert!(!valid_phase_two_args("read", &json!({"passcode":"2580"})));
        assert!(valid_phase_two_args("unlock", &Value::Null));
        assert!(valid_phase_two_args("unlock", &json!({})));
        assert!(valid_phase_two_args("unlock", &json!({"timeout_ms":10000})));
    }
    #[tokio::test]
    async fn invalid_or_unavailable_passcode_is_never_sent() {
        for secret in [
            Err(()),
            Ok("123"),
            Ok("1234567"),
            Ok("12a4"),
            Ok("１２３４"),
            Ok("1234\n"),
            Ok("1234 "),
        ] {
            let expected = if secret.is_err() {
                "passcode_unavailable"
            } else {
                "passcode_invalid"
            };
            let g = gateway(move |_| secret.map(str::to_owned));
            let (server, mut phone) = tokio::io::duplex(4096);
            let clone = g.clone();
            let task = tokio::spawn(async move { clone.connected(server).await });
            assert_eq!(call(&g, json!({})).await["error"]["code"], expected);
            // A heartbeat is allowed; a req containing even an invalid secret is not.
            while let Ok(Ok(frame)) = timeout(
                Duration::from_millis(20),
                protocol::read_frame(&mut phone, 4096),
            )
            .await
            {
                assert_eq!(frame["t"], "ping");
            }
            assert!(g.inner.lock().await.inflight.is_none());
            task.abort();
        }
    }
    #[tokio::test]
    async fn valid_passcode_is_transient_and_response_is_closed() {
        for secret in ["2580", "25801", "258019"] {
            let g = gateway(move |_| Ok(secret.into()));
            let (server, mut phone) = tokio::io::duplex(4096);
            let clone = g.clone();
            let task = tokio::spawn(async move { clone.connected(server).await });
            let clone = g.clone();
            let request =
                tokio::spawn(async move { call(&clone, json!({"timeout_ms":1234})).await });
            loop {
                let frame = protocol::read_frame(&mut phone, 4096).await.unwrap();
                if frame["t"] == "ping" {
                    continue;
                }
                assert!(frame["args"]["passcode"].as_str() == Some(secret));
                assert_eq!(frame["args"]["timeout_ms"], 1234);
                assert!(g
                    .inner
                    .lock()
                    .await
                    .inflight
                    .as_ref()
                    .unwrap()
                    .args
                    .is_null());
                protocol::write_frame(&mut phone,&json!({"t":"res","id":"unlock-test","ok":false,"error":{"code":secret,"message":secret},"result":{"passcode":secret}})).await.unwrap();
                break;
            }
            let response = request.await.unwrap();
            assert!(!response.to_string().contains(secret));
            assert_eq!(response["error"]["code"], "unlock_unconfirmed");
            assert_eq!(response["result"]["retry_allowed"], false);
            task.abort();
        }
        assert_eq!(
            unlock_response(
                "id",
                &json!({"ok":true,"result":{"unlocked":true,"passcode":"2580"}})
            ),
            json!({"id":"id","ok":true,"result":{"unlocked":true}})
        );
    }
}
