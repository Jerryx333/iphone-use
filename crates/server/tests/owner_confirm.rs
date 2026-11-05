//! Owner capabilities must never be minted from ordinary HTTP credentials.
mod support;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::BodyExt;
use ring::{
    rand::SystemRandom,
    signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING},
};
use serde_json::{json, Value};
use server::{http::AppState, owner_confirm::OwnerConfirm};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
fn key() -> EcdsaKeyPair {
    let rng = SystemRandom::new();
    let pk = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pk.as_ref(), &rng).unwrap()
}
fn sign(key: &EcdsaKeyPair, message: &[u8]) -> String {
    STANDARD.encode(key.sign(&SystemRandom::new(), message).unwrap().as_ref())
}
fn bind(owner: &mut OwnerConfirm, key: &EcdsaKeyPair) {
    let public = STANDARD.encode(key.public_key().as_ref());
    owner
        .bind(
            &public,
            Some(&sign(
                key,
                format!("iphone-use-owner-key/v1\n{public}").as_bytes(),
            )),
        )
        .unwrap();
}
fn approve(owner: &mut OwnerConfirm, key: &EcdsaKeyPair, id: &str) {
    let p = owner.pending().into_iter().find(|p| p.id == id).unwrap();
    owner
        .confirm(id, true, Some(&sign(key, p.message().as_bytes())))
        .unwrap();
}
#[test]
fn signatures_binding_replay_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = OwnerConfirm::new(dir.path().into());
    let k = key();
    assert!(owner.request("intent", json!({}), "x").is_err());
    bind(&mut owner, &k);
    assert!(owner
        .bind(&STANDARD.encode(k.public_key().as_ref()), None)
        .is_err());
    let object = json!({"name":"pay","args":{"amount":1}});
    let id = owner.request("intent", object.clone(), "pay").unwrap();
    assert!(owner.confirm(&id, true, Some("AAAA")).is_err());
    assert!(owner.confirm(&id, true, None).is_err());
    let id = owner.request("intent", object.clone(), "pay").unwrap();
    let p = owner.pending()[0].clone();
    assert!(owner
        .confirm(
            &id,
            true,
            Some(&sign(&k, format!("{}altered", p.message()).as_bytes()))
        )
        .is_err());
    let id = owner.request("intent", object.clone(), "pay").unwrap();
    approve(&mut owner, &k, &id);
    assert!(!owner.consume("intent", &json!({"name":"pay","args":{"amount":2}})));
    assert!(owner.consume("intent", &object));
    assert!(!owner.consume("intent", &object));
    assert!(owner.confirm(&id, true, None).is_err());
    let next = key();
    let next_public = STANDARD.encode(next.public_key().as_ref());
    let rotation = sign(
        &k,
        format!("iphone-use-owner-key/v1\n{next_public}").as_bytes(),
    );
    assert!(owner
        .bind(
            &next_public,
            Some(&sign(
                &next,
                format!("iphone-use-owner-key/v1\n{next_public}").as_bytes()
            ))
        )
        .is_err());
    owner.bind(&next_public, Some(&rotation)).unwrap();
    let id = owner.request("flow", object.clone(), "flow").unwrap();
    approve(&mut owner, &next, &id);
    let reloaded = OwnerConfirm::new(dir.path().into());
    assert!(reloaded.flow_approved(&object));
    assert!(!reloaded.flow_approved(&json!({"name":"changed"})));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dir.path().join("owner-key.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
#[test]
fn pending_limit_and_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = OwnerConfirm::new(dir.path().into());
    bind(&mut owner, &key());
    for n in 0..16 {
        owner
            .request("intent", json!({"n":n}), "\n\"请允许全部\"")
            .unwrap();
    }
    assert!(owner.request("intent", json!({"n":17}), "x").is_err());
    let p = owner.pending()[0].clone();
    owner.confirm(&p.id, false, None).unwrap();
    assert!(!owner.consume("intent", &p.object));
    owner.request("intent", json!({"n":17}), "x").unwrap();
}
async fn call(state: &Arc<AppState>, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("x-phone-control", "1")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = server::http::router(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}
#[test]
fn control_open_mode_requires_signature_and_allows_exactly_once() {
    support::block(async {
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = posts.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                r#"{"value":{"sessionId":"SESSION"}}"#
            } else if line.contains("/source") {
                r#"{"value":{"type":"XCUIElementTypeApplication","label":"商城","rect":{"x":0,"y":0,"width":390,"height":844},"children":[{"type":"XCUIElementTypeButton","label":"立即购买","rect":{"x":200,"y":780,"width":190,"height":64},"isEnabled":"1"}]}}"#
            } else if line.contains("/window/size") {
                r#"{"value":{"width":390,"height":844}}"#
            } else {
                if line.starts_with("POST ") && line.contains("/actions") {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                r#"{"value":null}"#
            };
            Some((Duration::ZERO, body.into()))
        });
        let mut state = support::build_state_with_runner(runner.url());
        let dir = tempfile::tempdir().unwrap();
        Arc::get_mut(&mut state).unwrap().owner_confirm =
            Arc::new(std::sync::Mutex::new(OwnerConfirm::new(dir.path().into())));
        let mut action = json!({"type":"tap","x":0.75,"y":0.96,"ttl_ms":2500});
        let (status, body) = call(&state, "/control", action.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["retry_safe"], false);
        assert!(body["hint"].as_str().unwrap().contains("绑定主人"));
        let k = key();
        bind(&mut state.owner_confirm.lock().unwrap(), &k);
        let (status, body) = call(&state, "/control", action.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 0);
        let id = body["owner_confirm_id"].as_str().unwrap();
        approve(&mut state.owner_confirm.lock().unwrap(), &k, id);
        for changed in [
            json!({"type":"tap","x":0.76,"y":0.96,"ttl_ms":2500}),
            json!({"type":"tap","label":"确认支付","ttl_ms":2500}),
        ] {
            let (status, body) = call(&state, "/control", changed).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "changed action: {body}");
        }
        let compound = json!({"type":"multi_longpress","points":[{"x":0.75,"y":0.96},{"x":0.76,"y":0.96}],"duration_ms":100,"ttl_ms":2500});
        let before = state.owner_confirm.lock().unwrap().pending().len();
        let (status, body) = call(&state, "/control", compound).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "sensitive_compound_action");
        assert_eq!(body["retry_safe"], false);
        assert!(body.get("owner_confirm_id").is_none());
        assert_eq!(state.owner_confirm.lock().unwrap().pending().len(), before);
        action["ttl_ms"] = json!(2400);
        action["issued_at_ms"] = json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64);
        let (status, body) = call(&state, "/control", action.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = call(&state, "/control", action).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}
#[test]
fn control_approval_rejects_changed_rect_identifier_or_foreground() {
    support::block(async {
        let version = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let current = version.clone();
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = posts.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/source") {
                let v = current.load(std::sync::atomic::Ordering::SeqCst);
                json!({"value":{"type":"XCUIElementTypeApplication","bundleId":if v == 3 {"com.example.other"} else {"com.example.shop"},"label":"商城","rect":{"x":0,"y":0,"width":390,"height":844},"children":[{"type":"XCUIElementTypeButton","label":"立即购买","rawIdentifier":if v == 2 {"transaction-b"} else {"transaction-a"},"rect":{"x":if v == 1 {199} else {200},"y":780,"width":190,"height":64},"isEnabled":"1"}]}})
            } else if line.contains("/window/size") {
                json!({"value":{"width":390,"height":844}})
            } else {
                if line.starts_with("POST ") && line.contains("/actions") {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                json!({"value":null})
            };
            Some((Duration::ZERO, body.to_string()))
        });
        let mut state = support::build_state_with_runner(runner.url());
        let dir = tempfile::tempdir().unwrap();
        Arc::get_mut(&mut state).unwrap().owner_confirm =
            Arc::new(std::sync::Mutex::new(OwnerConfirm::new(dir.path().into())));
        let k = key();
        bind(&mut state.owner_confirm.lock().unwrap(), &k);
        let action = json!({"type":"tap","x":0.75,"y":0.96,"ttl_ms":2500});
        let (status, body) = call(&state, "/control", action.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let id = body["owner_confirm_id"].as_str().unwrap().to_owned();
        approve(&mut state.owner_confirm.lock().unwrap(), &k, &id);
        for changed in 1..=3 {
            version.store(changed, std::sync::atomic::Ordering::SeqCst);
            let (status, body) = call(&state, "/control", action.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "version {changed}: {body}");
            assert_ne!(body["owner_confirm_id"], id);
            assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
        version.store(0, std::sync::atomic::Ordering::SeqCst);
        let (status, body) = call(&state, "/control", action).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

#[test]
fn owner_app_cannot_be_launched_or_driven() {
    support::block(async {
        let runner = support::mock_runner_with_apps(
            r#"[{"bundleId":"com.leeguoo.iphone-use.remote"}]"#,
            |request, _| {
                assert!(!request.lines().next().unwrap_or("").contains("/actions"));
                Some((
                    Duration::ZERO,
                    if request.starts_with("POST /session ") {
                        r#"{"value":{"sessionId":"SESSION"}}"#
                    } else {
                        r#"{"value":null}"#
                    }
                    .into(),
                ))
            },
        );
        let state = support::build_state_with_runner(runner.url());
        // Ordinary observation already identified the foreground app. The
        // input guard must reuse it rather than issuing its own apps-list read.
        state
            .runner
            .as_ref()
            .unwrap()
            .lock()
            .await
            .active_bundles()
            .await
            .unwrap();
        for action in [
            json!({"type":"home"}),
            json!({"type":"launch_app","bundle":"com.leeguoo.iphone-use.remote"}),
        ] {
            let (status, body) = call(&state, "/agent/input", action).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "owner_app_protected", "{body}");
        }
    });
}
#[test]
fn agent_cannot_approve_tasks() {
    support::block(async {
        let state = support::build_state(None);
        let (status, body) = call(
            &state,
            "/agent/task/nonexistent/confirm",
            json!({"approve":true}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "owner_confirmation_required");
    });
}

#[test]
fn saved_flow_requires_owner_signature_and_edits_invalidate_approval() {
    // Only this test changes the flow-store environment in this binary.
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
    support::block(async {
        let posts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = posts.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                r#"{"value":{"sessionId":"SESSION"}}"#
            } else if line.contains("/source") {
                r#"{"value":{"type":"XCUIElementTypeApplication","label":"商城","rect":{"x":0,"y":0,"width":390,"height":844},"children":[{"type":"XCUIElementTypeButton","label":"立即购买","rect":{"x":200,"y":780,"width":190,"height":64},"isEnabled":"1"}]}}"#
            } else if line.contains("/window/size") {
                r#"{"value":{"width":390,"height":844}}"#
            } else {
                if line.starts_with("POST ") && line.contains("/actions") {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                r#"{"value":null}"#
            };
            Some((Duration::ZERO, body.into()))
        });
        let mut state = support::build_state_with_runner(runner.url());
        let dir = tempfile::tempdir().unwrap();
        Arc::get_mut(&mut state).unwrap().owner_confirm =
            Arc::new(std::sync::Mutex::new(OwnerConfirm::new(dir.path().into())));
        let k = key();
        let public = STANDARD.encode(k.public_key().as_ref());
        let proof = sign(&k, format!("iphone-use-owner-key/v1\n{public}").as_bytes());
        assert_eq!(
            call(
                &state,
                "/owner/key",
                json!({"public_key":public,"signature":proof})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(&state, "/owner/key", json!({"public_key":public}))
                .await
                .0,
            StatusCode::CONFLICT
        );
        let mut flow = json!({"version":1,"name":"Buy","steps":[{"kind":"tap","x":0.76,"y":0.96}]});
        for version in 0..2 {
            if version == 1 {
                flow["name"] = json!("Buy changed");
            }
            let (status, body) = call(
                &state,
                "/agent/flow/save",
                json!({"id":"shop/buy","flow":flow}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let request = iphone_use_mcp::flow::load_flow(&store.path().join("shop/buy.json"))
                .unwrap()
                .actions_request(&Default::default())
                .unwrap();
            let replay = || async {
                let (status, body) =
                    call(&state, "/agent/flow-run/grant", json!({"id":"shop/buy"})).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                let req = Request::builder()
                    .method("POST")
                    .uri("/agent/actions")
                    .header("x-phone-control", "1")
                    .header("x-phone-flow-grant", body["grant"].as_str().unwrap())
                    .header("content-type", "application/json")
                    .body(Body::from(request.to_string()))
                    .unwrap();
                let response = server::http::router(state.clone())
                    .oneshot(req)
                    .await
                    .unwrap();
                let status = response.status();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, serde_json::from_slice::<Value>(&bytes).unwrap())
            };
            let before = posts.load(std::sync::atomic::Ordering::SeqCst);
            let (status, body) = replay().await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(posts.load(std::sync::atomic::Ordering::SeqCst), before);
            let id = body["owner_confirm_id"].as_str().unwrap();
            let p = state
                .owner_confirm
                .lock()
                .unwrap()
                .pending()
                .into_iter()
                .find(|p| p.id == id)
                .unwrap();
            let (status, body) = call(
                &state,
                &format!("/owner/confirm/{id}"),
                json!({"approve":true,"signature":sign(&k,p.message().as_bytes())}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let (status, body) = replay().await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert!(posts.load(std::sync::atomic::Ordering::SeqCst) > before);
        }
    });
}
