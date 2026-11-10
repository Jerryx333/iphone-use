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

fn question(tree: &Value) -> Value {
    use sha2::{Digest, Sha256};
    let page = tree["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap_or(""),
                r["label"].as_str().unwrap_or(""),
                r["value"].as_str().unwrap_or(""),
            )
        })
        .collect::<Vec<_>>();
    json!({"action":"CLICK","target":"立即购买","reason":"","app":"商城","bundle":"com.example.shop",
        "page_digest":server::owner_confirm::hex(&Sha256::digest(serde_json::to_vec(&page).unwrap())),
        "control":{"label":"立即购买","identifier":"pay","kind":"Button"},"parameters":{"operation":"CLICK","text":null}})
}

#[test]
fn final_daemon_tree_rejects_changed_amount_payee_or_bundle_without_tap() {
    support::block(async {
        for change in ["none", "amount", "payee", "bundle"] {
            let changed = Arc::new(Mutex::new(false));
            let live = changed.clone();
            let taps = Arc::new(Mutex::new(0));
            let count = taps.clone();
            let runner = support::mock_runner_with_apps(
                r#"[{"bundleId":"com.example.shop"}]"#,
                move |request, _| {
                    let line = request.lines().next().unwrap();
                    let reply = |v: Value| Some((Duration::ZERO, v.to_string()));
                    if line.starts_with("POST /session ") {
                        return reply(json!({"value":{"sessionId":"S"}}));
                    }
                    if line.contains("/source") {
                        let fresh = *live.lock().unwrap();
                        return reply(
                            json!({"value":{"type":"XCUIElementTypeApplication","label":"商城","bundleId":if fresh && change == "bundle" {"com.example.fake"} else {"com.example.shop"},"rect":{"x":0,"y":0,"width":390,"height":844},"children":[
                                {"type":"XCUIElementTypeStaticText","label":"金额","value":if fresh && change == "amount" {"999"} else {"55"}},
                                {"type":"XCUIElementTypeStaticText","label":if fresh && change == "payee" {"Mallory"} else {"Alice"}},
                                {"type":"XCUIElementTypeButton","label":"立即购买","rawIdentifier":"pay","rect":{"x":200,"y":700,"width":180,"height":60}}
                            ]}}),
                        );
                    }
                    if line.contains("/window/size") {
                        return reply(json!({"value":{"width":390,"height":844}}));
                    }
                    if line.ends_with("/elements HTTP/1.1") {
                        return reply(json!({"value":[{"ELEMENT":"pay"}]}));
                    }
                    if line.contains("/click") || line.contains("/actions") {
                        *count.lock().unwrap() += 1;
                    }
                    reply(json!({"value":null}))
                },
            );
            let state = support::build_state_with_runner(runner.url());
            let (status, tree) = call(&state, "/agent/elements", None, None).await;
            assert_eq!(status, StatusCode::OK, "{tree}");
            let token = iphone_use_mcp::task::bound_step_fixture(question(&tree)).unwrap();
            // Task has just re-read the approved screen; it changes before daemon reads.
            *changed.lock().unwrap() = true;
            let (status, result) = call(
                &state,
                "/agent/input",
                Some(json!({"type":"tap","element":3,"snapshot":tree["snapshot"]})),
                Some(&token),
            )
            .await;
            if change == "none" {
                assert_eq!(status, StatusCode::OK, "{result}");
                assert_eq!(*taps.lock().unwrap(), 1, "{result}");
            } else {
                assert_eq!(result["outcome"], "not_sent", "{change}: {result}");
                assert_eq!(*taps.lock().unwrap(), 0, "{change}: {result}");
            }
            iphone_use_mcp::task::clear_approved_press(&token);
        }
    });
}

#[test]
fn owner_foreground_allows_only_one_bound_return_through_server_guard() {
    support::block(async {
        let launches = Arc::new(Mutex::new(0));
        let count = launches.clone();
        let runner = support::mock_runner_with_apps(
            r#"[{"bundleId":"dev.iphoneuse.selfrunner.spike"}]"#,
            move |request, _| {
                let line = request.lines().next().unwrap();
                let reply = |v: Value| Some((Duration::ZERO, v.to_string()));
                if line.starts_with("POST /session ") {
                    return reply(json!({"value":{"sessionId":"S"}}));
                }
                if line.contains("/wda/apps/launch") {
                    *count.lock().unwrap() += 1;
                }
                reply(json!({"value":null}))
            },
        );
        let mut state = support::build_state_with_runner(runner.url());
        let dir = tempfile::tempdir().unwrap();
        Arc::get_mut(&mut state).unwrap().owner_confirm = Arc::new(Mutex::new(
            server::owner_confirm::OwnerConfirm::new(dir.path().into()),
        ));
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ring::{
            rand::SystemRandom,
            signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING},
        };
        let rng = SystemRandom::new();
        let pk = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pk.as_ref(), &rng).unwrap();
        let public = STANDARD.encode(key.public_key().as_ref());
        let signature = STANDARD.encode(
            key.sign(
                &rng,
                format!("iphone-use-owner-key/v1\n{public}").as_bytes(),
            )
            .unwrap()
            .as_ref(),
        );
        state
            .owner_confirm
            .lock()
            .unwrap()
            .bind(&public, Some(&signature))
            .unwrap();
        let _router = server::http::router(state.clone());
        let approved_question =
            json!({"bundle":"com.example.shop","action":"CLICK","confirmation_id":"return-test"});
        let (task_id, answer) =
            iphone_use_mcp::task::owner_confirmation_fixture(approved_question.clone());
        let (_, pending) = call(
            &state,
            &format!("/agent/task/{task_id}/confirm"),
            Some(json!({"approve":true})),
            None,
        )
        .await;
        let owner_id = pending["owner_confirm_id"].as_str().unwrap();
        let sheet = state
            .owner_confirm
            .lock()
            .unwrap()
            .pending()
            .into_iter()
            .find(|p| p.id == owner_id)
            .unwrap();
        let signature =
            STANDARD.encode(key.sign(&rng, sheet.message().as_bytes()).unwrap().as_ref());
        let (status, result) = call(
            &state,
            &format!("/owner/confirm/{owner_id}"),
            Some(json!({"approve":true,"signature":signature})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert!(answer.await.unwrap());
        state
            .runner
            .as_ref()
            .unwrap()
            .lock()
            .await
            .active_bundles()
            .await
            .unwrap();
        let token = iphone_use_mcp::task::bound_step_fixture(approved_question).unwrap();
        for (bundle, capability) in [
            ("com.example.shop", None),
            ("com.example.other", Some(token.as_str())),
        ] {
            let (_, result) = call(
                &state,
                "/agent/input",
                Some(json!({"type":"launch_app","bundle":bundle})),
                capability,
            )
            .await;
            assert_eq!(result["error"], "owner_app_protected", "{result}");
        }
        let (status, result) = call(
            &state,
            "/agent/input",
            Some(json!({"type":"launch_app","bundle":"com.example.shop"})),
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(*launches.lock().unwrap(), 1);
        state
            .runner
            .as_ref()
            .unwrap()
            .lock()
            .await
            .active_bundles()
            .await
            .unwrap();
        let (_, result) = call(
            &state,
            "/agent/input",
            Some(json!({"type":"launch_app","bundle":"com.example.shop"})),
            Some(&token),
        )
        .await;
        assert_eq!(result["error"], "owner_app_protected", "{result}");
        assert_eq!(*launches.lock().unwrap(), 1);
        iphone_use_mcp::task::clear_approved_press(&token);
    });
}

#[test]
fn return_requires_the_approved_unique_focused_field_at_final_dispatch() {
    support::block(async {
        for change in ["none", "switch", "unknown", "multiple", "null_control", "identifier", "duplicate"] {
            let changed = Arc::new(Mutex::new(false));
            let live = changed.clone();
            let keys = Arc::new(Mutex::new(0));
            let count = keys.clone();
            let runner = support::mock_runner_with_apps(
                r#"[{"bundleId":"com.example.shop"}]"#,
                move |request, _| {
                    let line = request.lines().next().unwrap();
                    let reply = |v: Value| Some((Duration::ZERO, v.to_string()));
                    if line.starts_with("POST /session ") {
                        return reply(json!({"value":{"sessionId":"S"}}));
                    }
                    if line.contains("/source") {
                        let fresh = *live.lock().unwrap();
                        let mut first = json!({"type":"XCUIElementTypeTextField","label":"发送","rawIdentifier":"message","value":"hello","isFocused":true});
                        let mut second = json!({"type":"XCUIElementTypeTextField","label":"搜索","rawIdentifier":"search","value":"query","isFocused":false});
                        if fresh {
                            match change {
                                "switch" => { first["isFocused"] = json!(false); second["isFocused"] = json!(true); }
                                "unknown" => { first.as_object_mut().unwrap().remove("isFocused"); }
                                "multiple" => { second["isFocused"] = json!(true); }
                                "identifier" => { first["rawIdentifier"] = json!("other"); }
                                _ => {}
                            }
                        }
                        let mut children = vec![first.clone(), second];
                        // Same page rows before and after; only focus changes.
                        if change == "duplicate" {
                            first["isFocused"] = json!(false);
                            children.push(first);
                        }
                        return reply(json!({"value":{"type":"XCUIElementTypeApplication","label":"商城","bundleId":"com.example.shop","children":children}}));
                    }
                    if line.contains("/actions") && line.starts_with("POST ") {
                        *count.lock().unwrap() += 1;
                    }
                    reply(json!({"value":null}))
                },
            );
            let state = support::build_state_with_runner(runner.url());
            let (status, tree) = call(&state, "/agent/elements", None, None).await;
            assert_eq!(status, StatusCode::OK, "{tree}");
            let mut approved = question(&tree);
            approved["action"] = json!("PRESS_RETURN");
            approved["target"] = Value::Null;
            approved["control"] = if change == "null_control" { Value::Null } else {
                json!({"label":"发送","identifier":"message","kind":"TextField"})
            };
            approved["parameters"] = json!({"operation":"PRESS_RETURN","text":null});
            let token = iphone_use_mcp::task::bound_step_fixture(approved).unwrap();
            *changed.lock().unwrap() = true;
            let (status, result) = call(&state, "/agent/input",
                Some(json!({"type":"key","name":"return"})), Some(&token)).await;
            if change == "none" {
                assert_eq!(status, StatusCode::OK, "{result}");
                assert_eq!(*keys.lock().unwrap(), 1, "{result}");
            } else {
                assert_eq!(result["outcome"], "not_sent", "{change}: {result}");
                assert_eq!(*keys.lock().unwrap(), 0, "{change}: {result}");
            }
            iphone_use_mcp::task::clear_approved_press(&token);
        }
    });
}
