//! Isolated instance: never writes a registry into the user's real state.
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
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tower::ServiceExt;
#[test]
fn body_operator_confirmation_is_ignored_and_signed_intent_runs_once() {
    let dir = tempfile::tempdir().unwrap();
    server::instance::install(server::instance::Instance::derive("", dir.path(), None).unwrap());
    let instance = server::instance::current();
    std::fs::create_dir_all(&instance.state_dir).unwrap();
    std::fs::write(instance.intents_registry(),json!({"version":3,"bridge":{"name":"iU Bridge","required_version":4},"intents":[{"name":"pay","summary":"pay","side_effect":"write","args_schema":{},"returns_schema":{},"min_bridge_version":3,"status":"active","confirm":"operator"}]}).to_string()).unwrap();
    support::block(async {
        let posts = Arc::new(AtomicUsize::new(0));
        let count = posts.clone();
        let runner = support::mock_runner(move |req, _| {
            let line = req.lines().next().unwrap_or("");
            if line.starts_with("POST ") && line.contains("/url ") {
                count.fetch_add(1, Ordering::SeqCst);
            }
            Some((
                Duration::ZERO,
                if line.starts_with("POST /session ") {
                    r#"{"value":{"sessionId":"SESSION"}}"#
                } else {
                    r#"{"value":null}"#
                }
                .into(),
            ))
        });
        let state = support::build_state_with_runner(runner.url());
        let call = |path: String, body: Value| {
            let state = state.clone();
            async move {
                let req = Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("x-phone-control", "1")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap();
                let response = server::http::router(state).oneshot(req).await.unwrap();
                let status = response.status();
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                (status, serde_json::from_slice::<Value>(&bytes).unwrap())
            }
        };
        let rng = SystemRandom::new();
        let pk = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let k =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pk.as_ref(), &rng).unwrap();
        let public = STANDARD.encode(k.public_key().as_ref());
        let proof = STANDARD.encode(
            k.sign(
                &rng,
                format!("iphone-use-owner-key/v1\n{public}").as_bytes(),
            )
            .unwrap(),
        );
        assert_eq!(
            call(
                "/owner/key".into(),
                json!({"public_key":public,"signature":proof})
            )
            .await
            .0,
            StatusCode::OK
        );
        let request = json!({"name":"pay","args":{"amount":1},"operator_confirmed":true});
        let (status, body) = call("/agent/intent".into(), request.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(posts.load(Ordering::SeqCst), 0);
        let id = body["owner_confirm_id"].as_str().unwrap();
        let p = state.owner_confirm.lock().unwrap().pending()[0].clone();
        assert_eq!(call(format!("/owner/confirm/{id}"),json!({"approve":true,"signature":STANDARD.encode(k.sign(&rng,p.message().as_bytes()).unwrap())})).await.0,StatusCode::OK);
        assert_eq!(
            call(
                "/agent/intent".into(),
                json!({"name":"pay","args":{"amount":2},"operator_confirmed":true})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let (status, body) = call("/agent/intent".into(), request.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            call("/agent/intent".into(), request).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(posts.load(Ordering::SeqCst), 1);
    });
}
