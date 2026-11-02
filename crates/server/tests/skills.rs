//! Learned skills over HTTP (`/agent/skills`): a draft stays hidden until
//! accepted, an accepted one rides in the app's `registry` block, and the
//! release reply asks to draft one or to report how following one went.

mod support;

use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use support::{block, build_state, build_state_with_runner, mock_runner};

async fn call(
    state: &std::sync::Arc<server::http::AppState>,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-phone-control", "1");
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

const SKILL: &str = "---\nname: new-note\ndisplay: 新建备忘录\ndescription: 新建一条用户给定内容的备忘录\n\
apps: [com.example.notes]\naliases: [记一下]\n---\n## 入口\n打开备忘录。\n## 步骤\n1. 点右下角新建。\n2. 输入 <内容>。\n\
## 容易出错的地方\n键盘没弹出时先点正文。\n## 怎样算办成\n列表第一条是刚写的内容。\n## 什么时候停下交给用户\n要解锁备忘录时。\n";

fn op(body: serde_json::Value) -> String {
    body.to_string()
}

fn launch_and_taps(bundle: &str, taps: usize) -> String {
    let mut steps =
        vec![serde_json::json!({"kind":"action","action":{"type":"launch_app","bundle":bundle}})];
    for _ in 0..taps {
        steps.push(serde_json::json!({"kind":"action","action":{"type":"key","name":"return"}}));
    }
    serde_json::json!({ "steps": steps }).to_string()
}

#[test]
fn skills_are_drafted_accepted_shown_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("flows");
    std::fs::create_dir(&store).unwrap();
    std::env::set_var("IPHONE_USE_FLOWS_DIR", &store);
    std::env::set_var("IPHONE_USE_FLOWS_NO_SUGGEST", "1");

    block(async {
        let state = build_state(None);
        let (status, json) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"draft","text":"## 入口"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"], "skill_refused");

        let (status, json) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"draft","text":SKILL})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["name"], "new-note");
        assert_eq!(json["status"], "draft");
        // Beside app-notes, outside the flow store.
        assert!(root.path().join("skills/drafts/new-note.md").is_file());
        let (_, json) = call(&state, "GET", "/agent/skills?bundle=com.example.notes", "").await;
        assert_eq!(json["skills"][0]["status"], "draft", "{json}");

        let runner = mock_runner(|request, _| {
            if request.starts_with("POST /session ") {
                return Some((
                    Duration::ZERO,
                    r#"{"value":{"sessionId":"SESSION"}}"#.to_string(),
                ));
            }
            Some((Duration::ZERO, r#"{"value":null}"#.to_string()))
        });
        let driving = build_state_with_runner(runner.url());
        let (_, json) = call(
            &driving,
            "POST",
            "/agent/actions",
            &launch_and_taps("com.example.notes", 0),
        )
        .await;
        assert!(
            json["registry"].get("skills").is_none(),
            "a draft is not used: {json}"
        );
        assert_eq!(json["registry"]["skill_drafts_waiting"], 1, "{json}");

        // A session that drove the app for a while is asked to draft one.
        let (_, json) = call(
            &driving,
            "POST",
            "/agent/actions",
            &launch_and_taps("com.example.other", 6),
        )
        .await;
        assert_eq!(json["ok"], true, "{json:#}");
        let (status, json) = call(&driving, "POST", "/agent/owner", r#"{"release":true}"#).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["learn"]["draft"]["app"], "com.example.other", "{json}");
        assert!(json["learn"]["draft_message"]
            .as_str()
            .unwrap()
            .contains("op=accept"));
        let (_, json) = call(&driving, "POST", "/agent/owner", r#"{"release":true}"#).await;
        assert!(json.get("learn").is_none(), "offered once: {json}");

        let (status, json) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"accept","name":"new-note"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["status"], "trial");
        let (_, json) = call(
            &driving,
            "POST",
            "/agent/actions",
            &launch_and_taps("com.example.notes", 6),
        )
        .await;
        assert_eq!(json["registry"]["skills"][0]["name"], "new-note", "{json}");
        assert!(json["registry"]["skills_hint"]
            .as_str()
            .unwrap()
            .contains("op=result"));

        // Reading it marks it followed: the release asks for the result, not a new draft.
        let (_, json) = call(&driving, "GET", "/agent/skills?name=new-note", "").await;
        assert_eq!(json["text"], SKILL.trim_end(), "{json}");
        let (_, json) = call(&driving, "POST", "/agent/owner", r#"{"release":true}"#).await;
        assert_eq!(json["learn"]["report"][0], "new-note", "{json}");
        assert!(json["learn"].get("draft").is_none(), "{json}");

        let (_, json) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"result","name":"new-note","result":"verified"})),
        )
        .await;
        assert_eq!(json["status"], "active", "{json}");
        let (status, json) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"result","name":"new-note","result":"maybe"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        for _ in 0..2 {
            call(
                &state,
                "POST",
                "/agent/skills",
                &op(serde_json::json!({"op":"result","name":"new-note","result":"failed"})),
            )
            .await;
        }
        let (_, json) = call(&state, "GET", "/agent/skills?name=new-note", "").await;
        assert_eq!(json["status"], "suspended", "{json}");
        let (_, json) = call(
            &driving,
            "POST",
            "/agent/actions",
            &launch_and_taps("com.example.notes", 0),
        )
        .await;
        assert!(
            json["registry"].get("skills").is_none(),
            "suspended is hidden: {json}"
        );

        let (status, _) = call(
            &state,
            "POST",
            "/agent/skills",
            &op(serde_json::json!({"op":"delete","name":"new-note"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&state, "GET", "/agent/skills?name=new-note", "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    });
}
