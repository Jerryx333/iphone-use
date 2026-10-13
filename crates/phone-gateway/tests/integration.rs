use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::BodyExt;
use phone_gateway::{
    config::{self, Config, Pairing},
    fake_phone,
    gateway::{Gateway, Timing},
    protocol,
};
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tower::ServiceExt;

struct Fixture {
    dir: PathBuf,
    config: Config,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("gateway-test-{}", uuid::Uuid::new_v4()));
        config::pair(
            &dir,
            false,
            "localhost".into(),
            7381,
            &dir.join("pair.json"),
        )
        .unwrap();
        let config = Config::load(&dir).unwrap();
        Self { dir, config }
    }
    fn gateway(&self) -> Gateway {
        Gateway::new(
            self.config.clone(),
            Timing {
                hello: Duration::from_millis(200),
                ping: Duration::from_millis(50),
                offline: Duration::from_millis(250),
                ..Timing::default()
            },
        )
    }
    async fn start(&self) -> (Gateway, Pairing, tokio::task::JoinHandle<()>) {
        let g = self.gateway();
        // Port sandbox errors intentionally fail this test; never skip or weaken it.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let p = self
            .config
            .pairing("127.0.0.1".into(), listener.local_addr().unwrap().port());
        let clone = g.clone();
        let task = tokio::spawn(async move {
            clone.listen(listener).await.unwrap();
        });
        (g, p, task)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
async fn http(g: &Gateway, token: &str, body: Value) -> Value {
    let response = g
        .router()
        .oneshot(
            Request::post("/v1/command")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
async fn wait_online(g: &Gateway, expected: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if g.snapshot().await["transport"] == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[test]
fn pair_permissions_reuse_rotate_and_no_secret_output() {
    let f = Fixture::new();
    assert_eq!(
        std::fs::metadata(&f.dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in [
        "server.crt",
        "server.key",
        "psk",
        "device_id",
        "client-token",
        "pair.json",
    ] {
        assert_eq!(
            std::fs::metadata(f.dir.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let first = f.config.pairing("host".into(), 7381);
    let same = config::pair(&f.dir, false, "host".into(), 7381, &f.dir.join("pair.json")).unwrap();
    assert_eq!(first.cert_sha256, same.cert_sha256);
    assert_eq!(first.psk, same.psk);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
        .env("PHONE_GATEWAY_HOME", &f.dir)
        .args(["pair"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains(&first.psk));
    assert!(!combined.contains(&f.config.token));
    let rotated =
        config::pair(&f.dir, true, "host".into(), 7381, &f.dir.join("pair.json")).unwrap();
    assert_ne!(first.cert_sha256, rotated.cert_sha256);
    assert_ne!(first.psk, rotated.psk);
}
#[test]
fn one_time_challenge_and_identity() {
    let f = Fixture::new();
    let p = f.config.pairing("host".into(), 1);
    let mut challenge = protocol::Challenge::new().unwrap();
    let hello = fake_phone::hello(&p, &challenge.frame()).unwrap();
    assert!(challenge.verify(&hello, &f.config.psk, &f.config.device_id));
    assert!(!challenge.verify(&hello, &f.config.psk, &f.config.device_id));
    let mut other = protocol::Challenge::new().unwrap();
    assert!(!other.verify(&hello, &f.config.psk, &f.config.device_id));
}
#[tokio::test]
async fn authorization_before_body_and_status_offline() {
    let f = Fixture::new();
    let g = f.gateway();
    for token in [None, Some("wrong")] {
        // A body that never produces even its first byte proves auth precedes polling.
        struct Never;
        impl http_body::Body for Never {
            type Data = axum::body::Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<
                Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>,
            > {
                panic!("unauthorized body was polled")
            }
        }
        let mut req = Request::post("/v1/command").header("Content-Length", "999999999");
        if let Some(token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let response = g
            .router()
            .oneshot(req.body(Body::new(Never)).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let response = g
        .router()
        .oneshot(
            Request::post("/v1/status")
                .header("Authorization", format!("Bearer {}", f.config.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["transport"], "offline");
    assert_eq!(
        g.router()
            .oneshot(Request::get("/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}
#[tokio::test]
async fn authentication_pin_limits_and_timeout() {
    let f = Fixture::new();
    let (g, p, task) = f.start().await;
    assert!(
        fake_phone::tls(&format!("{}:{}", p.host, p.port), "00".repeat(32))
            .await
            .is_err()
    );
    for kind in 0..4 {
        tokio::time::sleep(Duration::from_millis(110)).await;
        let mut stream = fake_phone::tls(&format!("{}:{}", p.host, p.port), p.cert_sha256.clone())
            .await
            .unwrap();
        let challenge = protocol::read_frame(&mut stream, 4096).await.unwrap();
        match kind {
            0 | 1 => {
                let mut bad = p.clone();
                if kind == 0 {
                    bad.psk = STANDARD.encode([0; 32]);
                } else {
                    bad.device_id = "00".repeat(16);
                }
                protocol::write_frame(&mut stream, &fake_phone::hello(&bad, &challenge).unwrap())
                    .await
                    .unwrap();
            }
            2 => {
                stream.write_u32(4097).await.unwrap();
            }
            _ => {
                tokio::time::sleep(Duration::from_millis(240)).await;
            }
        }
        assert!(tokio::time::timeout(
            Duration::from_secs(1),
            protocol::read_frame(&mut stream, 4096)
        )
        .await
        .unwrap()
        .is_err());
        assert_eq!(g.snapshot().await["transport"], "offline");
    }
    tokio::time::sleep(Duration::from_millis(110)).await;
    let mut stream = fake_phone::connect(&p).await.unwrap();
    wait_online(&g, "online").await;
    let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
    assert_eq!(frame["t"], "ping");
    protocol::write_frame(&mut stream, &json!({"t":"pong","seq":999999}))
        .await
        .unwrap();
    wait_online(&g, "offline").await;
    assert!(g.snapshot().await["last_pong_at"].is_null());
    task.abort();
}
#[tokio::test]
async fn roundtrip_duplicate_ids_disconnect_and_replacement() {
    let f = Fixture::new();
    let (g, p, task) = f.start().await;
    let mut stream = fake_phone::connect(&p).await.unwrap();
    wait_online(&g, "online").await;
    for _ in 0..2 {
        let clone = g.clone();
        let token = f.config.token.clone();
        let request = tokio::spawn(async move {
            http(
                &clone,
                &token,
                json!({"action":"read","id":"stable","deadline_ms":1000}),
            )
            .await
        });
        loop {
            let frame = protocol::read_frame(&mut stream, protocol::FRAME_LIMIT)
                .await
                .unwrap();
            if frame["t"] == "ping" {
                protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                    .await
                    .unwrap();
                continue;
            }
            assert_eq!(frame["id"], "stable");
            protocol::write_frame(
                &mut stream,
                &json!({"t":"res","id":"stable","ok":true,"result":{"nodes":[]}}),
            )
            .await
            .unwrap();
            break;
        }
        assert_eq!(request.await.unwrap()["ok"], true);
    }
    let clone = g.clone();
    let token = f.config.token.clone();
    let request = tokio::spawn(async move {
        http(
            &clone,
            &token,
            json!({"action":"tap","id":"inflight","deadline_ms":1000}),
        )
        .await
    });
    loop {
        let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
        if frame["t"] == "req" {
            break;
        }
    }
    // A wrong authenticated candidate cannot replace this transport.
    tokio::time::sleep(Duration::from_millis(110)).await;
    let mut bad = p.clone();
    bad.psk = STANDARD.encode([0; 32]);
    assert!(fake_phone::connect(&bad).await.is_err());
    assert_eq!(g.snapshot().await["transport"], "online");
    drop(stream);
    assert_eq!(request.await.unwrap()["error"]["code"], "unknown");
    wait_online(&g, "offline").await;
    tokio::time::sleep(Duration::from_millis(110)).await;
    let mut stream = fake_phone::connect(&p).await.unwrap();
    let clone = g.clone();
    let token = f.config.token.clone();
    let request = tokio::spawn(async move {
        http(&clone, &token, json!({"action":"tap","deadline_ms":1000})).await
    });
    loop {
        if protocol::read_frame(&mut stream, 4096).await.unwrap()["t"] == "req" {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(110)).await;
    let _replacement = fake_phone::connect(&p).await.unwrap();
    assert_eq!(request.await.unwrap()["error"]["code"], "unknown");
    task.abort();
}
#[tokio::test]
async fn fifo_capacity_and_queue_deadline() {
    let f = Fixture::new();
    let g = f.gateway();
    let mut requests = Vec::new();
    for i in 0..8 {
        let clone = g.clone();
        let token = f.config.token.clone();
        requests.push(tokio::spawn(async move {
            http(
                &clone,
                &token,
                json!({"action":"read","id":format!("q{i}"),"deadline_ms":400}),
            )
            .await
        }));
        tokio::time::timeout(Duration::from_secs(1), async {
            while g.snapshot().await["queue_length"] != i + 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(
        http(&g, &f.config.token, json!({"action":"read"})).await["error"]["code"],
        "queue_full"
    );
    for r in requests {
        assert_eq!(r.await.unwrap()["error"]["code"], "expired");
    }
    assert_eq!(g.snapshot().await["queue_length"], 0);
    assert_eq!(
        http(
            &g,
            &f.config.token,
            json!({"action":"read","deadline_ms":0})
        )
        .await["error"]["code"],
        "expired"
    );
}

#[tokio::test]
async fn matching_pong_refreshes_only_once() {
    let f = Fixture::new();
    let (g, p, task) = f.start().await;
    let mut stream = fake_phone::connect(&p).await.unwrap();
    wait_online(&g, "online").await;
    let ping = protocol::read_frame(&mut stream, 4096).await.unwrap();
    let pong = json!({"t":"pong","seq":ping["seq"]});
    protocol::write_frame(&mut stream, &pong).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while g.snapshot().await["last_pong_at"].is_null() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let acknowledged = g.snapshot().await["last_pong_at"].clone();
    tokio::time::sleep(Duration::from_millis(100)).await;
    protocol::write_frame(&mut stream, &pong).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(g.snapshot().await["last_pong_at"], acknowledged);
    wait_online(&g, "offline").await;
    task.abort();
}

#[tokio::test]
async fn authenticated_body_limit_and_post_only() {
    let f = Fixture::new();
    let g = f.gateway();
    let req = Request::post("/v1/command")
        .header("Authorization", format!("Bearer {}", f.config.token))
        .body(Body::from(vec![b'x'; 1024 * 1024 + 1]))
        .unwrap();
    assert_eq!(
        g.router().oneshot(req).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        g.router()
            .oneshot(Request::get("/v1/command").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn network_fifo_and_one_inflight() {
    let f = Fixture::new();
    let (g, p, task) = f.start().await;
    let mut stream = fake_phone::connect(&p).await.unwrap();
    wait_online(&g, "online").await;
    let mut requests = Vec::new();
    for i in 0..3 {
        let clone = g.clone();
        let token = f.config.token.clone();
        requests.push(tokio::spawn(async move {
            http(
                &clone,
                &token,
                json!({"action":"read","id":format!("fifo{i}"),"deadline_ms":2000}),
            )
            .await
        }));
        if i == 0 {
            loop {
                let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
                if frame["t"] == "ping" {
                    protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                        .await
                        .unwrap();
                    continue;
                }
                assert_eq!(frame["id"], "fifo0");
                break;
            }
        } else {
            tokio::time::timeout(Duration::from_secs(1), async {
                while g.snapshot().await["queue_length"] != i {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }
    // Only heartbeat can arrive while fifo0 is outstanding; respond to preserve liveness.
    let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
    assert_eq!(frame["t"], "ping");
    protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
        .await
        .unwrap();
    protocol::write_frame(&mut stream, &json!({"t":"res","id":"fifo0","ok":true}))
        .await
        .unwrap();
    for i in 1..3 {
        loop {
            let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
            if frame["t"] == "ping" {
                protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                    .await
                    .unwrap();
                continue;
            }
            assert_eq!(frame["id"], format!("fifo{i}"));
            protocol::write_frame(&mut stream, &json!({"t":"res","id":frame["id"],"ok":true}))
                .await
                .unwrap();
            break;
        }
    }
    for r in requests {
        assert_eq!(r.await.unwrap()["ok"], true);
    }
    task.abort();
}

#[tokio::test]
async fn real_http_tls_command_and_hello_replay() {
    use tokio::io::AsyncReadExt;
    let f = Fixture::new();
    let (g, p, phone_task) = f.start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = g.router();
    let http_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut stream = fake_phone::tls(&format!("{}:{}", p.host, p.port), p.cert_sha256.clone())
        .await
        .unwrap();
    let challenge = protocol::read_frame(&mut stream, 4096).await.unwrap();
    let hello = fake_phone::hello(&p, &challenge).unwrap();
    protocol::write_frame(&mut stream, &hello).await.unwrap();
    assert_eq!(
        protocol::read_frame(&mut stream, 4096).await.unwrap()["t"],
        "welcome"
    );
    wait_online(&g, "online").await;
    let token = f.config.token.clone();
    let request = tokio::spawn(async move {
        let mut http = tokio::net::TcpStream::connect(address).await.unwrap();
        let body = r#"{"action":"status","id":"wire","deadline_ms":2000}"#;
        http.write_all(format!("POST /v1/command HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        let mut response = String::new();
        http.read_to_string(&mut response).await.unwrap();
        response
    });
    loop {
        let frame = protocol::read_frame(&mut stream, 4096).await.unwrap();
        if frame["t"] == "ping" {
            protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                .await
                .unwrap();
            continue;
        }
        assert_eq!(frame["id"], "wire");
        protocol::write_frame(
            &mut stream,
            &json!({"t":"res","id":"wire","ok":true,"result":{"ready":true}}),
        )
        .await
        .unwrap();
        break;
    }
    let response = request.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("\"ready\":true"));
    // A second hello on the same connection cannot consume the challenge again.
    protocol::write_frame(&mut stream, &hello).await.unwrap();
    wait_online(&g, "offline").await;
    http_task.abort();
    phone_task.abort();
}

#[tokio::test]
async fn compact_cli_through_fake_phone() {
    let f = Fixture::new();
    let (g, p, phone_task) = f.start().await;
    // Use an ephemeral port; do not bypass sandbox listen failures.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let router = g.router();
    let http_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut stream = fake_phone::connect(&p).await.unwrap();
    wait_online(&g, "online").await;
    let shot = f.dir.join("screen.png");
    let cases = [
        (
            "read",
            json!({"pid":37,"snapshot_id":"snap","truncated":false,"nodes":[{"i":5,"type":9,"label":"O\u{200b}K\nnow","frame":{"x":1.5,"y":2,"w":3,"h":4}}]}),
        ),
        (
            "wait_for",
            json!({"found":true,"node":{"i":9,"type":48,"label":"Ready"}}),
        ),
        (
            "screenshot",
            json!({"png":STANDARD.encode(b"fake png"),"width":393,"height":852}),
        ),
        ("screenshot", Value::Null),
    ];
    for (action, result) in cases {
        let dir = f.dir.clone();
        let path = shot.clone();
        let cli = tokio::task::spawn_blocking(move || {
            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"));
            command
                .env("PHONE_GATEWAY_HOME", dir)
                .env("PHONE_GATEWAY_ENDPOINT", "192.0.2.1:7382")
                .args(["cmd", action, "--format", "compact", "--endpoint"])
                .arg(endpoint.to_string());
            if matches!(action, "read" | "wait_for") {
                command.args(["--target", "system"]);
            }
            if action == "wait_for" {
                command.args(["--args", r#"{"label":"Ready","target":"system"}"#]);
            }
            if action == "screenshot" {
                command.arg("--save-screenshot").arg(path);
            }
            command.output().unwrap()
        });
        loop {
            let frame = protocol::read_frame(&mut stream, protocol::FRAME_LIMIT)
                .await
                .unwrap();
            if frame["t"] == "ping" {
                protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                    .await
                    .unwrap();
                continue;
            }
            assert_eq!(frame["action"], action);
            if matches!(action, "read" | "wait_for") {
                assert_eq!(frame["args"]["target"], "system");
            }
            if action == "wait_for" {
                assert_eq!(frame["args"]["label"], "Ready");
            }
            let response = if result.is_null() {
                json!({"t":"res","id":frame["id"],"ok":false,"error":{"code":"expired"}})
            } else {
                json!({"t":"res","id":frame["id"],"ok":true,"result":result})
            };
            protocol::write_frame(&mut stream, &response).await.unwrap();
            break;
        }
        let output = cli.await.unwrap();
        assert!(output.status.success());
        let out = String::from_utf8(output.stdout).unwrap();
        match action {
            "read" => {
                assert!(out.starts_with("ok read "));
                assert!(out.contains("pid=37 snapshot=snap truncated=false nodes=1 shown=1"));
                assert!(out.contains("[5] Button \"OK now\" id= val=\"\" @2,2,3,4"));
            }
            "wait_for" => assert!(out.contains("[9] StaticText \"Ready\"")),
            _ if result.is_null() => assert!(out.starts_with("error expired ")),
            _ => {
                assert!(out.contains(&format!("saved={} 393x852", shot.display())));
                assert!(!out.contains(&STANDARD.encode(b"fake png")));
                assert_eq!(std::fs::read(&shot).unwrap(), b"fake png");
            }
        }
    }
    http_task.abort();
    phone_task.abort();
}

#[test]
fn cmd_usage_errors_exit_64_before_loading_token() {
    let missing_home =
        std::env::temp_dir().join(format!("gateway-missing-{}", uuid::Uuid::new_v4()));
    for args in [
        vec!["tap", "--then-read", "--format", "json"],
        vec!["tap", "--then-read"],
        vec!["read", "--then-read", "--format", "compact"],
        vec!["status", "--then-read", "--format", "compact"],
        vec!["screenshot", "--then-read", "--format", "compact"],
        vec!["wait_for", "--then-read", "--format", "compact"],
        vec!["unknown", "--then-read", "--format", "compact"],
        vec!["read", "--endpoint", "192.0.2.1:7382"],
        vec!["read", "--endpoint", "0.0.0.0:7382"],
        vec!["read", "--endpoint", "[::]:7382"],
        vec!["read", "--endpoint", "bad-address"],
        vec!["read", "--endpoint"],
        vec!["read", "--target", "invalid"],
        vec!["read", "--target"],
        vec!["status", "--target", "system"],
        vec![
            "read",
            "--target",
            "system",
            "--args",
            r#"{"target":"foreground"}"#,
        ],
        vec!["read", "--target", "system", "--args", "null"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
            .env("PHONE_GATEWAY_HOME", &missing_home)
            .env_remove("PHONE_GATEWAY_ENDPOINT")
            .arg("cmd")
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(64),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
        .env("PHONE_GATEWAY_HOME", &missing_home)
        .env("PHONE_GATEWAY_ENDPOINT", "192.0.2.1:7382")
        .args(["cmd", "read"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
}

#[test]
fn cmd_endpoint_env_and_default_without_network() {
    let f = Fixture::new();
    // Local invalid_args returns before connecting, after endpoint validation.
    for endpoint in [None, Some("127.0.0.1:0"), Some("[::1]:0")] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"));
        command
            .env("PHONE_GATEWAY_HOME", &f.dir)
            .env_remove("PHONE_GATEWAY_ENDPOINT");
        if let Some(endpoint) = endpoint {
            command.env("PHONE_GATEWAY_ENDPOINT", endpoint);
        }
        let output = command.args(["cmd", "type"]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["error"]["code"], "invalid_args");
    }
}

#[tokio::test]
async fn cmd_then_read_success_action_error_and_http_failure() {
    use axum::{middleware, response::IntoResponse};
    use std::sync::{Arc, Mutex};

    for (action_ok, read_http_failure) in [(true, false), (false, false), (true, true)] {
        let f = Fixture::new();
        let (g, p, phone_task) = f.start().await;
        let mut stream = fake_phone::connect(&p).await.unwrap();
        wait_online(&g, "online").await;
        // Record the CLI request before the gateway subtracts queue time from the deadline.
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let router = g.router().layer(middleware::from_fn(
            move |request: Request<Body>, next: middleware::Next| {
                let recorded = recorded.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let bytes = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
                    let value: Value = serde_json::from_slice(&bytes).unwrap();
                    recorded
                        .lock()
                        .unwrap()
                        .push((value.clone(), std::time::Instant::now()));
                    if read_http_failure && value["action"] == "read" {
                        // A 200 HTTP response with invalid JSON makes call return Err.
                        return "invalid JSON".into_response();
                    }
                    next.run(Request::from_parts(parts, Body::from(bytes)))
                        .await
                }
            },
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let http_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let phone = tokio::spawn(async move {
            let mut received = Vec::new();
            loop {
                let frame = protocol::read_frame(&mut stream, protocol::FRAME_LIMIT)
                    .await
                    .unwrap();
                if frame["t"] == "ping" {
                    protocol::write_frame(&mut stream, &json!({"t":"pong","seq":frame["seq"]}))
                        .await
                        .unwrap();
                    continue;
                }
                let is_read = frame["action"] == "read";
                assert_eq!(
                    frame["action"],
                    if received.is_empty() { "tap" } else { "read" }
                );
                if is_read {
                    assert_eq!(frame["args"], json!({}));
                    let deadline = frame["deadline_ms"].as_u64().unwrap();
                    assert!((1..=8000).contains(&deadline));
                }
                let response = if is_read {
                    json!({"t":"res","id":frame["id"],"ok":true,"result":{
                        "pid":37,"snapshot_id":"after","truncated":false,
                        "nodes":[{"i":5,"type":9,"label":"Ready","frame":{"x":1,"y":2,"w":3,"h":4}}]
                    }})
                } else if action_ok {
                    json!({"t":"res","id":frame["id"],"ok":true})
                } else {
                    json!({"t":"res","id":frame["id"],"ok":false,"error":{"code":"no_effect"}})
                };
                protocol::write_frame(&mut stream, &response).await.unwrap();
                received.push(frame);
                if is_read || read_http_failure {
                    return received;
                }
            }
        });
        let dir = f.dir.clone();
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
                    .env("PHONE_GATEWAY_HOME", dir)
                    .env_remove("PHONE_GATEWAY_ENDPOINT")
                    .args([
                        "cmd",
                        "tap",
                        "--args",
                        r#"{"label":"Go"}"#,
                        "--format",
                        "compact",
                        "--then-read",
                        "--endpoint",
                    ])
                    .arg(endpoint.to_string())
                    .output()
                    .unwrap()
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let out = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = out.lines().collect();
        assert!(lines[0].starts_with(if action_ok {
            "ok tap "
        } else {
            "error no_effect "
        }));
        assert!(lines[0].ends_with("ms"));
        assert_eq!(lines[1], "--- screen after tap");
        if read_http_failure {
            assert_eq!(&lines[2..], &["error read_unavailable 0ms"]);
        } else {
            assert!(lines[2].starts_with("ok read "));
            assert!(lines[2].ends_with("ms"));
            assert_eq!(
                lines[3],
                "pid=37 snapshot=after truncated=false nodes=1 shown=1"
            );
            assert_eq!(lines[4], "[5] Button \"Ready\" id= val=\"\" @1,2,3,4");
            assert_eq!(lines.len(), 5);
        }
        let received = phone.await.unwrap();
        assert_eq!(received.len(), if read_http_failure { 1 } else { 2 });
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].0,
            json!({"action":"read","args":{},"deadline_ms":8000})
        );
        assert!(requests[1].1.duration_since(requests[0].1) >= Duration::from_millis(400));
        http_task.abort();
        phone_task.abort();
    }
}

#[tokio::test]
async fn cmd_then_read_invalid_args_sends_no_requests() {
    let f = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
        .env("PHONE_GATEWAY_HOME", &f.dir)
        .env_remove("PHONE_GATEWAY_ENDPOINT")
        .args([
            "cmd",
            "type",
            "--then-read",
            "--format",
            "compact",
            "--endpoint",
        ])
        .arg(endpoint.to_string())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"error invalid_args 0ms\n");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}
