//! Presses that pay or place an order, and typing into a password or code
//! field, are refused by the daemon (`crate::guard`) whatever the agent
//! asks: every way of reaching the control answers `403 sensitive_action`
//! and the phone receives nothing. Ordinary and send buttons still work, and
//! a saved flow's replay passes.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use server::http::AppState;
use support::{block, build_state_with_runner, mock_runner_with_apps};

const SESSION: &str = r#"{"value":{"sessionId":"SESSION"}}"#;

/// A product page: buy and cart buttons, a send button, a cell whose text
/// pays, a 免密支付 switch, a password field, a focused code field, and a
/// keyboard whose action key pays.
const TREE: &str = r#"{"value":{
    "type":"XCUIElementTypeApplication","label":"商城",
    "rect":{"x":0,"y":0,"width":390,"height":844},
    "children":[
      {"type":"XCUIElementTypeButton","label":"发送","rect":{"x":300,"y":100,"width":80,"height":44},"isEnabled":"1"},
      {"type":"XCUIElementTypeCell","label":"","rect":{"x":0,"y":160,"width":390,"height":60},"isEnabled":"1","children":[
        {"type":"XCUIElementTypeStaticText","label":"确认付款 ¥99","rect":{"x":20,"y":170,"width":200,"height":40}}
      ]},
      {"type":"XCUIElementTypeSwitch","label":"免密支付","value":"0","rect":{"x":300,"y":230,"width":60,"height":40},"isEnabled":"1"},
      {"type":"XCUIElementTypeSecureTextField","label":"支付密码","rect":{"x":20,"y":290,"width":350,"height":44},"isEnabled":"1"},
      {"type":"XCUIElementTypeTextField","label":"短信验证码","rect":{"x":20,"y":350,"width":350,"height":44},"isEnabled":"1","isFocused":"1"},
      {"type":"XCUIElementTypeKeyboard","label":"","rect":{"x":0,"y":420,"width":390,"height":200},"children":[
        {"type":"XCUIElementTypeKey","label":"1","rect":{"x":0,"y":430,"width":130,"height":50}},
        {"type":"XCUIElementTypeKey","label":"9","rect":{"x":260,"y":490,"width":130,"height":50}},
        {"type":"XCUIElementTypeKey","label":"空格","rect":{"x":0,"y":550,"width":260,"height":50}},
        {"type":"XCUIElementTypeButton","label":"确认付款","rect":{"x":260,"y":550,"width":130,"height":50},"isEnabled":"1"}
      ]},
      {"type":"XCUIElementTypeButton","label":"加入购物车","rect":{"x":0,"y":780,"width":190,"height":64},"isEnabled":"1"},
      {"type":"XCUIElementTypeButton","label":"立即购买","rawIdentifier":"buyNow","rect":{"x":200,"y":780,"width":190,"height":64},"isEnabled":"1"}
    ]}}"#;

/// Every request that reaches the runner, first line and body.
type Seen = Arc<Mutex<Vec<String>>>;

fn runner(apps: &'static str, tree: Option<&'static str>, seen: Seen) -> support::MockRunner {
    runner_with_alert(apps, tree, false, seen)
}

/// [`runner`] with a 确认支付 alert up (`alert`) or none.
fn runner_with_alert(
    apps: &'static str,
    tree: Option<&'static str>,
    alert: bool,
    seen: Seen,
) -> support::MockRunner {
    mock_runner_with_apps(apps, move |request, _| {
        let line = request.lines().next().unwrap_or("").to_string();
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        seen.lock().unwrap().push(format!("{line} {body}"));
        let reply = |s: &str| Some((Duration::ZERO, s.to_string()));
        if line.starts_with("POST /session ") {
            return reply(SESSION);
        }
        if line.contains("/source") {
            return match tree {
                Some(tree) => reply(tree),
                None => reply(r#"{"value":{"error":"unknown error","message":"snapshot failed"}}"#),
            };
        }
        if line.contains("/window/size") {
            return reply(r#"{"value":{"width":390,"height":844}}"#);
        }
        if line.contains("/alert/text") && !alert {
            let body = r#"{"value":{"error":"no such alert","message":"no alert"}}"#;
            return Some((
                Duration::ZERO,
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
            ));
        }
        if line.contains("/alert/text") {
            return reply(r#"{"value":"确认支付 ¥99？"}"#);
        }
        if line.contains("/wda/alert/buttons") {
            return reply(r#"{"value":["取消","立即支付"]}"#);
        }
        reply(r#"{"value":null}"#)
    })
}

/// Requests that would change the phone: any POST but a session, an
/// element lookup or a read.
fn sent(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with("POST "))
        .filter(|line| !line.starts_with("POST /session "))
        .filter(|line| !line.contains("/elements ") && !line.contains("/source"))
        .cloned()
        .collect()
}

async fn call(
    state: &Arc<AppState>,
    uri: &str,
    body: &str,
    flow_run: bool,
) -> (StatusCode, serde_json::Value) {
    call_grant(state, uri, body, flow_run.then_some("1"), None).await
}

async fn call_grant(
    state: &Arc<AppState>,
    uri: &str,
    body: &str,
    marker: Option<&str>,
    grant: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder()
        .method(if body.is_empty() { "GET" } else { "POST" })
        .uri(uri)
        .header("x-phone-control", "1")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(marker) = marker {
        request = request.header("x-phone-flow-run", marker);
    }
    if let Some(grant) = grant {
        request = request.header("x-phone-flow-grant", grant);
    }
    let response = server::http::router(state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn assert_refused(status: StatusCode, json: &serde_json::Value, seen: &Seen, what: &str) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{what}: {json}");
    assert_eq!(json["error"], "sensitive_action", "{what}: {json}");
    assert_eq!(json["outcome"], "not_sent", "{what}: {json}");
    assert!(
        json["hint"]
            .as_str()
            .unwrap_or("")
            .contains("主人自己在手机上"),
        "{what}: {json}"
    );
    assert_eq!(
        sent(seen),
        Vec::<String>::new(),
        "{what}: nothing reaches the phone"
    );
}

/// Center of a rect as normalized coordinates on the 390×844 screen.
fn at(x: f64, y: f64, w: f64, h: f64) -> String {
    format!(
        r#""x":{},"y":{}"#,
        (x + w / 2.0) / 390.0,
        (y + h / 2.0) / 844.0
    )
}

/// The snapshot token and the index of the row labelled `label`.
async fn element(state: &Arc<AppState>, label: &str) -> (String, usize) {
    let (status, json) = call(state, "/agent/elements", "", false).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let index = json["elements"]
        .as_array()
        .unwrap()
        .iter()
        .position(|row| row["label"] == label)
        .unwrap_or_else(|| panic!("{label} in {json}"));
    (json["snapshot"].as_str().unwrap().to_string(), index)
}

#[test]
fn every_way_to_press_pay_is_refused_before_the_phone_sees_it() {
    block(async {
        let seen: Seen = Arc::default();
        let mock = runner("[]", Some(TREE), seen.clone());
        let state = build_state_with_runner(mock.url());
        let singles: Vec<(&str, String)> = vec![
            (
                "tap label",
                r#"{"type":"tap","label":"立即购买"}"#.to_string(),
            ),
            (
                "tap coordinates",
                format!(r#"{{"type":"tap",{}}}"#, at(200.0, 780.0, 190.0, 64.0)),
            ),
            (
                "tap text inside a cell",
                format!(r#"{{"type":"tap",{}}}"#, at(250.0, 165.0, 10.0, 10.0)),
            ),
            (
                "longpress coordinates",
                format!(
                    r#"{{"type":"longpress",{}}}"#,
                    at(200.0, 780.0, 190.0, 64.0)
                ),
            ),
            (
                "tap_locator",
                r#"{"type":"tap_locator","locator":{"label_contains":"立即"}}"#.to_string(),
            ),
            (
                "tap_locator by identifier",
                r#"{"type":"tap_locator","locator":{"identifier":"buyNow"}}"#.to_string(),
            ),
            (
                "a swipe that does not move",
                r#"{"type":"swipe","x1":0.75,"y1":0.96,"x2":0.76,"y2":0.96}"#.to_string(),
            ),
            (
                "keyboard return",
                r#"{"type":"key","name":"return"}"#.to_string(),
            ),
            (
                "text into a code field",
                r#"{"type":"text","text":"123456"}"#.to_string(),
            ),
        ];
        for (what, body) in singles {
            seen.lock().unwrap().clear();
            let (status, json) = call(&state, "/agent/input", &body, false).await;
            assert_refused(status, &json, &seen, what);
        }

        // Element-bound actions name a row of a snapshot.
        for (what, label, action) in [
            ("tap element", "", r#""type":"tap""#),
            (
                "perform toggle",
                "免密支付",
                r#""type":"perform","action":"toggle""#,
            ),
            (
                "set_value into a password field",
                "支付密码",
                r#""type":"set_value","value":"123456""#,
            ),
        ] {
            let (snapshot, index) = if label.is_empty() {
                // The cell with no label of its own.
                let (snapshot, text) = element(&state, "确认付款 ¥99").await;
                (snapshot, text - 1)
            } else {
                element(&state, label).await
            };
            seen.lock().unwrap().clear();
            let body = format!(r#"{{{action},"element":{index},"snapshot":"{snapshot}"}}"#);
            let (status, json) = call(&state, "/agent/input", &body, false).await;
            assert_refused(status, &json, &seen, what);
        }

        // One paying step refuses the whole batch: the first step never runs.
        seen.lock().unwrap().clear();
        let body = format!(
            r#"{{"steps":[{{"kind":"action","action":{{"type":"tap",{}}}}},
                         {{"kind":"action","action":{{"type":"tap","label":"立即购买"}}}}]}}"#,
            at(0.0, 780.0, 190.0, 64.0)
        );
        let (status, json) = call(&state, "/agent/actions", &body, false).await;
        assert_refused(status, &json, &seen, "batch");
        assert_eq!(json["batch_outcome"], "nothing_applied", "{json}");

        // A step whose target is only known on the screen it lands on stops
        // the batch there.
        seen.lock().unwrap().clear();
        let body = format!(
            r#"{{"steps":[{{"kind":"action","action":{{"type":"tap",{}}}}}]}}"#,
            at(200.0, 780.0, 190.0, 64.0)
        );
        let (status, json) = call(&state, "/agent/actions", &body, false).await;
        assert_refused(status, &json, &seen, "batch coordinates");
        assert_eq!(json["failed_step"], 0, "{json}");
    });
}

#[test]
fn ordinary_and_send_buttons_and_flow_replays_still_run() {
    block(async {
        let seen: Seen = Arc::default();
        let mock = runner("[]", Some(TREE), seen.clone());
        let state = build_state_with_runner(mock.url());
        for (what, rect) in [
            ("ordinary button", (0.0, 780.0, 190.0, 64.0)),
            ("send button", (300.0, 100.0, 80.0, 44.0)),
        ] {
            seen.lock().unwrap().clear();
            let body = format!(r#"{{"type":"tap",{}}}"#, at(rect.0, rect.1, rect.2, rect.3));
            let (status, json) = call(&state, "/agent/input", &body, false).await;
            assert_eq!(status, StatusCode::OK, "{what}: {json}");
            assert!(!sent(&seen).is_empty(), "{what}: the tap reached the phone");
        }

        // A caller's replay marker carries no sensitive authority.
        seen.lock().unwrap().clear();
        let body = format!(
            r#"{{"steps":[{{"kind":"action","action":{{"type":"tap",{}}}}}]}}"#,
            at(200.0, 780.0, 190.0, 64.0)
        );
        for marker in ["1", "random-forged-token"] {
            let (status, json) =
                call_grant(&state, "/agent/actions", &body, Some(marker), Some(marker)).await;
            assert_refused(status, &json, &seen, marker);
        }
        // The same batch without the flow header is refused.
        seen.lock().unwrap().clear();
        let (status, json) = call(&state, "/agent/actions", &body, false).await;
        assert_refused(status, &json, &seen, "same batch, not a flow");
    });
}

#[test]
fn an_unreadable_press_in_a_payment_app_is_refused_and_elsewhere_passes() {
    block(async {
        let body = format!(r#"{{"type":"tap",{}}}"#, at(0.0, 0.0, 390.0, 844.0));
        let seen: Seen = Arc::default();
        let mock = runner(
            r#"[{"bundleId":"com.alipay.iphoneclient"}]"#,
            None,
            seen.clone(),
        );
        let state = build_state_with_runner(mock.url());
        let (status, json) = call(&state, "/agent/input", &body, false).await;
        assert_refused(status, &json, &seen, "unreadable tap in Alipay");
        assert_eq!(json["app"], "支付宝", "{json}");

        let seen: Seen = Arc::default();
        let mock = runner(
            r#"[{"bundleId":"com.apple.Preferences"}]"#,
            None,
            seen.clone(),
        );
        let state = build_state_with_runner(mock.url());
        let (status, json) = call(&state, "/agent/input", &body, false).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(!sent(&seen).is_empty(), "the tap reached the phone");
    });
}

#[test]
fn alert_buttons_that_pay_are_refused() {
    block(async {
        let seen: Seen = Arc::default();
        let mock = runner_with_alert("[]", Some(TREE), true, seen.clone());
        let state = build_state_with_runner(mock.url());
        for (what, body) in [
            ("alert button", r#"{"type":"alert","button":"确认支付"}"#),
            ("alert accept", r#"{"type":"alert","action":"accept"}"#),
        ] {
            seen.lock().unwrap().clear();
            let (status, json) = call(&state, "/agent/input", body, false).await;
            assert_refused(status, &json, &seen, what);
        }
        // Cancelling it is the agent's to do.
        seen.lock().unwrap().clear();
        let (status, json) = call(
            &state,
            "/agent/input",
            r#"{"type":"alert","button":"取消"}"#,
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert!(
            sent(&seen)
                .iter()
                .any(|line| line.contains("/alert/accept")),
            "{:?}",
            sent(&seen)
        );
    });
}

#[test]
fn installed_flow_grants_are_bound_and_single_use() {
    // This integration-test binary has only one environment-mutating test.
    // Restore it even if an assertion panics; other tests never grant from disk.
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => std::env::set_var("IPHONE_USE_FLOWS_DIR", v),
                None => std::env::remove_var("IPHONE_USE_FLOWS_DIR"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("IPHONE_USE_FLOWS_DIR"));
    let store = tempfile::tempdir().unwrap();
    std::env::set_var("IPHONE_USE_FLOWS_DIR", store.path());
    std::fs::create_dir(store.path().join("shop")).unwrap();
    let path = store.path().join("shop/buy.json");
    std::fs::write(
        &path,
        r#"{"version":1,"name":"Buy","steps":[{"kind":"tap","x":0.76,"y":0.96}]}"#,
    )
    .unwrap();
    let flow = iphone_use_mcp::flow::load_flow(&path).unwrap();
    let request = flow.actions_request(&Default::default()).unwrap();
    block(async {
        let seen: Seen = Arc::default();
        let mock = runner("[]", Some(TREE), seen.clone());
        let state = build_state_with_runner(mock.url());
        // Replay authority now additionally requires the owner's content signature.
        let owner_dir = tempfile::tempdir().unwrap();
        *state.owner_confirm.lock().unwrap() =
            server::owner_confirm::OwnerConfirm::new(owner_dir.path().into());
        use base64::Engine;
        use ring::{
            rand::SystemRandom,
            signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING},
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let public = b64.encode(key.public_key().as_ref());
        let proof = b64.encode(
            key.sign(
                &rng,
                format!("iphone-use-owner-key/v1\n{public}").as_bytes(),
            )
            .unwrap(),
        );
        let mut owner = state.owner_confirm.lock().unwrap();
        owner.bind(&public, Some(&proof)).unwrap();
        // The same object the daemon binds: the flow file, its inputs, its id
        // and the batch it expands to.
        let object = serde_json::json!({"flow":serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),"inputs":{},"id":"shop/buy","actions":request});
        let id = owner.request("flow", object, "Buy").unwrap();
        let pending = owner.pending()[0].clone();
        owner
            .confirm(
                &id,
                true,
                Some(&b64.encode(key.sign(&rng, pending.message().as_bytes()).unwrap())),
            )
            .unwrap();
        drop(owner);
        for id in ["../buy", "/tmp/buy.json", "shop/missing"] {
            let (status, _) = call(
                &state,
                "/agent/flow-run/grant",
                &serde_json::json!({"id":id}).to_string(),
                false,
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
        let expired = state
            .flow_grants
            .lock()
            .unwrap()
            .issue_at(
                &request.to_string(),
                std::time::Instant::now() - Duration::from_secs(121),
            )
            .unwrap();
        let (status, response) = call_grant(
            &state,
            "/agent/actions",
            &request.to_string(),
            Some("1"),
            Some(&expired),
        )
        .await;
        assert_refused(status, &response, &seen, "expired grant");
        let grant = || {
            call(
                &state,
                "/agent/flow-run/grant",
                r#"{"id":"shop/buy","inputs":{}}"#,
                false,
            )
        };
        let (status, response) = grant().await;
        assert_eq!(status, StatusCode::OK, "{response}");
        let token = response["grant"].as_str().unwrap();
        seen.lock().unwrap().clear();
        let (status, response) = call_grant(
            &state,
            "/agent/actions",
            &request.to_string(),
            Some("1"),
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert!(!sent(&seen).is_empty());
        seen.lock().unwrap().clear();
        let (status, response) = call_grant(
            &state,
            "/agent/actions",
            &request.to_string(),
            Some("1"),
            Some(token),
        )
        .await;
        assert_refused(status, &response, &seen, "used grant");
        let (_, response) = grant().await;
        let token = response["grant"].as_str().unwrap();
        let mut different = request.clone();
        different["steps"][0]["action"]["x"] = serde_json::json!(0.77);
        let (status, response) = call_grant(
            &state,
            "/agent/actions",
            &different.to_string(),
            Some("1"),
            Some(token),
        )
        .await;
        assert_refused(status, &response, &seen, "different steps");
        let (status, response) = call_grant(
            &state,
            "/agent/actions",
            &request.to_string(),
            Some("1"),
            Some(token),
        )
        .await;
        assert_refused(status, &response, &seen, "mismatch consumed grant");
    });
}
