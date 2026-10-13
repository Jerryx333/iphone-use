//! Runs alone in its own process: tracing caches callsite interest globally, so
//! capturing logs is flaky when other tests run in parallel threads.
#![allow(dead_code, unused_imports)]
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
#[tokio::test]
async fn logs_do_not_include_secrets_or_body() {
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Parallel tests without a subscriber may have cached callsite interest as "never".
    tracing::callsite::rebuild_interest_cache();
    let f = Fixture::new();
    let g = f.gateway();
    let marker = "PRIVATE_SCREEN_CONTENT";
    let result = http(&g,&f.config.token,json!({"action":"read","id":"log-test","deadline_ms":10,"args":{"text":marker,"psk":STANDARD.encode(&f.config.psk),"token":f.config.token}})).await;
    assert_eq!(result["error"]["code"], "expired");
    let typed = http(
        &g,
        &f.config.token,
        json!({"action":"type","id":"type-log-test","deadline_ms":10,"args":{"text":marker}}),
    )
    .await;
    assert_eq!(typed["error"]["code"], "expired");
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("log-test"));
    assert!(log.contains("expired"));
    assert!(log.contains("type-log-test"));
    assert!(!log.contains(marker));
    assert!(!log.contains(&f.config.token));
    assert!(!log.contains(&STANDARD.encode(&f.config.psk)));

    // Exercise the real injection/transport/response path with a test-only source.
    let fake_passcode = "908172";
    let expected_device = f.config.device_id.clone();
    let g = Gateway::with_passcode_reader(f.config.clone(), Timing::default(), move |device| {
        assert!(device == expected_device);
        Ok(fake_passcode.into())
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pairing = f
        .config
        .pairing("127.0.0.1".into(), listener.local_addr().unwrap().port());
    let clone = g.clone();
    let task = tokio::spawn(async move { clone.listen(listener).await.unwrap() });
    let mut phone = fake_phone::connect(&pairing).await.unwrap();
    wait_online(&g, "online").await;
    let clone = g.clone();
    let token = f.config.token.clone();
    let request = tokio::spawn(async move {
        http(
            &clone,
            &token,
            json!({"action":"unlock","id":"unlock-log-test","args":{},"deadline_ms":2000}),
        )
        .await
    });
    loop {
        let frame = protocol::read_frame(&mut phone, protocol::FRAME_LIMIT)
            .await
            .unwrap();
        if frame["t"] == "ping" {
            continue;
        }
        assert!(frame["args"]["passcode"].as_str() == Some(fake_passcode));
        protocol::write_frame(&mut phone, &json!({"t":"res","id":frame["id"],"ok":false,
            "error":{"code":fake_passcode,"message":fake_passcode},"result":{"passcode":fake_passcode}})).await.unwrap();
        break;
    }
    let result = request.await.unwrap();
    assert!(!result.to_string().contains(fake_passcode));
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("unlock-log-test"));
    assert!(log.contains("unlock_unconfirmed"));
    assert!(!log.contains(fake_passcode));
    task.abort();
}
