//! An owned delayed runner; no phone or personal daemon participates.
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine as _;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use support::{block, build_state_with_runner, mock_runner};
use tower::ServiceExt;
static SERIAL: Mutex<()> = Mutex::new(());
struct Ocr;
impl Ocr {
    fn new() -> Self {
        server::ocr::set_stand_in(Some(Arc::new(|_, _| {
            Ok(vec![server::ocr::OcrRow {
                text: "PRIVATE_BOARD".into(),
                confidence: 1.,
                rect: [0., 0., 100., 20.],
            }])
        })));
        Self
    }
}
impl Drop for Ocr {
    fn drop(&mut self) {
        server::ocr::set_stand_in(None);
    }
}
async fn hold(state: Arc<server::http::AppState>, private: bool) {
    let reply = server::http::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/hold")
                .header("x-phone-owner", "wait-fixture")
                .header("x-phone-control", "1")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"secs":if private{60}else{0},"private":private}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
}
async fn batch(state: Arc<server::http::AppState>, tail: bool) -> axum::response::Response {
    let mut steps = vec![
        json!({"kind":"action","action":{"type":"tap","x":0.5,"y":0.5}}),
        json!({"kind":"wait_for","expect":{"ocr_present":["PRIVATE_BOARD"]},"timeout_ms":3000,"poll_ms":100}),
    ];
    if tail {
        steps.push(json!({"kind":"action","action":{"type":"tap","x":0.2,"y":0.2}}));
    }
    server::http::router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/agent/actions")
                .header("x-phone-owner", "wait-fixture")
                .header("x-phone-control", "1")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"steps":steps,"observe":false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}
fn responder(request: &str) -> String {
    if request.starts_with("POST /session ") {
        json!({"value":{"sessionId":"SESSION"}}).to_string()
    } else if request.contains("/window/size") {
        json!({"value":{"width":390,"height":844}}).to_string()
    } else {
        json!({"value":null}).to_string()
    }
}
#[test]
fn private_on_off_during_capture_stops_suffix_and_preserves_applied_prefix() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ocr = Ocr::new();
    block(async {
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let taps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = taps.clone();
        let png = base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("fixtures/privacy.png"));
        let runner = mock_runner(move |request, _| {
            if request.contains("/actions ") {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if request.contains("/screenshot") {
                signal.notify_one();
                Some((Duration::from_millis(500), json!({"value":png}).to_string()))
            } else {
                Some((Duration::ZERO, responder(request)))
            }
        });
        let state = build_state_with_runner(runner.url());
        let worker = state.clone();
        let pending = tokio::spawn(async move { batch(worker, true).await });
        tokio::time::timeout(Duration::from_secs(3), started.notified())
            .await
            .unwrap();
        hold(state.clone(), true).await;
        hold(state, false).await;
        let reply = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reply.status(), StatusCode::LOCKED);
        let bytes = reply.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["applied_actions"], 1);
        assert_eq!(body["completed"], 1);
        assert_eq!(body["steps"][0]["ok"], true);
        assert_eq!(body["retry_safe"], false);
        assert_eq!(taps.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_BOARD"));
    });
}
#[test]
fn queued_ocr_body_withholds_text_without_erasing_tap_acknowledgement() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ocr = Ocr::new();
    block(async {
        let png = base64::engine::general_purpose::STANDARD
            .encode(include_bytes!("fixtures/privacy.png"));
        let runner = mock_runner(move |request, _| {
            Some((
                Duration::ZERO,
                if request.contains("/screenshot") {
                    json!({"value":png}).to_string()
                } else {
                    responder(request)
                },
            ))
        });
        let state = build_state_with_runner(runner.url());
        let reply = batch(state.clone(), false).await;
        assert_eq!(reply.status(), StatusCode::OK);
        hold(state.clone(), true).await;
        hold(state, false).await;
        let bytes = reply.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["applied_actions"], 1);
        assert_eq!(body["steps"][0]["ok"], true);
        assert!(
            body["steps"][1].get("ok").is_none(),
            "revoked waits cannot retain a success verdict"
        );
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "screen_privacy_changed");
        assert_eq!(body["observation_omitted"], true);
        assert_eq!(body["scope_changed"], "screen_privacy_changed");
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_BOARD"));
    });
}

#[test]
fn queued_element_wait_withholds_observation_but_preserves_action_receipt() {
    block(async {
        let runner = mock_runner(|request, _| {
            Some((
                Duration::ZERO,
                if request.contains("/source") {
                    json!({"value":{"type":"XCUIElementTypeApplication","label":"Owned","rect":{"x":0,"y":0,"width":390,"height":844},
                    "children":[{"type":"XCUIElementTypeStaticText","label":"PRIVATE_BOARD","rect":{"x":1,"y":1,"width":100,"height":20}}]}}).to_string()
                } else if request.contains("/elements ") {
                    json!({"value":[{"ELEMENT":"E"}]}).to_string()
                } else {
                    responder(request)
                },
            ))
        });
        let state = build_state_with_runner(runner.url());
        let reply=server::http::router(state.clone()).oneshot(Request::builder().method("POST").uri("/agent/actions")
            .header("x-phone-owner","wait-fixture").header("x-phone-control","1").header("content-type","application/json")
            .body(Body::from(json!({"observe":false,"steps":[
                {"kind":"action","action":{"type":"tap","x":0.5,"y":0.5}},
                {"kind":"wait_for","expect":{"present":[{"label":"PRIVATE_BOARD"}]},"timeout_ms":3000,"poll_ms":100}
            ]}).to_string())).unwrap()).await.unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        hold(state.clone(), true).await;
        hold(state, false).await;
        let bytes = reply.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["steps"][0]["ok"], true);
        assert_eq!(body["applied_actions"], 1);
        assert!(body["steps"][1].get("ok").is_none());
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "screen_privacy_changed");
        assert_eq!(body["observation_omitted"], true);
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE_BOARD"));
    });
}

#[test]
fn busy_ocr_refuses_wait_and_forced_elements_before_another_capture() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    block(async {
        let _reservation = server::ocr::reserve_background().unwrap();
        let captures = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = captures.clone();
        let runner = mock_runner(move |request, _| {
            if request.contains("/screenshot") {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Some((
                Duration::ZERO,
                if request.contains("/source") {
                    json!({"value":{"type":"XCUIElementTypeApplication","label":"Owned","rect":{"x":0,"y":0,"width":390,"height":844},
                "children":[{"type":"XCUIElementTypeButton","label":"Owned button","rect":{"x":1,"y":1,"width":100,"height":20}}]}}).to_string()
                } else {
                    responder(request)
                },
            ))
        });
        let state = build_state_with_runner(runner.url());
        let request = Request::builder().method("POST").uri("/agent/actions")
            .header("x-phone-owner","wait-fixture").header("x-phone-control","1").header("content-type","application/json")
            .body(Body::from(json!({"observe":false,"steps":[{"kind":"wait_for","expect":{"ocr_absent":["Missing"]},"timeout_ms":200,"poll_ms":50}]}).to_string())).unwrap();
        let response = server::http::router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["ok"], false, "busy OCR must not prove absence: {body}");
        assert!(body.to_string().contains("already recognizing"), "{body}");
        let response = server::http::router(state)
            .oneshot(
                Request::builder()
                    .uri("/agent/elements?ocr=force")
                    .header("x-phone-owner", "wait-fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            body.to_string().contains("already recognizing"),
            "forced OCR did not exercise the busy lane: {body}"
        );
        assert_eq!(
            captures.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "busy OCR must not capture more screenshots"
        );
    });
}
