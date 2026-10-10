//! The task engine against a fake daemon and a fake assist model: steps, the
//! confirm gate before a payment, and the summary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iphone_use_mcp::client::DaemonClient;
use iphone_use_mcp::task::{self, AssistEndpoint, ModelsConfig, TaskRequest};
use serde_json::{json, Value};

/// A tiny HTTP server: `respond(method, path, body) -> (status, json body)`.
fn serve(respond: impl Fn(&str, &str, &str) -> (u16, String) + Send + 'static) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut data = Vec::new();
            let mut buffer = [0u8; 65536];
            // Read headers, then the body by Content-Length.
            loop {
                let Ok(n) = stream.read(&mut buffer) else { break };
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&data);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if data.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&data).to_string();
            let line = text.lines().next().unwrap_or("").to_string();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let (status, reply) = respond(&method, &path, &body);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
        }
    });
    url
}

struct Phone {
    paid: Mutex<bool>,
    inputs: Mutex<Vec<Value>>,
}

fn fake_daemon(phone: Arc<Phone>) -> String {
    serve(move |method, path, body| {
        let reply = match (method, path.split('?').next().unwrap_or("")) {
            ("GET", "/agent/status") => json!({"ok": true, "drivable": true, "backend": "direct"}),
            ("GET", "/agent/elements") => {
                if *phone.paid.lock().unwrap() {
                    json!({"snapshot": "s2", "elements": [
                        {"kind": "Application", "label": "拼多多"},
                        {"kind": "StaticText", "label": "支付成功"}
                    ]})
                } else {
                    json!({"snapshot": "s1", "elements": [
                        {"kind": "Application", "label": "拼多多"},
                        {"kind": "StaticText", "label": "凑单团价 ¥55.51"},
                        {"kind": "Button", "label": "确认支付 ¥55.51", "rect": [20, 800, 400, 50]}
                    ]})
                }
            }
            ("POST", "/agent/input") => {
                let action: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                if action["type"] == "tap" {
                    *phone.paid.lock().unwrap() = true;
                }
                phone.inputs.lock().unwrap().push(action);
                json!({"ok": true})
            }
            ("GET", "/agent/screenshot") => return (200, "PNG".into()),
            ("POST", "/agent/owner") => json!({"ok": true, "owner": null}),
            _ => return (404, json!({"ok": false}).to_string()),
        };
        (200, reply.to_string())
    })
}

/// An OpenAI-format model: taps the payment button, then says DONE, and
/// writes the summary.
fn fake_model(phone: Arc<Phone>) -> String {
    serve(move |_, _, body| {
        let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let system = request["messages"][0]["content"].as_str().unwrap_or("");
        let content = if system.contains("saved phone automation") {
            r#"{"flow": null}"#.to_string()
        } else if system.contains("operate a real iPhone") {
            if *phone.paid.lock().unwrap() {
                r#"{"operation": "DONE", "target": null, "text": null, "reason": "paid"}"#.to_string()
            } else {
                r#"{"operation": "CLICK", "target": 0, "text": null, "reason": "pay as asked"}"#.to_string()
            }
        } else {
            "已用免密支付付款 55.51 元，显示支付成功。".to_string()
        };
        (200, json!({"choices": [{"message": {"content": content}}]}).to_string())
    })
}

fn request(model_url: &str) -> TaskRequest {
    TaskRequest {
        goal: "在拼多多买下这把指甲刀".into(),
        models: Some(ModelsConfig {
            jev: None,
            text: None,
            assist: vec![AssistEndpoint {
                format: task::Format::Openai,
                base_url: model_url.to_string(),
                key: String::new(),
                model: "fake".into(),
            }],
        }),
        ..Default::default()
    }
}

async fn wait_for(id: &str, status: &str) -> Value {
    for _ in 0..200 {
        let task = task::get(id, None).unwrap();
        if task["status"] == status {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("task never reached {status}: {}", task::get(id, None).unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_waits_for_the_owner_then_runs_once_and_is_summarized() {
    // No saved flows anywhere.
    let flows = tempfile::tempdir().unwrap();
    std::env::set_var("IPHONE_USE_FLOWS_DIR", flows.path());

    // Approved.
    let phone = Arc::new(Phone { paid: Mutex::new(false), inputs: Mutex::new(Vec::new()) });
    let daemon_url = fake_daemon(phone.clone());
    let model_url = fake_model(phone.clone());
    let id = task::start(request(&model_url), |owner| DaemonClient::new(daemon_url.clone(), None).with_owner(owner))
        .ok()
        .unwrap();
    let waiting = wait_for(&id, "needs_confirm").await;
    assert_eq!(waiting["confirm"]["target"], "确认支付 ¥55.51", "{waiting}");
    assert!(phone.inputs.lock().unwrap().is_empty(), "nothing tapped before the owner answers");
    assert!(matches!(task::start(request(&model_url), |o| DaemonClient::new(daemon_url.clone(), None).with_owner(o)), Err(task::StartError::Busy(_))));
    task::answer(&id, true).unwrap();
    let done = wait_for(&id, "done").await;
    let taps: Vec<Value> = phone.inputs.lock().unwrap().iter().filter(|a| a["type"] == "tap").cloned().collect();
    assert_eq!(taps.len(), 1, "the approved step ran exactly once: {taps:?}");
    assert_eq!(taps[0]["element"], 2, "the payment button's tree row");
    let steps = done["steps"].as_array().unwrap();
    assert!(steps.iter().any(|s| s["confirmed"] == true && s["by"] == "assist"), "{done}");
    assert_eq!(done["summary"], "已用免密支付付款 55.51 元，显示支付成功。");
    assert!(done["confirm"].is_null());

    // Declined: nothing is tapped and the task ends blocked.
    let phone = Arc::new(Phone { paid: Mutex::new(false), inputs: Mutex::new(Vec::new()) });
    let daemon_url = fake_daemon(phone.clone());
    let model_url = fake_model(phone.clone());
    let id = task::start(request(&model_url), |owner| DaemonClient::new(daemon_url.clone(), None).with_owner(owner))
        .ok()
        .unwrap();
    wait_for(&id, "needs_confirm").await;
    task::answer(&id, false).unwrap();
    let blocked = wait_for(&id, "blocked").await;
    assert!(phone.inputs.lock().unwrap().iter().all(|a| a["type"] != "tap"), "declined means not tapped");
    assert!(blocked["error"].as_str().unwrap().contains("did not allow"), "{blocked}");
    assert_eq!(task::list()["tasks"].as_array().unwrap().len(), 2);
}
