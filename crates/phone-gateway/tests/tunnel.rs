use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use phone_gateway::{
    config::{self, Config, Pairing},
    fake_phone::{self, TunnelOptions},
    gateway::{Gateway, Timing},
    protocol,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::{sleep, timeout, Instant},
};
use tower::ServiceExt;

struct Fixture {
    config: Config,
    gateway: Gateway,
    pair: Pairing,
    forwards: [SocketAddr; 2],
    tasks: Vec<JoinHandle<()>>,
}
impl Fixture {
    async fn new(enabled: bool, idle: Duration) -> Self {
        let dir = std::env::temp_dir().join(format!("tunnel-test-{}", uuid::Uuid::new_v4()));
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
        let phone = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runner = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mjpeg = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let forwards = [runner.local_addr().unwrap(), mjpeg.local_addr().unwrap()];
        let pair = config.pairing("127.0.0.1".into(), phone.local_addr().unwrap().port());
        let gateway = Gateway::new(
            config.clone(),
            Timing {
                tunnel_open: Duration::from_millis(150),
                tunnel_wait: Duration::from_millis(450),
                tunnel_idle: idle,
                ..Timing::default()
            },
        )
        .with_forwards(
            enabled.then_some(forwards[0]),
            enabled.then_some(forwards[1]),
        )
        .unwrap();
        let g = gateway.clone();
        let mut tasks = vec![tokio::spawn(async move { g.listen(phone).await.unwrap() })];
        if enabled {
            for (listener, port) in [(runner, 8100), (mjpeg, 9100)] {
                let g = gateway.clone();
                tasks.push(tokio::spawn(async move {
                    g.listen_forward(listener, port).await.unwrap()
                }));
            }
        }
        Self {
            config,
            gateway,
            pair,
            forwards,
            tasks,
        }
    }
    async fn tunnel(&mut self, options: TunnelOptions) {
        // Preserve the production 100ms source throttle rather than bypassing it.
        sleep(Duration::from_millis(110)).await;
        let stream = fake_phone::connect_tunnel(&self.pair).await.unwrap();
        self.tasks.push(tokio::spawn(async move {
            let _ = fake_phone::tunnel(stream, options).await;
        }));
    }
    async fn count(&self, field: &str, expected: usize) {
        timeout(Duration::from_secs(2), async {
            loop {
                if self.gateway.snapshot().await["tunnels"][field] == expected {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn echo(&mut self) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        self.tasks.push(tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let (mut read, mut write) = stream.split();
                    let _ = tokio::io::copy(&mut read, &mut write).await;
                    let _ = write.shutdown().await;
                });
            }
        }));
        address
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
fn target(port: u16, address: SocketAddr) -> TunnelOptions {
    TunnelOptions {
        targets: [(port, address)].into(),
        ..Default::default()
    }
}
async fn roundtrip(address: SocketAddr, payload: Vec<u8>) {
    timeout(Duration::from_secs(5), async {
        let stream = TcpStream::connect(address).await.unwrap();
        let (mut read, mut write) = stream.into_split();
        let expected = payload.clone();
        let sending = async {
            write.write_all(&payload).await.unwrap();
            write.shutdown().await.unwrap();
        };
        let receiving = async {
            let mut result = Vec::new();
            read.read_to_end(&mut result).await.unwrap();
            assert_eq!(result, expected);
        };
        tokio::join!(sending, receiving);
    })
    .await
    .unwrap();
}
async fn closed(mut stream: TcpStream) {
    timeout(Duration::from_secs(2), async {
        let mut byte = [0];
        assert!(matches!(stream.read(&mut byte).await, Ok(0) | Err(_)));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn runner_and_mjpeg_small_large_and_half_close() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    let echo = f.echo().await;
    for (index, port) in [(0, 8100), (1, 9100)] {
        for size in [17, 1024 * 1024 + 73] {
            f.tunnel(target(port, echo)).await;
            f.count("idle", 1).await;
            roundtrip(
                f.forwards[index],
                (0..size).map(|i| (i % 251) as u8).collect(),
            )
            .await;
            f.count("active", 0).await;
        }
    }
}

#[tokio::test]
async fn control_connection_survives_tunnel_use() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    let mut control = fake_phone::connect(&f.pair).await.unwrap();
    let control_task = tokio::spawn(async move {
        loop {
            let frame = protocol::read_frame(&mut control, protocol::FRAME_LIMIT)
                .await
                .unwrap();
            let response = match frame["t"].as_str() {
                Some("ping") => json!({"t":"pong","seq":frame["seq"]}),
                Some("req") => {
                    json!({"t":"res","id":frame["id"],"ok":true,"result":{"same_control":true}})
                }
                _ => panic!("unexpected control frame"),
            };
            protocol::write_frame(&mut control, &response)
                .await
                .unwrap();
        }
    });
    f.tasks.push(control_task);
    let echo = f.echo().await;
    f.tunnel(target(8100, echo)).await;
    assert_eq!(f.gateway.snapshot().await["transport"], "online");
    roundtrip(f.forwards[0], b"control stays online".to_vec()).await;
    let response = f
        .gateway
        .router()
        .oneshot(
            Request::post("/v1/command")
                .header("Authorization", format!("Bearer {}", f.config.token))
                .body(Body::from(
                    json!({"action":"status","deadline_ms":1000}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let response: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(response["result"]["same_control"], true);
    assert_eq!(f.gateway.snapshot().await["transport"], "online");
}

#[tokio::test]
async fn authentication_context_role_and_disabled_rejected() {
    for enabled in [true, false] {
        let f = Fixture::new(enabled, Duration::from_secs(10)).await;
        for case in 0..5 {
            sleep(Duration::from_millis(110)).await;
            let mut stream = fake_phone::tls(
                &format!("{}:{}", f.pair.host, f.pair.port),
                f.pair.cert_sha256.clone(),
            )
            .await
            .unwrap();
            let challenge = protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT)
                .await
                .unwrap();
            let mut hello = match case {
                0 | 2 => fake_phone::hello(&f.pair, &challenge).unwrap(),
                _ => fake_phone::tunnel_hello(&f.pair, &challenge).unwrap(),
            };
            match case {
                0 => hello["role"] = json!("tunnel"), // control MAC in tunnel context
                1 => {
                    hello.as_object_mut().unwrap().remove("role");
                }
                2 => hello["role"] = json!("other"),
                3 => hello["role"] = Value::Null,
                _ if enabled => continue,
                _ => {} // valid tunnel hello with forwarding disabled
            }
            protocol::write_frame(&mut stream, &hello).await.unwrap();
            assert!(timeout(
                Duration::from_secs(1),
                protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT)
            )
            .await
            .unwrap()
            .is_err());
            assert_eq!(f.gateway.snapshot().await["transport"], "offline");
        }
    }
}

#[tokio::test]
async fn empty_pool_wait_deadline_closes_local() {
    let f = Fixture::new(true, Duration::from_secs(10)).await;
    let started = Instant::now();
    closed(TcpStream::connect(f.forwards[0]).await.unwrap()).await;
    assert!(started.elapsed() >= Duration::from_millis(400));
    f.count("active", 0).await;
}

#[tokio::test]
async fn rejected_port_closes_local() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    f.tunnel(TunnelOptions {
        reject: Some(8100),
        ..Default::default()
    })
    .await;
    closed(TcpStream::connect(f.forwards[0]).await.unwrap()).await;
    f.count("idle", 0).await;
}

#[tokio::test]
async fn stalled_open_skips_oldest_and_uses_next() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    let echo = f.echo().await;
    f.tunnel(TunnelOptions {
        stall: true,
        ..Default::default()
    })
    .await;
    f.tunnel(target(8100, echo)).await;
    let started = Instant::now();
    roundtrip(f.forwards[0], b"next tunnel".to_vec()).await;
    assert!(started.elapsed() >= Duration::from_millis(140));
    f.count("idle", 0).await;
}

#[tokio::test]
async fn waiting_local_uses_new_arrival() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    let echo = f.echo().await;
    let address = f.forwards[0];
    let task = tokio::spawn(async move { roundtrip(address, b"arrival".to_vec()).await });
    f.count("active", 1).await;
    f.tunnel(target(8100, echo)).await;
    task.await.unwrap();
}

#[tokio::test]
async fn idle_timeout_closes_phone() {
    let f = Fixture::new(true, Duration::from_millis(120)).await;
    let mut stream = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    f.count("idle", 1).await;
    assert!(timeout(
        Duration::from_secs(1),
        protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT)
    )
    .await
    .unwrap()
    .is_err());
    f.count("idle", 0).await;
}

#[tokio::test]
async fn idle_pool_limit_eight() {
    let f = Fixture::new(true, Duration::from_secs(10)).await;
    let mut streams = Vec::new();
    for _ in 0..8 {
        sleep(Duration::from_millis(110)).await;
        streams.push(fake_phone::connect_tunnel(&f.pair).await.unwrap());
    }
    f.count("idle", 8).await;
    sleep(Duration::from_millis(110)).await;
    let mut extra = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    // The oldest is evicted (closed); the newest arrival stays in the pool.
    assert!(timeout(
        Duration::from_secs(1),
        protocol::read_frame(&mut streams[0], protocol::PRE_AUTH_LIMIT)
    )
    .await
    .unwrap()
    .is_err());
    assert!(timeout(
        Duration::from_millis(300),
        protocol::read_frame(&mut extra, protocol::PRE_AUTH_LIMIT)
    )
    .await
    .is_err());
    f.count("idle", 8).await;
}

#[tokio::test]
async fn active_limit_sixteen_across_both_listeners() {
    let f = Fixture::new(true, Duration::from_secs(10)).await;
    let mut locals = Vec::new();
    for i in 0..16 {
        locals.push(TcpStream::connect(f.forwards[i % 2]).await.unwrap());
    }
    f.count("active", 16).await;
    closed(TcpStream::connect(f.forwards[1]).await.unwrap()).await;
    assert_eq!(f.gateway.snapshot().await["tunnels"]["active"], 16);
}

#[tokio::test]
async fn forward_addresses_loopback_only() {
    let dir = std::env::temp_dir().join(format!("forward-test-{}", uuid::Uuid::new_v4()));
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
    for address in ["0.0.0.0:0", "192.0.2.1:8100", "[::]:0", "[2001:db8::1]:0"] {
        for (runner, mjpeg) in [
            (Some(address.parse().unwrap()), None),
            (None, Some(address.parse().unwrap())),
        ] {
            assert!(Gateway::new(config.clone(), Timing::default())
                .with_forwards(runner, mjpeg)
                .is_err());
        }
    }
    for address in ["127.0.0.1:0", "127.0.0.2:0", "[::1]:0"] {
        assert!(Gateway::new(config.clone(), Timing::default())
            .with_forwards(Some(address.parse().unwrap()), None)
            .is_ok());
    }
}

#[tokio::test]
async fn malformed_opened_skips_channel_without_consuming_raw_bytes() {
    let mut f = Fixture::new(true, Duration::from_secs(10)).await;
    let mut bad = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    f.tasks.push(tokio::spawn(async move {
        assert_eq!(
            protocol::read_frame(&mut bad, protocol::PRE_AUTH_LIMIT)
                .await
                .unwrap()["t"],
            "open"
        );
        protocol::write_frame(&mut bad, &json!({"t":"unexpected","ok":true}))
            .await
            .unwrap();
    }));
    sleep(Duration::from_millis(110)).await;
    let mut good = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    f.tasks.push(tokio::spawn(async move {
        let open = protocol::read_frame(&mut good, protocol::PRE_AUTH_LIMIT)
            .await
            .unwrap();
        assert_eq!(open["port"], 8100);
        // One TLS write contains both the framed acknowledgement and immediate raw payload.
        let bytes = serde_json::to_vec(&json!({"t":"opened","ok":true})).unwrap();
        let mut combined = (bytes.len() as u32).to_be_bytes().to_vec();
        combined.extend(bytes);
        combined.extend(b"immediate raw bytes");
        good.write_all(&combined).await.unwrap();
        good.shutdown().await.unwrap();
        let mut incoming = Vec::new();
        good.read_to_end(&mut incoming).await.unwrap();
    }));
    let mut local = TcpStream::connect(f.forwards[0]).await.unwrap();
    local.shutdown().await.unwrap();
    let mut received = Vec::new();
    timeout(Duration::from_secs(2), local.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, b"immediate raw bytes");
}

#[tokio::test]
async fn control_reconnect_drops_tunnels_from_before_the_previous_link() {
    let f = Fixture::new(true, Duration::from_secs(10)).await;
    let _stale = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    f.count("idle", 1).await;
    sleep(Duration::from_millis(110)).await;
    let _first = fake_phone::connect(&f.pair).await.unwrap();
    sleep(Duration::from_millis(110)).await;
    let _fresh = fake_phone::connect_tunnel(&f.pair).await.unwrap();
    f.count("idle", 2).await;
    sleep(Duration::from_millis(110)).await;
    // A new control link: the tunnel that joined before the previous link was last alive goes.
    let _second = fake_phone::connect(&f.pair).await.unwrap();
    f.count("idle", 1).await;
}
