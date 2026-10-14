//! Local relay tests: no real phone, external relay, or persistent keys.
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::SinkExt;
use phone_gateway::{
    config::{self, Config, Pairing},
    e2e, fake_phone,
    gateway::{Gateway, Timing},
    protocol,
};
use serde_json::json;
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};
use tokio_tungstenite::tungstenite::{Error, Message};

struct Fixture {
    gateway: Gateway,
    pair: Pairing,
    ws: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    timing: Timing,
    token: String,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("gateway-ws-{}", uuid::Uuid::new_v4()));
        config::pair(
            &dir,
            false,
            "localhost".into(),
            7381,
            &dir.join("pair.json"),
        )
        .unwrap();
        let config = Config::load(&dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        let lan = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = ws.local_addr().unwrap();
        let mut pair = config.pairing("127.0.0.1".into(), lan.local_addr().unwrap().port());
        pair.ws_url = Some(format!("ws://{addr}"));
        let timing = Timing {
            hello: Duration::from_millis(500),
            ..Timing::default()
        };
        let token = config.token.clone();
        let gateway = Gateway::new(config, timing.clone())
            .with_forwards(Some("127.0.0.1:0".parse().unwrap()), None)
            .unwrap();
        let g = gateway.clone();
        let a = tokio::spawn(async move {
            g.listen(lan).await.unwrap();
        });
        let g = gateway.clone();
        let b = tokio::spawn(async move {
            g.listen_ws(ws).await.unwrap();
        });
        Self {
            gateway,
            pair,
            ws: addr,
            tasks: vec![a, b],
            timing,
            token,
        }
    }
    fn url(&self) -> String {
        format!(
            "ws://{}/selfrunner/v1/{}",
            self.ws,
            e2e::route(&STANDARD.decode(&self.pair.psk).unwrap())
        )
    }
    async fn link(&self, expected: &str) {
        timeout(Duration::from_secs(2), async {
            loop {
                if self.gateway.snapshot().await["link"] == expected {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn control_commands_status_and_entry_replacement() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let f = Fixture::new().await;
    assert!(f.gateway.snapshot().await["link"].is_null());
    let mut ws = fake_phone::ws(&f.pair, f.timing.clone(), false)
        .await
        .unwrap();
    f.link("wss").await;
    let g = f.gateway.clone();
    let status = g
        .router()
        .oneshot(
            Request::post("/v1/status")
                .header("Authorization", format!("Bearer {}", f.token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), 200);
    let status: serde_json::Value =
        serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(status["link"], "wss");
    let phone = tokio::spawn(async move {
        loop {
            let frame = protocol::read_frame(&mut ws, protocol::FRAME_LIMIT)
                .await
                .unwrap();
            match frame["t"].as_str() {
                Some("ping") => {
                    protocol::write_frame(&mut ws, &json!({"t":"pong","seq":frame["seq"]}))
                        .await
                        .unwrap()
                }
                Some("req") => {
                    protocol::write_frame(
                        &mut ws,
                        &json!({"t":"res","id":frame["id"],"ok":true,"result":{"via":"ws"}}),
                    )
                    .await
                    .unwrap();
                    break;
                }
                _ => panic!("unexpected frame {frame}"),
            }
        }
    });
    let response = g
        .router()
        .oneshot(
            Request::post("/v1/command")
                .header("Authorization", format!("Bearer {}", f.token))
                .body(Body::from(json!({"action":"read","args":{}}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let response: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    phone.await.unwrap();
    assert_eq!(response["result"]["via"], "ws", "{response}");
    let lan = fake_phone::connect(&f.pair).await.unwrap();
    f.link("lan").await;
    drop(lan);
    let ws = fake_phone::ws(&f.pair, f.timing.clone(), false)
        .await
        .unwrap();
    f.link("wss").await;
    drop(ws);
    timeout(Duration::from_secs(2), async {
        while !f.gateway.snapshot().await["link"].is_null() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn ws_tunnel_large_roundtrip_and_half_close() {
    let mut f = Fixture::new().await;
    let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = local.local_addr().unwrap();
    let g = f.gateway.clone();
    f.tasks.push(tokio::spawn(async move {
        g.listen_forward(local, 8100).await.unwrap();
    }));
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let payload: Vec<u8> = (0..1_200_000).map(|i| (i % 251) as u8).collect();
    let expected = payload.clone();
    f.tasks.push(tokio::spawn(async move {
        let (mut s, _) = target.accept().await.unwrap();
        let mut b = vec![];
        s.read_to_end(&mut b).await.unwrap();
        assert_eq!(b, expected);
        s.write_all(&b).await.unwrap();
        s.shutdown().await.unwrap();
    }));
    let tunnel = fake_phone::ws(&f.pair, f.timing.clone(), true)
        .await
        .unwrap();
    let mut options = fake_phone::TunnelOptions::default();
    options.targets.insert(8100, target_addr);
    let phone = tokio::spawn(async move {
        fake_phone::tunnel(tunnel, options).await.unwrap();
    });
    timeout(Duration::from_secs(10), async {
        let mut s = TcpStream::connect(local_addr).await.unwrap();
        s.write_all(&payload).await.unwrap();
        s.shutdown().await.unwrap();
        let mut received = vec![];
        s.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, payload);
        phone.await.unwrap();
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn wrong_psk_is_rejected_by_e2e_before_hello() {
    let f = Fixture::new().await;
    let (ws, _) = tokio_tungstenite::client_async(f.url(), TcpStream::connect(f.ws).await.unwrap())
        .await
        .unwrap();
    let mut stream = e2e::handshake(ws, &[0; 32], false, f.timing.clone())
        .await
        .unwrap();
    assert!(protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT)
        .await
        .is_err());
}
#[tokio::test]
async fn wrong_path_and_non_get_are_404_and_full_handshake_slots_503() {
    let f = Fixture::new().await;
    let wrong = tokio_tungstenite::client_async(
        format!("ws://{}/wrong", f.ws),
        TcpStream::connect(f.ws).await.unwrap(),
    )
    .await
    .unwrap_err();
    assert!(matches!(wrong,Error::Http(r) if r.status()==404));
    let mut tcp = TcpStream::connect(f.ws).await.unwrap();
    tcp.write_all(
        format!(
            "POST /selfrunner/v1/{} HTTP/1.1\r\nHost: localhost\r\n\r\n",
            e2e::route(&STANDARD.decode(&f.pair.psk).unwrap())
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut b = vec![];
    tcp.read_to_end(&mut b).await.unwrap();
    assert!(b.starts_with(b"HTTP/1.1 404"));
    let mut holding = vec![];
    for _ in 0..4 {
        holding.push(
            tokio_tungstenite::client_async(f.url(), TcpStream::connect(f.ws).await.unwrap())
                .await
                .unwrap()
                .0,
        );
    }
    let full = tokio_tungstenite::client_async(f.url(), TcpStream::connect(f.ws).await.unwrap())
        .await
        .unwrap_err();
    assert!(matches!(full,Error::Http(r) if r.status()==503));
    // Wrong paths do not need a handshake permit.
    let wrong = tokio_tungstenite::client_async(
        format!("ws://{}/wrong", f.ws),
        TcpStream::connect(f.ws).await.unwrap(),
    )
    .await
    .unwrap_err();
    assert!(matches!(wrong,Error::Http(r) if r.status()==404));
    for ws in &mut holding {
        let _ = ws.send(Message::Close(None)).await;
    }
}
#[tokio::test]
async fn preupgrade_connection_header_and_timeout_limits() {
    let f = Fixture::new().await;
    let mut holding = vec![];
    for _ in 0..16 {
        holding.push(TcpStream::connect(f.ws).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut extra = TcpStream::connect(f.ws).await.unwrap();
    let mut b = [0];
    assert!(timeout(Duration::from_millis(500), extra.read(&mut b))
        .await
        .unwrap()
        .is_ok_and(|n| n == 0));
    drop(holding);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut large = TcpStream::connect(f.ws).await.unwrap();
    large.write_all(&vec![b'x'; 8193]).await.unwrap();
    assert!(timeout(Duration::from_secs(1), large.read(&mut b))
        .await
        .unwrap()
        .map_or(true, |n| n == 0));
    let mut slow = TcpStream::connect(f.ws).await.unwrap();
    slow.write_all(b"GET ").await.unwrap();
    assert_eq!(
        timeout(Duration::from_secs(4), slow.read(&mut b))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
#[test]
fn cli_ws_bind_and_pair_url_usage_errors() {
    for args in [
        vec!["serve", "--ws-bind", "0.0.0.0:7383"],
        vec!["serve", "--ws-bind", "[::]:7383"],
        vec!["pair", "--ws-url", "wss://host/path"],
    ] {
        let dir = std::env::temp_dir().join(format!("gateway-cli-{}", uuid::Uuid::new_v4()));
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
            .args(args)
            .env("PHONE_GATEWAY_HOME", &dir)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(64));
        assert!(!dir.exists());
    }
}
#[test]
fn pairing_url_grammar_and_legacy_serialization() {
    for url in [
        "wss://phone.example",
        "wss://phone.example:443/",
        "ws://localhost",
        "ws://127.0.0.1:7383",
    ] {
        assert!(config::validate_ws_url(url).is_ok());
    }
    for url in [
        "ws://example",
        "wss://host/path",
        "wss://u@host",
        "wss://host?x",
        "wss://host#x",
        "wss://host//",
        "wss://host:65536",
        "wss://host:",
        "https://host",
        "wss://[::1]",
        "wss://é",
    ] {
        assert!(config::validate_ws_url(url).is_err(), "{url}");
    }
    assert!(config::validate_ws_url(&format!("wss://{}", "a".repeat(254))).is_err());
    let old = r#"{"v":1,"host":"localhost","port":7381,"cert_sha256":"hash","psk":"psk","device_id":"id"}"#;
    let mut pair: Pairing = serde_json::from_str(old).unwrap();
    assert_eq!(serde_json::to_string(&pair).unwrap(), old);
    pair.ws_url = Some("wss://phone.example".into());
    assert_eq!(
        serde_json::to_value(pair).unwrap()["ws_url"],
        "wss://phone.example"
    );
}
