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
use std::sync::Arc;
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
fn signed_task_confirmation_answers_only_the_waiting_question() {
    support::block(async {
        let mut state = support::build_state(None);
        let dir = tempfile::tempdir().unwrap();
        Arc::get_mut(&mut state).unwrap().owner_confirm =
            Arc::new(std::sync::Mutex::new(OwnerConfirm::new(dir.path().into())));
        let k = key();
        bind(&mut state.owner_confirm.lock().unwrap(), &k);
        let _router = server::http::router(state.clone());
        let (id, mut receiver) = iphone_use_mcp::task::owner_confirmation_fixture(
            json!({"action":"CLICK","target":"立即购买","confirmation_id":"one"}),
        );
        let (status, body) = call(
            &state,
            &format!("/agent/task/{id}/confirm"),
            json!({"approve":true}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert!(receiver.try_recv().is_err());
        let owner_id = body["owner_confirm_id"].as_str().unwrap();
        let p = state
            .owner_confirm
            .lock()
            .unwrap()
            .pending()
            .into_iter()
            .find(|p| p.id == owner_id)
            .unwrap();
        let (status, body) = call(
            &state,
            &format!("/owner/confirm/{owner_id}"),
            json!({"approve":true,"signature":sign(&k,p.message().as_bytes())}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(receiver.await.unwrap());
        assert_eq!(
            call(
                &state,
                &format!("/owner/confirm/{owner_id}"),
                json!({"approve":true,"signature":sign(&k,p.message().as_bytes())})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        // A stale sheet must not answer a later question, even for the same task.
        let (later_id, mut later_receiver) = iphone_use_mcp::task::owner_confirmation_fixture(
            json!({"action":"CLICK","target":"立即购买","confirmation_id":"two"}),
        );
        let stale_object = json!({"task_id":later_id,"operation":{"action":"CLICK","target":"立即购买","confirmation_id":"old"}});
        let stale_id = state
            .owner_confirm
            .lock()
            .unwrap()
            .request("task_press", stale_object, "立即购买")
            .unwrap();
        let stale = state
            .owner_confirm
            .lock()
            .unwrap()
            .pending()
            .into_iter()
            .find(|p| p.id == stale_id)
            .unwrap();
        let (status, body) = call(
            &state,
            &format!("/owner/confirm/{stale_id}"),
            json!({"approve":true,"signature":sign(&k,stale.message().as_bytes())}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"], "owner_confirmation_object_changed");
        assert!(later_receiver.try_recv().is_err());
        iphone_use_mcp::task::answer(&later_id, false).unwrap();
    });
}
