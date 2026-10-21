mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use support::{block, build_state_with_runner, mock_native_runner};
use tower::ServiceExt;

async fn batch(base: &str, body: String) -> (StatusCode, serde_json::Value) {
    let response = server::http::router(build_state_with_runner(base))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/actions")
                .header("x-phone-control", "1")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn swipe_then_tap_calls_settle_but_swipe_then_pause_does_not() {
    block(async {
        for pause in [false, true] {
            let settles = Arc::new(AtomicUsize::new(0));
            let count = settles.clone();
            let runner = mock_native_runner(move |request, _| {
                if request.contains("/wda/settle") {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                let body = if request.starts_with("POST /session ") {
                    r#"{"value":{"sessionId":"SESSION"}}"#
                } else if request.contains("/window/size") {
                    r#"{"value":{"width":393,"height":852}}"#
                } else {
                    r#"{"value":null}"#
                };
                Some((Duration::ZERO, body.into()))
            });
            let next = if pause {
                serde_json::json!({"kind":"pause","ms":10})
            } else {
                serde_json::json!({"kind":"action","action":{"type":"tap","x":0.5,"y":0.5}})
            };
            let (_, body) = batch(
                runner.url(),
                serde_json::json!({"steps":[
                    {"kind":"action","action":{"type":"swipe","x1":0.5,"y1":0.8,"x2":0.5,"y2":0.2}},next
                ]})
                .to_string(),
            )
            .await;
            assert_eq!(body["ok"], true, "{body}");
            assert_eq!(
                settles.load(Ordering::SeqCst),
                usize::from(!pause),
                "{body}"
            );
        }
    });
}

#[test]
fn probe_misses_survive_a_final_tree_timeout() {
    block(async {
        let probes = Arc::new(AtomicUsize::new(0));
        let count = probes.clone();
        let runner = mock_native_runner(move |request, _| {
            if request.starts_with("POST /session ") {
                return Some((
                    Duration::ZERO,
                    r#"{"value":{"sessionId":"SESSION"}}"#.into(),
                ));
            }
            if request.contains("/elements") {
                count.fetch_add(1, Ordering::SeqCst);
                return Some((Duration::ZERO, r#"{"value":[]}"#.into()));
            }
            if request.contains("/source") {
                return Some((Duration::from_secs(3), r#"{"value":{}}"#.into()));
            }
            Some((Duration::ZERO, r#"{"value":null}"#.into()))
        });
        let (_,body) = batch(runner.url(),serde_json::json!({"steps":[{"kind":"wait_for", "expect":{"present":[{"identifier":"not-here"}]},"timeout_ms":1200,"poll_ms":100}]}).to_string()).await;
        assert_eq!(body["error"], "expectation_timeout", "{body}");
        assert!(probes.load(Ordering::SeqCst) > 0, "{body}");
        assert!(
            body["observation"]["probe_misses"].as_u64().unwrap() > 0,
            "{body}"
        );
        assert!(
            !body.to_string().contains("no readable element tree"),
            "{body}"
        );
    });
}
