use base64::{engine::general_purpose::STANDARD, Engine};
use phone_gateway::{
    config,
    output::{
        render,
        OutputFormat::{Compact, Json},
    },
};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

#[test]
fn compact_truncated_accepts_numbers_and_bools_or_reports_unknown() {
    for action in ["read", "wait_for"] {
        for (flag, expected) in [
            (Some(json!(1)), "true"),
            (Some(json!(0)), "false"),
            (Some(json!(true)), "true"),
            (Some(json!(false)), "false"),
            (None, "unknown"),
            (Some(json!(null)), "unknown"),
            (Some(json!(2)), "unknown"),
        ] {
            let mut response = json!({"ok":true,"result":{"nodes":[]}});
            if let Some(flag) = flag {
                response["result"]["truncated"] = flag;
            }
            let out = render(action, response, Compact, 0, None).unwrap();
            assert_eq!(
                out.lines().nth(1).unwrap(),
                format!("pid=null snapshot= truncated={expected} nodes=0 shown=0")
            );
        }
    }
}

#[test]
fn compact_read_format_cleaning_indices_and_types() {
    let response = json!({"ok":true,"result":{"pid":37,"snapshot_id":"snap-1","truncated":true,"bundle":"app","nodes":[
        {"type":1},
        {"i":7,"type":9,"label":"打\u{200b}开\n设置\u{202e}","identifier":"go","value":"a\u{feff}b\r\nc","frame":{"x":1.6,"y":-2.6,"w":49.5,"h":20.1}},
        {"type":999,"identifier":"unknown"}
    ]}});
    let out = render("read", response, Compact, 12, None).unwrap();
    assert_eq!(out, "ok read 12ms\npid=37 snapshot=snap-1 truncated=true nodes=3 shown=2\n{\"bundle\":\"app\"}\n[7] Button \"打开 设置\" id=go val=\"ab  c\" @2,-3,50,20\n[2] t999 \"\" id=unknown val=\"\" @0,0,0,0");
    for (n, name) in [
        (1, "Other"),
        (2, "Application"),
        (4, "Window"),
        (9, "Button"),
        (21, "NavigationBar"),
        (40, "Switch"),
        (43, "Image"),
        (45, "SearchField"),
        (48, "StaticText"),
        (49, "TextField"),
        (50, "SecureTextField"),
        (75, "Cell"),
    ] {
        let out = render(
            "read",
            json!({"ok":true,"result":{"nodes":[{"type":n,"label":"x"}]}}),
            Compact,
            0,
            None,
        )
        .unwrap();
        assert!(out.contains(&format!("[0] {name} \"x\"")));
    }
}

#[test]
fn compact_limits_unicode_and_wait_for_singular_node() {
    let nodes: Vec<_> = (0..405)
        .map(|i| json!({"i":i,"type":48,"value":"中".repeat(100)}))
        .collect();
    let out = render(
        "read",
        json!({"ok":true,"result":{"nodes":nodes}}),
        Compact,
        0,
        None,
    )
    .unwrap();
    assert!(out.contains("nodes=405 shown=400"));
    assert_eq!(out.lines().filter(|l| l.starts_with('[')).count(), 400);
    assert!(out.ends_with("... 5 more"));
    assert!(out.contains(&format!("val=\"{}\"", "中".repeat(80))));
    assert!(!out.contains(&"中".repeat(81)));
    let out = render(
        "wait_for",
        json!({"ok":true,"result":{"found":true,"node":{"i":17,"type":49,"label":"Search"}}}),
        Compact,
        3,
        None,
    )
    .unwrap();
    assert!(out.contains("nodes=1 shown=1\n{\"found\":true}\n[17] TextField"));
    let out = render(
        "status",
        json!({"ok":true,"result":{"message":"中".repeat(3000)}}),
        Compact,
        0,
        None,
    )
    .unwrap();
    assert_eq!(out.lines().nth(1).unwrap().chars().count(), 2000);
}

#[test]
fn screenshot_saved_without_base64_and_json_compatibility() {
    let dir = std::env::temp_dir().join(format!("gateway-output-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("shot.png");
    let encoded = STANDARD.encode(b"fake png bytes");
    for key in ["png", "image", "base64", "data"] {
        let response = json!({"ok":true,"result":{key:encoded,"width":393,"height":852}});
        assert_eq!(
            render("screenshot", response.clone(), Json, 0, None).unwrap(),
            response.to_string()
        );
        let out = render("screenshot", response, Compact, 24, Some(&path)).unwrap();
        assert_eq!(
            out,
            format!("ok screenshot 24ms saved={} 393x852", path.display())
        );
        assert!(!out.contains(&encoded));
        assert_eq!(std::fs::read(&path).unwrap(), b"fake png bytes");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let out = render(
        "screenshot",
        json!({"ok":true,"result":{"png":encoded,"width":1,"height":2}}),
        Compact,
        0,
        None,
    )
    .unwrap();
    let saved = out
        .strip_prefix("ok screenshot 0ms saved=")
        .unwrap()
        .strip_suffix(" 1x2")
        .unwrap();
    let saved = std::path::Path::new(saved);
    assert!(saved
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("shot-"));
    assert_eq!(
        std::fs::metadata(saved.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(std::fs::read(saved).unwrap(), b"fake png bytes");
    std::fs::remove_file(saved).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn screenshot_error_and_invalid_unlock_preserve_protocol_exit_status() {
    assert!(render(
        "screenshot",
        json!({"ok":false,"error":{"code":"expired"}}),
        Json,
        8,
        Some(std::path::Path::new("unused.png")),
    )
    .is_err());
    assert_eq!(
        render(
            "screenshot",
            json!({"ok":false,"error":{"code":"expired"}}),
            Compact,
            8,
            None
        )
        .unwrap(),
        "error expired 8ms"
    );
    let dir = std::env::temp_dir().join(format!("gateway-cli-{}", uuid::Uuid::new_v4()));
    // Invalid arguments never need a listener or a keychain lookup.
    std::fs::create_dir(&dir).unwrap();
    config::private_write(
        &dir.join("client-token"),
        STANDARD.encode([0; 32]).as_bytes(),
    )
    .unwrap();
    for format in ["json", "compact"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_phone-gateway"))
            .env("PHONE_GATEWAY_HOME", &dir)
            .args([
                "cmd",
                "unlock",
                "--args",
                r#"{"passcode":"2580"}"#,
                "--format",
                format,
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(!String::from_utf8_lossy(&out.stdout).contains("2580"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("2580"));
        assert!(String::from_utf8_lossy(&out.stdout).contains("invalid_args"));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
