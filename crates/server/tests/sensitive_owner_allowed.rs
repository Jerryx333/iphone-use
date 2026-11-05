//! A daemon task asks the owner before a press that pays (`needs_confirm`);
//! once they allow it, that one press passes the sensitive-action guard,
//! and only once. A request without the private step capability cannot consume it.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use support::{block, build_state_with_runner, mock_runner};

const TREE: &str = r#"{"value":{
    "type":"XCUIElementTypeApplication","label":"商城",
    "rect":{"x":0,"y":0,"width":390,"height":844},
    "children":[
      {"type":"XCUIElementTypeButton","label":"立即购买","rect":{"x":200,"y":780,"width":190,"height":64},"isEnabled":"1"}
    ]}}"#;

#[test]
fn the_press_the_owner_allowed_runs_once() {
    block(async {
        let posts: Arc<Mutex<usize>> = Arc::default();
        let count = posts.clone();
        let runner = mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let reply = |s: &str| Some((Duration::ZERO, s.to_string()));
            if line.starts_with("POST /session ") {
                return reply(r#"{"value":{"sessionId":"SESSION"}}"#);
            }
            if line.contains("/source") {
                return reply(TREE);
            }
            if line.contains("/window/size") {
                return reply(r#"{"value":{"width":390,"height":844}}"#);
            }
            if line.starts_with("POST ") && line.contains("/actions") {
                *count.lock().unwrap() += 1;
            }
            reply(r#"{"value":null}"#)
        });
        let state = build_state_with_runner(runner.url());
        let tap = |token: Option<String>| {
            let state = state.clone();
            async move {
                let mut request = Request::builder()
                    .method("POST")
                    .uri("/agent/input")
                    .header("x-phone-control", "1")
                    .header(header::CONTENT_TYPE, "application/json");
                if let Some(token) = token {
                    request = request.header("x-phone-owner-press", token);
                }
                let request = request
                    .body(Body::from(r#"{"type":"tap","x":0.75,"y":0.96}"#))
                    .unwrap();
                let response = server::http::router(state.clone())
                    .oneshot(request)
                    .await
                    .unwrap();
                let status = response.status();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_default(),
                )
            }
        };

        let (status, json) = tap(None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "not allowed yet: {json}");
        assert_eq!(*posts.lock().unwrap(), 0);

        let token = iphone_use_mcp::task::approve_press("立即购买").unwrap();
        let (status, json) = tap(None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "missing capability: {json}");
        assert_eq!(*posts.lock().unwrap(), 0);
        let wrong = iphone_use_mcp::task::approve_press("确认转账").unwrap();
        let (status, json) = tap(Some(wrong.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "wrong target: {json}");
        iphone_use_mcp::task::clear_approved_press(&wrong);
        let (status, json) = tap(Some(token.clone())).await;
        assert_eq!(status, StatusCode::OK, "allowed by the owner: {json}");
        assert_eq!(*posts.lock().unwrap(), 1, "the tap reached the phone");

        let (status, json) = tap(Some(token.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "used up: {json}");
        assert_eq!(json["error"], "sensitive_action", "{json}");
        assert_eq!(*posts.lock().unwrap(), 1);
    });
}
