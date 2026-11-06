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
fn control_approval_rejects_changed_rect_identifier_foreground_or_page_amount() {
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
                json!({"value":{"type":"XCUIElementTypeApplication","bundleId":if v == 3 {"com.example.other"} else {"com.example.shop"},"label":"商城","rect":{"x":0,"y":0,"width":390,"height":844},"children":[{"type":"XCUIElementTypeStaticText","label":if v == 4 {"金额 200 元"} else {"金额 100 元"},"rect":{"x":0,"y":100,"width":200,"height":40}},{"type":"XCUIElementTypeButton","label":"立即购买","rawIdentifier":if v == 2 {"transaction-b"} else {"transaction-a"},"rect":{"x":if v == 1 {199} else {200},"y":780,"width":190,"height":64},"isEnabled":"1"}]}})
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
        let pending = state.owner_confirm.lock().unwrap().pending();
        let sheet = pending.iter().find(|p| p.id == id).unwrap();
        assert!(sheet
            .details
            .iter()
            .any(|d| d.starts_with("页面内容摘要：") && d.chars().count() == 15));
        approve(&mut state.owner_confirm.lock().unwrap(), &k, &id);
        for changed in 1..=4 {
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
fn selfrunner_bundles_are_refused_by_input_guard() {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("IPU_SELFRUNNER_BUNDLE_ID", value),
                None => std::env::remove_var("IPU_SELFRUNNER_BUNDLE_ID"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("IPU_SELFRUNNER_BUNDLE_ID"));
    std::env::set_var("IPU_SELFRUNNER_BUNDLE_ID", "dev.example.owner");
    support::block(async {
        for bundle in [
            "dev.iphoneuse.selfrunner.spike",
            "dev.iphoneuse.selfrunner.spike.signed",
            "dev.example.owner",
            "dev.example.owner.signed",
        ] {
            let apps: &'static str = match bundle {
                "dev.iphoneuse.selfrunner.spike" => {
                    r#"[{"bundleId":"dev.iphoneuse.selfrunner.spike"}]"#
                }
                "dev.iphoneuse.selfrunner.spike.signed" => {
                    r#"[{"bundleId":"dev.iphoneuse.selfrunner.spike.signed"}]"#
                }
                "dev.example.owner" => r#"[{"bundleId":"dev.example.owner"}]"#,
                _ => r#"[{"bundleId":"dev.example.owner.signed"}]"#,
            };
            let runner = support::mock_runner_with_apps(apps, |request, _| {
                assert!(!request.lines().next().unwrap_or("").contains("/actions"));
                assert!(!request
                    .lines()
                    .next()
                    .unwrap_or("")
                    .contains("/apps/launch"));
                Some((
                    Duration::ZERO,
                    if request.starts_with("POST /session ") {
                        r#"{"value":{"sessionId":"SESSION"}}"#
                    } else {
                        r#"{"value":null}"#
                    }
                    .into(),
                ))
            });
            let state = support::build_state_with_runner(runner.url());
            // Explicit target must be refused even with no foreground evidence.
            let (status, body) = call(
                &state,
                "/agent/input",
                json!({"type":"launch_app","bundle":bundle}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "owner_app_protected");
            state
                .runner
                .as_ref()
                .unwrap()
                .lock()
                .await
                .active_bundles()
                .await
                .unwrap();
            let (status, body) = call(&state, "/agent/input", json!({"type":"home"})).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert_eq!(body["error"], "owner_app_protected");
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

#[test]
fn control_alert_approval_binds_body_buttons_and_target_and_is_single_use() {
    support::block(async {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let version = Arc::new(AtomicUsize::new(0));
        let current = version.clone();
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let v = current.load(SeqCst);
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/alert/text") {
                if v == 3 {
                    json!({"value":null})
                } else {
                    json!({"value":if v == 1 {"向乙支付 200 元"} else {"向甲支付 100 元"}})
                }
            } else if line.contains("/wda/alert/buttons") {
                json!({"value":if v == 3 {vec![]} else if v == 2 {vec!["立即购买", "确认支付"]} else {vec!["确认转账", "确认支付"]}})
            } else {
                if line.starts_with("POST ")
                    && (line.contains("/alert/accept") || line.contains("/alert/dismiss"))
                {
                    count.fetch_add(1, SeqCst);
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
        for action in [
            json!({"type":"alert","button":"确认支付","ttl_ms":2500}),
            json!({"type":"alert","action":"accept","ttl_ms":2500}),
            json!({"type":"alert","action":"dismiss","ttl_ms":2500}),
        ] {
            version.store(0, SeqCst);
            let before = posts.load(SeqCst);
            let (status, body) = call(&state, "/control", action.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            let id = body["owner_confirm_id"].as_str().unwrap().to_owned();
            let pending = state.owner_confirm.lock().unwrap().pending();
            let sheet = pending.iter().find(|p| p.id == id).unwrap();
            assert!(sheet
                .details
                .iter()
                .any(|d| d.contains("弹窗正文：") && d.contains("向甲支付 100 元")));
            assert!(sheet.details.iter().any(|d| d.contains("目标按钮：")));
            approve(&mut state.owner_confirm.lock().unwrap(), &k, &id);
            for changed in 1..=2 {
                version.store(changed, SeqCst);
                let (status, body) = call(&state, "/control", action.clone()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
                assert_ne!(body["owner_confirm_id"], id);
                assert_eq!(posts.load(SeqCst), before);
            }
            // An unreadable alert must not consume an explicit payment approval.
            if action.get("button").is_some() {
                version.store(3, SeqCst);
                let (status, body) = call(&state, "/control", action.clone()).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
                let unreadable = body["owner_confirm_id"].as_str().unwrap().to_owned();
                approve(&mut state.owner_confirm.lock().unwrap(), &k, &unreadable);
                assert_eq!(
                    call(&state, "/control", action.clone()).await.0,
                    StatusCode::FORBIDDEN
                );
                assert_eq!(posts.load(SeqCst), before);
            }
            version.store(0, SeqCst);
            let (status, body) = call(&state, "/control", action.clone()).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(posts.load(SeqCst), before + 1);
            assert_eq!(
                call(&state, "/control", action).await.0,
                StatusCode::FORBIDDEN
            );
        }
    });
}

// Each /control attempt reads twice in the sensitive guard, then once in execution.
fn control_alert_confirmation_case(change: &str, during_execution: bool) {
    support::block(async {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let reads = Arc::new(AtomicUsize::new(0));
        let read_count = reads.clone();
        let phase = Arc::new(AtomicUsize::new(0));
        let current_phase = phase.clone();
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let change_is_unchanged = change == "unchanged";
        let change = change.to_owned();
        let mock_change = change.clone();
        let runner = support::mock_runner(move |request, _| {
            let change = mock_change.as_str();
            let line = request.lines().next().unwrap_or("");
            let phase = current_phase.load(SeqCst);
            let changed = phase == 1 && (!during_execution || read_count.load(SeqCst) >= 3);
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/alert/text") {
                let n = read_count.fetch_add(1, SeqCst);
                let changed = phase == 1 && (!during_execution || n >= 2);
                json!({"value":if changed && change == "null_text" {Value::Null}
                    else if changed && change == "body" {json!("B：支付 200 元")}
                    else if change == "null_text" {json!("")}
                    else {json!("A：支付 100 元")}})
            } else if line.contains("/wda/alert/buttons") {
                json!({"value":if changed && change == "mixed_buttons" {json!(["取消", 7, "确认支付"])}
                    else if changed && change == "buttons" {json!(["返回", "确认支付"])}
                    else if changed && change == "order" {json!(["确认支付", "取消"])}
                    else if changed && change == "empty_buttons" {json!([])}
                    else {json!(["取消", "确认支付"])}})
            } else {
                if line.starts_with("POST ")
                    && (line.contains("/alert/accept") || line.contains("/alert/dismiss"))
                {
                    count.fetch_add(1, SeqCst);
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
        let action = json!({"type":"alert","button":"确认支付","ttl_ms":2500});
        let (status, body) = call(&state, "/control", action.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let id = body["owner_confirm_id"].as_str().unwrap().to_owned();
        let original_controls = state
            .owner_confirm
            .lock()
            .unwrap()
            .pending()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap()
            .object["controls"]
            .clone();
        approve(&mut state.owner_confirm.lock().unwrap(), &k, &id);
        reads.store(0, SeqCst);
        phase.store(1, SeqCst);
        let (status, body) = call(&state, "/control", action.clone()).await;
        if change_is_unchanged {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(reads.load(SeqCst), 3);
            assert_eq!(posts.load(SeqCst), 1);
            assert_eq!(
                call(&state, "/control", action).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(posts.load(SeqCst), 1);
            return;
        }
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "owner_confirmation_required");
        assert_eq!(body["outcome"], "not_sent");
        assert_eq!(body["retry_safe"], false);
        assert_eq!(posts.load(SeqCst), 0);
        let fresh_id = body["owner_confirm_id"].as_str().unwrap();
        assert_ne!(fresh_id, id);
        let pending = state.owner_confirm.lock().unwrap().pending();
        let fresh = pending.iter().find(|p| p.id == fresh_id).unwrap();
        if during_execution {
            assert_eq!(reads.load(SeqCst), 3);
            if fresh.object["controls"][0].get("alert").is_some() {
                assert_ne!(fresh.object["controls"], original_controls);
                let alert = &fresh.object["controls"][0]["alert"];
                assert_eq!(alert["target"], "确认支付");
                match change.as_str() {
                    "body" => assert_eq!(alert["text"], "B：支付 200 元"),
                    "buttons" => assert_eq!(alert["buttons"], json!(["返回", "确认支付"])),
                    "order" => assert_eq!(alert["buttons"], json!(["确认支付", "取消"])),
                    _ => panic!("malformed execution read produced an identity"),
                }
            }
        } else {
            // An unreadable identity may produce a sheet, but never a consumable one.
            assert!(fresh.object["controls"][0].get("alert").is_none());
            approve(&mut state.owner_confirm.lock().unwrap(), &k, fresh_id);
            assert_eq!(
                call(&state, "/control", action.clone()).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(posts.load(SeqCst), 0);
        }
        phase.store(0, SeqCst);
        let (status, body) = call(&state, "/control", action.clone()).await;
        if during_execution {
            // The old approval was consumed before execution detected the change.
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            let next = body["owner_confirm_id"].as_str().unwrap();
            approve(&mut state.owner_confirm.lock().unwrap(), &k, next);
        } else {
            // Malformed reads did not consume the original, exact approval.
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(posts.load(SeqCst), 1);
            return;
        }
        let (status, body) = call(&state, "/control", action).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(posts.load(SeqCst), 1);
    });
}

#[test]
fn control_alert_approved_execution_rechecks_body_buttons_and_order() {
    for change in [
        "body",
        "buttons",
        "order",
        "null_text",
        "mixed_buttons",
        "empty_buttons",
    ] {
        control_alert_confirmation_case(change, true);
    }
}

#[test]
fn control_alert_null_text_cannot_consume_approval() {
    control_alert_confirmation_case("null_text", false);
}

#[test]
fn control_alert_mixed_buttons_cannot_consume_approval() {
    control_alert_confirmation_case("mixed_buttons", false);
}

#[test]
fn control_alert_empty_buttons_cannot_consume_approval() {
    control_alert_confirmation_case("empty_buttons", false);
}

#[test]
fn control_alert_approved_execution_unchanged_clicks_once() {
    control_alert_confirmation_case("unchanged", true);
}

#[test]
fn gateway_guard_resolves_targets_and_consumes_exact_approval_once() {
    support::block(async {
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let page = changed.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/source") {
                json!({"value":{"type":"XCUIElementTypeApplication","label":if page.load(std::sync::atomic::Ordering::SeqCst) {"changed"} else {"商城"},"rect":{"x":0,"y":0,"width":390,"height":844},"children":[
                    {"type":"XCUIElementTypeButton","label":"立即购买","name":"buy","rect":{"x":0,"y":100,"width":100,"height":50}},
                    {"type":"XCUIElementTypeButton","label":"支付","rect":{"x":100,"y":100,"width":100,"height":50}},
                    {"type":"XCUIElementTypeButton","label":"取消","rect":{"x":200,"y":100,"width":100,"height":50}},
                    {"type":"XCUIElementTypeSecureTextField","label":"密码","rect":{"x":0,"y":200,"width":100,"height":50}}
                ]}})
            } else if line.contains("/window/size") {
                json!({"value":{"width":390,"height":844}})
            } else if line.contains("/wda/apps/list") {
                json!({"value":[{"bundleId":"com.example.shop"}]})
            } else {
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
        for args in [
            json!({"label":"立即购买"}),
            json!({"label":"支付"}),
            json!({"x":50,"y":125}),
            json!({"x":150,"y":125}),
            json!({"label":"立即购买","index":0}),
            json!({"label":"支付","index":0}),
        ] {
            let command = json!({"action":"tap","args":args});
            let (status, body) = call(&state, "/guard/gateway-command", command.clone()).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            let id = body["owner_confirm_id"].as_str().unwrap();
            approve(&mut state.owner_confirm.lock().unwrap(), &k, id);
            assert_eq!(
                call(&state, "/guard/gateway-command", command.clone())
                    .await
                    .1["allow"],
                true
            );
            assert_eq!(
                call(&state, "/guard/gateway-command", command.clone())
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
        }
        let command = json!({"action":"tap","args":{"label":"支付"}});
        let (_, body) = call(&state, "/guard/gateway-command", command.clone()).await;
        approve(
            &mut state.owner_confirm.lock().unwrap(),
            &k,
            body["owner_confirm_id"].as_str().unwrap(),
        );
        changed.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            call(&state, "/guard/gateway-command", command).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &state,
                "/guard/gateway-command",
                json!({"action":"tap","args":{"label":"取消"}})
            )
            .await
            .1["allow"],
            true
        );
        let (status, body) = call(
            &state,
            "/guard/gateway-command",
            json!({"action":"type","args":{"text":"1234"}}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["owner_confirm_id"].is_string());
    });
}

#[test]
fn gateway_confirmation_pool_does_not_displace_control() {
    let dir = tempfile::tempdir().unwrap();
    let mut owner = OwnerConfirm::new(dir.path().into());
    let k = key();
    bind(&mut owner, &k);
    for i in 0..8 {
        owner
            .request("gateway_press", json!({"i":i}), "gateway")
            .unwrap();
    }
    assert_eq!(
        owner
            .request("gateway_press", json!({"i":8}), "gateway")
            .unwrap_err(),
        "owner_confirmation_limit"
    );
    for i in 0..16 {
        owner
            .request("control_press", json!({"i":i}), "control")
            .unwrap();
    }
    assert_eq!(owner.pending().len(), 24);
}

#[test]
fn gateway_swipe_candidates_type_coverage_and_busy() {
    support::block(async {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mode = Arc::new(AtomicUsize::new(0));
        let source_mode = mode.clone();
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/source") {
                let body = json!({"value":{"type":"XCUIElementTypeApplication","label":"shop","rect":{"x":0,"y":0,"width":390,"height":844},"children":[
                    {"type":"XCUIElementTypeButton","label":"继续","rect":{"x":0,"y":100,"width":100,"height":50}},
                    {"type":"XCUIElementTypeButton","label":"继续","rect":{"x":100,"y":100,"width":100,"height":50},"children":[
                        {"type":"XCUIElementTypeStaticText","label":"确认付款","rect":{"x":110,"y":110,"width":80,"height":20}}]},
                    {"type":"XCUIElementTypeTextField","label":"搜索","isFocused":true,"rect":{"x":0,"y":200,"width":100,"height":50}}
                ]}}).to_string();
                let coverage = match source_mode.load(Ordering::SeqCst) {
                    1 => "X-IPU-Truncated: 1\r\nX-IPU-Tree-Complete: 0\r\n",
                    2 => "",
                    _ => "X-IPU-Truncated: 0\r\nX-IPU-Tree-Complete: 1\r\n",
                };
                return Some((Duration::ZERO, format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{coverage}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())));
            } else {
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
        for index in [json!(0), json!(1), Value::Null] {
            let (_, body) = call(
                &state,
                "/guard/gateway-command",
                json!({"action":"tap","args":{"label":"继续","index":index}}),
            )
            .await;
            assert!(body["owner_confirm_id"].is_string(), "{body}");
        }
        let (_, body) = call(
            &state,
            "/guard/gateway-command",
            json!({"action":"swipe","args":{"from":{"x":150,"y":125},"to":{"x":150,"y":125}}}),
        )
        .await;
        assert!(body["owner_confirm_id"].is_string(), "{body}");
        assert_eq!(
            call(
                &state,
                "/guard/gateway-command",
                json!({"action":"press","args":{"button":"lock","shortcut_name":"付款"}})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        for m in [1, 2, 0] {
            mode.store(m, Ordering::SeqCst);
            let (status, body) = call(
                &state,
                "/guard/gateway-command",
                json!({"action":"type","args":{"text":"hello"}}),
            )
            .await;
            if m == 0 {
                assert_eq!(body["allow"], true, "{body}");
            } else {
                assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            }
        }
        // The first request holds runner lock, hence is still inside admission.
        let runner_lock = state.runner.as_ref().unwrap().lock().await;
        let first_state = state.clone();
        let first = tokio::spawn(async move {
            call(
                &first_state,
                "/guard/gateway-command",
                json!({"action":"type","args":{"text":"pending"}}),
            )
            .await
        });
        while !state.gateway_guard_busy.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let (status, body) = call(
            &state,
            "/guard/gateway-command",
            json!({"action":"type","args":{"text":"second"}}),
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"], "guard_busy");
        drop(runner_lock);
        first.await.unwrap();
    });
}

#[test]
fn gateway_type_newline_checks_keyboard_submit_key() {
    support::block(async {
        let runner = support::mock_runner(move |request, _| {
            let line = request.lines().next().unwrap_or("");
            let body = if line.starts_with("POST /session ") {
                json!({"value":{"sessionId":"SESSION"}})
            } else if line.contains("/source") {
                let body = json!({"value":{"type":"XCUIElementTypeApplication","label":"shop","rect":{"x":0,"y":0,"width":390,"height":844},"children":[
                    {"type":"XCUIElementTypeTextField","label":"备注","isFocused":true,"rect":{"x":0,"y":200,"width":300,"height":50}},
                    {"type":"XCUIElementTypeKeyboard","rect":{"x":0,"y":544,"width":390,"height":300},"children":[
                        {"type":"XCUIElementTypeKey","label":"a","rect":{"x":0,"y":560,"width":39,"height":50}},
                        {"type":"XCUIElementTypeKey","label":"删除","rect":{"x":340,"y":700,"width":50,"height":50}},
                        {"type":"XCUIElementTypeKey","label":"123","rect":{"x":0,"y":780,"width":50,"height":50}},
                        {"type":"XCUIElementTypeButton","label":"支付","rect":{"x":290,"y":780,"width":100,"height":50}}]}
                ]}}).to_string();
                return Some((Duration::ZERO, format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-IPU-Truncated: 0\r\nX-IPU-Tree-Complete: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len())));
            } else {
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
        // Plain text without a newline does not submit: allowed.
        let (_, body) = call(
            &state,
            "/guard/gateway-command",
            json!({"action":"type","args":{"text":"hello"}}),
        )
        .await;
        assert_eq!(body["allow"], true, "{body}");
        // A newline presses the keyboard's 支付 action key: owner confirmation.
        for text in ["\n", "hello\r"] {
            let (status, body) = call(
                &state,
                "/guard/gateway-command",
                json!({"action":"type","args":{"text":text}}),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
            assert!(body["owner_confirm_id"].is_string(), "{body}");
        }
    });
}
