//! Exercise real daemon guards and final snapshot resolution with a scripted runner.
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;

async fn call(
    state: &Arc<server::http::AppState>,
    path: &str,
    action: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(if action.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("x-phone-control", "1")
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("x-phone-owner-press", token);
    }
    let response = server::http::router(state.clone())
        .oneshot(
            req.body(Body::from(
                action.map(|v| v.to_string()).unwrap_or_default(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

#[test]
fn bound_token_skips_uninitialized_focus_with_available_bridge() {
    let dir = tempfile::tempdir().unwrap();
    server::instance::install(server::instance::Instance::derive("", dir.path(), None).unwrap());
    let instance = server::instance::current();
    std::fs::create_dir_all(&instance.state_dir).unwrap();
    let intents = ["focus_on", "focus_off"].map(|name| json!({"name":name,
        "summary":name,"side_effect":"write","args_schema":{},"returns_schema":{},
        "min_bridge_version":3,"status":"active"}));
    std::fs::write(instance.intents_registry(), json!({"version":3,
        "bridge":{"name":"iU Bridge","required_version":4},
        "intents":intents}).to_string()).unwrap();
    assert!(!server::focus::opted_out());
    support::block(async {
        for action in [json!({"type":"launch_app","bundle":"com.example.other"}),
            json!({"type":"key","name":"return"})] {
            let writes = Arc::new(Mutex::new(Vec::new()));
            let seen = writes.clone();
            let runner = support::mock_runner_with_apps(
                r#"[{"bundleId":"com.apple.springboard"}]"#,
                move |request, _| {
                    let line = request.lines().next().unwrap();
                    if line.starts_with("POST ") && !line.starts_with("POST /session ") {
                        seen.lock().unwrap().push(line.to_string());
                    }
                    let body = if line.starts_with("POST /session ") {
                        json!({"value":{"sessionId":"S"}})
                    } else if line.contains("/source") {
                        json!({"value":{"type":"XCUIElementTypeApplication","label":"Home Screen","bundleId":"com.apple.springboard"}})
                    } else { json!({"value":null}) };
                    Some((Duration::ZERO, body.to_string()))
                },
            );
            let mut state = support::build_state_with_runner(runner.url());
            Arc::get_mut(&mut state).unwrap().managed_runner = true;
            assert!(!state.agent_focus.lock().unwrap().attempted());
            let token = iphone_use_mcp::task::bound_step_fixture(json!({
                "bundle":"com.example.shop","action":"PRESS_RETURN","control":null})).unwrap();
            let (_, result) = call(&state, "/agent/input", Some(action), Some(&token)).await;
            assert_eq!(result["outcome"], "not_sent", "{result}");
            let writes = writes.lock().unwrap().clone();
            assert!(writes.is_empty(), "unexpected writes: {writes:?}");
            assert!(!state.agent_focus.lock().unwrap().attempted());
            iphone_use_mcp::task::clear_approved_press(&token);
        }
    });
}
