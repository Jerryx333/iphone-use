use super::*;

struct Admission<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn valid_press(args: &serde_json::Value) -> bool {
    args.as_object().is_some_and(|m| {
        m.keys()
            .all(|k| ["button", "strategy", "shortcut_name"].contains(&k.as_str()))
    }) && matches!(
        args["button"].as_str(),
        Some("home" | "volume_up" | "volume_down" | "lock")
    ) && args
        .get("shortcut_name")
        .is_none_or(|name| args["button"] == "lock" && name == "锁屏")
        && args.get("strategy").is_none_or(|strategy| {
            args["button"] == "lock"
                && matches!(
                    strategy.as_str(),
                    Some("auto" | "shortcut" | "hid" | "device_event" | "press_lock")
                )
        })
}

fn raw_matches(node: &serde_json::Value, args: &serde_json::Value, rects: &mut Vec<[f64; 4]>) {
    if args["label"]
        .as_str()
        .is_some_and(|s| node["label"].as_str() == Some(s))
        || args["identifier"].as_str().is_some_and(|s| {
            ["identifier", "rawIdentifier", "name"]
                .iter()
                .any(|key| node[*key].as_str() == Some(s))
        })
    {
        let r = &node["rect"];
        rects.push(["x", "y", "width", "height"].map(|key| r[key].as_f64().unwrap_or(f64::NAN)));
    }
    if let Some(children) = node["children"].as_array() {
        for child in children {
            raw_matches(child, args, rects);
        }
    }
}

/// Read-only admission, called while the gateway holds its execution gate.
pub(super) async fn command(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(mut request): axum::Json<serde_json::Value>,
) -> Response {
    // Reserved gateway transport flag: never part of what the owner approves.
    if let Some(args) = request["args"].as_object_mut() {
        args.remove("gateway_guarded");
    }
    if let Some(refused) = auth_refusal(agent_auth(&state, &headers)) {
        return refused;
    }
    if state
        .gateway_guard_busy
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return agent_json_response(
            StatusCode::TOO_MANY_REQUESTS,
            serde_json::json!({"error":"guard_busy"}),
        );
    }
    let _admission = Admission(&state.gateway_guard_busy);
    let unavailable = || {
        agent_json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"error":"guard_unavailable"}),
        )
    };
    let invalid = || {
        agent_json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error":"invalid_args"}),
        )
    };
    let Some(runner) = &state.runner else {
        return unavailable();
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        let args = &request["args"];
        let kind = request["action"].as_str().unwrap_or("");
        if kind == "press" && !valid_press(args) { return invalid(); }
        let mut action = match kind {
            "tap" => serde_json::json!({"type":"tap"}),
            "type" => serde_json::json!({"type":"text","text":args["text"]}),
            "swipe" => serde_json::json!({"type":"swipe"}),
            "press" => serde_json::json!({"type":"shortcut","name":args["button"]}),
            "launch" => serde_json::json!({"type":"launch_app","bundle":args["bundle_id"]}),
            "open_url" => serde_json::json!({"type":"open_url","url":args["url"]}),
            _ => return invalid(),
        };
        let mut client = runner.lock().await;
        if state.managed_runner_pending || state.runner_lifecycle.is_transitioning()
            || state.released.load(std::sync::atomic::Ordering::Acquire) { return unavailable(); }
        client.forget_tree();
        if client.active_bundles().await.is_err() || args.get("snapshot_id").is_some() { return unavailable(); }
        if let Err(refusal) = protect_owner_app(&client, &action) { return guard_refusal(&refusal, false); }
        let policy = crate::guard::Policy::get();
        for checked in [policy.check_launch(&action), policy.check_url(&action)] {
            if let Err(refusal) = checked { return guard_refusal(&refusal, false); }
        }
        if let Err(refusal) = guard_foreground(&mut client, &action).await { return guard_refusal(&refusal, false); }
        let mut observed = serde_json::json!([]);
        let mut checked = crate::guard::check_sensitive_request(&action);
        if ["tap", "type", "swipe"].contains(&kind) {
            let (snapshot, raw) = match tokio::time::timeout(GUARD_SENSITIVE_READ_TIMEOUT, client.gateway_element_snapshot()).await {
                Ok(Ok(snapshot)) => snapshot, _ => return unavailable(),
            };
            let rows = &snapshot.rows;
            if kind != "type" && !snapshot.coverage.is_complete() { return unavailable(); }
            let page = rows.iter().map(|r| (&r.kind, &r.label, &r.value)).collect::<Vec<_>>();
            observed = serde_json::json!([{"page_digest":crate::owner_confirm::digest(&serde_json::json!(page)),"coverage":snapshot.coverage.as_str(),"raw_digest":crate::owner_confirm::digest(&raw)}]);
            let mut indices = Vec::new();
            if kind == "type" {
                // A missing focus is not proof that the phone will type nowhere.
                if !snapshot.coverage.is_complete() || crate::guard::focused_field(rows).is_none() {
                    checked = Err(crate::guard::Refusal::SensitiveField("输入目标或读屏覆盖无法核实".into()));
                }
                if let Some(i) = rows.iter().position(|row| row.focused == Some(true)) { indices.push(i); }
                for (i, row) in rows.iter().enumerate() {
                    if let Err(refusal) = crate::guard::check_typed_row(row) { checked = Err(refusal); indices.push(i); }
                }
                // Same rule as /control: a newline submits, so the keyboard's
                // action keys and the focused field must not pay.
                if let crate::guard::SensitiveRead::Focused { submits: true } = crate::guard::sensitive_read(&action) {
                    for (i, row) in rows.iter().enumerate() {
                        if matches!(row.kind.as_str(), "Button" | "Key" | "Keyboard") || row.focused == Some(true) {
                            indices.push(i);
                        }
                    }
                    if let Err(refusal) = crate::guard::check_return(rows) { checked = Err(refusal); }
                }
                indices.sort_unstable();
                indices.dedup();
            } else if kind == "tap" && args.get("x").is_none() && args.get("y").is_none() {
                // Ignore matching index: normalized and phone candidate orders differ.
                let mut rects = Vec::new();
                raw_matches(&raw, args, &mut rects);
                for (i, row) in rows.iter().enumerate() {
                    if args["label"].as_str().is_some_and(|s| row.label == s)
                        || args["identifier"].as_str().is_some_and(|s| row.identifier.as_deref() == Some(s))
                        || rects.contains(&row.rect) { indices.push(i); }
                }
                if indices.is_empty() || rects.iter().any(|r| !rows.iter().any(|row| row.rect == *r)) { return unavailable(); }
                for &i in &indices {
                    if let Err(refusal) = crate::guard::check_pressed_row(rows, i) { checked = Err(refusal); }
                }
            } else {
                let (w, h) = match client.window_size().await { Ok(size) => size, Err(_) => return unavailable() };
                if !w.is_finite() || !h.is_finite() || w <= 0.0 || h <= 0.0 { return unavailable(); }
                let point = |p: &serde_json::Value| -> Option<(f64, f64)> {
                    let (x,y) = (p["x"].as_f64()?, p["y"].as_f64()?);
                    (x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0 && x < w && y < h).then_some((x/w,y/h))
                };
                if kind == "tap" {
                    let Some((x,y)) = point(args) else { return unavailable(); };
                    action["x"] = x.into(); action["y"] = y.into();
                } else {
                    let (Some((x1,y1)),Some((x2,y2))) = (point(&args["from"]),point(&args["to"])) else { return unavailable(); };
                    action["x1"] = x1.into(); action["y1"] = y1.into();
                    action["x2"] = x2.into(); action["y2"] = y2.into();
                }
                // Reuse /control's short-swipe classification and point checks.
                if let crate::guard::SensitiveRead::Points(points) = crate::guard::sensitive_read(&action) {
                    for (x,y) in points {
                        let (control, text) = crate::guard::point_rows(rows, x*w, y*h);
                        indices.extend(control.into_iter().chain(text));
                        match crate::guard::check_point(rows, x*w, y*h) {
                            Some(Err(refusal)) => checked = Err(refusal),
                            None => { if let Err(refusal) = guard_sensitive_unverified(&mut client).await { checked = Err(refusal); } },
                            _ => {}
                        }
                    }
                }
            }
            for i in indices {
                let row = &rows[i];
                observed.as_array_mut().unwrap().push(serde_json::json!({"label":row.label,"identifier":row.identifier,"rect":row.rect,"kind":row.kind,"secure":crate::guard::check_typed_row(row).is_err()}));
            }
        }
        let mut binding = if args.is_object() { args.clone() } else { serde_json::json!({}) };
        binding["type"] = request["action"].clone();
        binding["gateway_request_digest"] = serde_json::json!(crate::owner_confirm::digest(&request));
        if let Err(refusal) = checked {
            let object = control_confirmation_object(&binding, &refusal, &observed, client.confirmation_foreground());
            if !recover(state.owner_confirm.lock()).consume("gateway_press", &object) {
                return owner_required(&state, "gateway_press", object, refusal.to_json()["label"].as_str().unwrap_or("敏感控件"));
            }
        }
        agent_json_response(StatusCode::OK, serde_json::json!({"allow":true}))
    }).await;
    result.unwrap_or_else(|_| unavailable())
}
