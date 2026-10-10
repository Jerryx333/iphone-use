//! Bounded list collection. Child of http so control guards stay shared.
use super::*;
use crate::wda::ElementRow;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;

const LIMITATIONS: &str = "Accessibility text only; identical text rows may be merged; screenshot-only fields and totals need separate verification; this does not establish business completeness.";

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Request {
    kind: String,
    direction: String,
    max_pages: usize,
    end_label: Option<String>,
    element: Option<usize>,
    snapshot: Option<String>,
}
impl Default for Request {
    fn default() -> Self {
        Self {
            kind: "Cell".into(),
            direction: "down".into(),
            max_pages: 6,
            end_label: None,
            element: None,
            snapshot: None,
        }
    }
}
impl Request {
    fn validate(&self) -> Result<(), String> {
        if !(1..=10).contains(&self.max_pages)
            || !["up", "down"].contains(&self.direction.as_str())
            || self.kind.trim().is_empty()
            || self.end_label.as_ref().is_some_and(|s| s.trim().is_empty())
            || self.element.is_some() != self.snapshot.is_some()
            || self.snapshot.as_ref().is_some_and(|s| s.is_empty())
        {
            return Err("max_pages must be 1..=10; direction up/down; kind/end_label nonempty; element and nonempty snapshot must be paired".into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct Collection {
    rows: Vec<Value>,
    pages: Vec<Value>,
    keys: HashSet<(String, String, Option<String>, Option<String>)>,
    screens: HashSet<Vec<String>>,
    all_settled: bool,
    end_marker_seen: bool,
    snapshot: Option<String>,
    scrolls_attempted: usize,
    scrolls_applied: usize,
    extra: serde_json::Map<String, Value>,
}
impl Collection {
    fn new() -> Self {
        Self {
            all_settled: true,
            ..Self::default()
        }
    }
    fn page(
        &mut self,
        request: &Request,
        rows: &[ElementRow],
        settled: bool,
        moved: Option<bool>,
    ) -> Option<&'static str> {
        self.all_settled &= settled;
        let mut signature = Vec::new();
        let before = self.rows.len();
        let mut seen = 0;
        for (i, row) in rows
            .iter()
            .enumerate()
            .filter(|(i, _)| row_is_on_screen(rows, *i))
        {
            if request
                .end_label
                .as_ref()
                .is_some_and(|s| row.label.contains(s))
            {
                self.end_marker_seen = true;
            }
            if row.kind != request.kind {
                continue;
            }
            let mut labels = Vec::new();
            for label in std::iter::once(&row.label).chain(
                rows[i + 1..subtree_end(rows, i)]
                    .iter()
                    .filter(|r| r.kind == "StaticText")
                    .map(|r| &r.label),
            ) {
                if !label.is_empty() && !labels.contains(label) {
                    labels.push(label.clone());
                }
            }
            let text = labels.join(" · ");
            signature.push(json!([row.kind, text, row.rect]).to_string());
            seen += 1;
            if self.keys.insert((
                row.kind.clone(),
                text.clone(),
                row.identifier.clone(),
                row.value.clone(),
            )) {
                let mut item = json!({"text": text});
                if let Some(value) = &row.value {
                    item["value"] = json!(value);
                }
                if let Some(identifier) = &row.identifier {
                    item["identifier"] = json!(identifier);
                }
                self.rows.push(item);
            }
        }
        signature.sort();
        signature.dedup();
        let repeated = !self.screens.insert(signature);
        self.pages
            .push(json!({"page": self.pages.len()+1, "rows_seen": seen,
            "new_rows": self.rows.len()-before, "settled": settled}));
        if self.end_marker_seen {
            Some("end_marker")
        } else if moved == Some(false) {
            Some("no_scroll_progress")
        } else if repeated {
            Some("repeated_page")
        } else if self.pages.len() >= request.max_pages {
            Some("page_budget")
        } else {
            None
        }
    }
    fn response(&self, reason: &str) -> Value {
        let mut body = json!({"ok": !["error", "app_changed", "alert"].contains(&reason)
                && self.extra.get("outcome").and_then(Value::as_str) != Some("unknown"),
            "rows": self.rows, "pages": self.pages, "stop_reason": reason,
            "end_marker_seen": self.end_marker_seen,
            "coverage_verified": reason == "end_marker" && self.all_settled && self.end_marker_seen,
            "limitations": LIMITATIONS, "snapshot": self.snapshot,
            "scrolls_attempted": self.scrolls_attempted, "scrolls_applied": self.scrolls_applied,
            "retry_safe": self.scrolls_attempted == 0});
        body.as_object_mut().unwrap().extend(self.extra.clone());
        body
    }
}

pub(super) async fn agent_collect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    match agent_auth(&state, &headers) {
        AgentAuth::Locked => {
            return with_security_headers(
                (StatusCode::TOO_MANY_REQUESTS, "too many attempts").into_response(),
            )
        }
        AgentAuth::Denied => {
            return with_security_headers(
                (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            )
        }
        AgentAuth::Ok => {}
    }
    if !has_phone_control_header(&headers) {
        return missing_phone_control_header_response();
    }
    match claim_phone_owner(&state, &headers) {
        Err(refused) => return refused,
        Ok(true) => prewarm_on_new_lease(&state, &headers),
        Ok(false) => {}
    }
    if body.len() > AGENT_ACTIONS_MAX_BODY_BYTES {
        return agent_actions_invalid("collect body too large");
    }
    let request: Request = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => return agent_actions_invalid(e.to_string()),
    };
    if let Err(e) = request.validate() {
        return agent_actions_invalid(e);
    }
    if state.managed_wda_pending {
        return target_not_configured_response();
    }
    if let Some(owner) = trusted_bearer_owner(&state, &headers) {
        crate::advice::reset(&owner);
    }
    let mut collection = Collection::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(40);
    let result = tokio::time::timeout_at(deadline, async {
        let Some(wda) = &state.wda else {
            anyhow::bail!("wda_not_configured");
        };
        state.touch_activity();
        let _priority = state.begin_wda_control();
        let mut w = wda.lock().await;
        if state.released.load(std::sync::atomic::Ordering::Acquire)
            || state.wda_lifecycle.is_transitioning()
        {
            anyhow::bail!("device_not_drivable");
        }
        let focus = engage_agent_focus(&state, &mut w).await;
        if focus_permission_pending(focus.as_ref()).is_some() {
            anyhow::bail!("focus_permission_pending");
        }
        let mut previous: Option<Vec<ElementRow>> = None;
        let mut application = None;
        let mut target: Option<ElementRow> = None;
        let dy = if request.direction == "down" {
            400.0
        } else {
            -400.0
        };
        loop {
            if state.released.load(std::sync::atomic::Ordering::Acquire)
                || state.wda_lifecycle.is_transitioning()
            {
                anyhow::bail!("device_not_drivable");
            }
            w.forget_alert_probe();
            if let Some(alert) = alert_json(probe_alert(&mut w).await) {
                collection.extra.insert("alert".into(), alert);
                return Ok("alert");
            }
            let budget = deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .min(std::time::Duration::from_millis(
                    AGENT_INPUT_SETTLE_DEFAULT_MS,
                ));
            let (observed, report) = settle_and_read_elements(&mut w, budget).await;
            w.forget_alert_probe();
            if let Some(alert) = alert_json(probe_alert(&mut w).await) {
                collection.extra.insert("alert".into(), alert);
                return Ok("alert");
            }
            let Some((snapshot, rows)) = observed else {
                if report.error.is_some() {
                    anyhow::bail!("observation_failed: {:?}", report.error);
                }
                return Ok("deadline");
            };
            remember_element_snapshot(&state, &snapshot, &Arc::new(rows.clone()));
            note_owner_snapshot(&state, &headers, &snapshot);
            collection.snapshot = Some(snapshot.clone());
            if let Some(current) = active_application(&rows) {
                if application.as_ref().is_some_and(|old| old != &current) {
                    return Ok("app_changed");
                }
                application = Some(current);
            }
            // Bind the caller's index only to its original snapshot. Subsequent
            // pages resolve the same container, never reuse a moving row index.
            if previous.is_none() {
                if let Some(index) = request.element {
                    if request.snapshot.as_ref() != Some(&snapshot) {
                        anyhow::bail!("stale_element_snapshot");
                    }
                    target = Some(
                        rows.get(index)
                            .ok_or_else(|| anyhow::anyhow!("element_not_found"))?
                            .clone(),
                    );
                }
            }
            let moved = previous
                .as_ref()
                .and_then(|old| crate::scroll_progress::scroll_anchor_moved(old, &rows, 0.0, dy));
            let stop = collection.page(&request, &rows, report.settled && !report.stale, moved);
            if let Some(error) = report.error {
                anyhow::bail!("observation_failed: {error}");
            }
            if report.stale {
                return Ok("deadline");
            }
            if let Some(reason) = stop {
                return Ok(reason);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok("deadline");
            }
            let mut action = json!({"type":"scroll", "dx":0, "dy":dy});
            if let Some(target) = &target {
                let candidates: Vec<_> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.kind == target.kind
                            && if target.identifier.is_some() {
                                row.identifier == target.identifier
                            } else {
                                row.label == target.label && row.rect == target.rect
                            }
                    })
                    .collect();
                if candidates.len() != 1 {
                    anyhow::bail!("ambiguous_or_missing_scroll_container");
                }
                action["element"] = json!(candidates[0].0);
                action["snapshot"] = json!(snapshot);
            } else {
                action["page"] = json!(true);
            }
            if state.released.load(std::sync::atomic::Ordering::Acquire)
                || state.wda_lifecycle.is_transitioning()
            {
                anyhow::bail!("device_not_drivable");
            }
            collection.scrolls_attempted += 1;
            let mut detail = None;
            let outcome =
                direct_agent_action(&mut w, &state.wda_actionable, &action, &mut detail).await;
            if outcome != WdaControlOutcome::Applied {
                collection
                    .extra
                    .insert("action_detail".into(), json!(format!("{outcome:?}")));
                anyhow::bail!("scroll_failed");
            }
            collection.scrolls_applied += 1;
            previous = Some(rows);
        }
    })
    .await;
    let reason = match result {
        Ok(Ok(reason)) => reason,
        Ok(Err(error)) => {
            collection
                .extra
                .insert("error".into(), json!(error.to_string()));
            "error"
        }
        Err(_) => {
            collection.extra.insert(
                "outcome".into(),
                json!(
                    if collection.scrolls_attempted > collection.scrolls_applied {
                        "unknown"
                    } else {
                        "observed"
                    }
                ),
            );
            "deadline"
        }
    };
    agent_actions_json(
        if reason == "error" {
            StatusCode::CONFLICT
        } else {
            StatusCode::OK
        },
        collection.response(reason),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(text: &str) -> ElementRow {
        ElementRow {
            kind: "Cell".into(),
            label: text.into(),
            rect: [0., 0., 100., 40.],
            ..Default::default()
        }
    }
    #[test]
    fn collect_list_dedup_order() {
        let mut c = Collection::new();
        c.page(
            &Request::default(),
            &[row("a"), row("b"), row("a")],
            true,
            None,
        );
        assert_eq!(c.rows, vec![json!({"text":"a"}), json!({"text":"b"})]);
    }
    #[test]
    fn collect_list_subtree_text() {
        let mut c = Collection::new();
        let mut child = row("b");
        child.kind = "StaticText".into();
        child.depth = 1;
        c.page(
            &Request::default(),
            &[row("a"), child.clone(), child, row("c")],
            true,
            None,
        );
        assert_eq!(c.rows[0]["text"], "a · b");
    }
    #[test]
    fn collect_list_end_marker() {
        let mut c = Collection::new();
        let r = Request {
            end_label: Some("end".into()),
            ..Default::default()
        };
        let mut hidden = row("end");
        hidden.visible = Some(false);
        assert_eq!(c.page(&r, &[hidden], true, None), None);
        assert_eq!(
            c.page(&r, &[row("the end")], true, None),
            Some("end_marker")
        );
    }
    #[test]
    fn collect_list_repeated_page() {
        let mut c = Collection::new();
        let r = Request::default();
        c.page(&r, &[row("a"), row("b")], true, None);
        assert_eq!(
            c.page(&r, &[row("b"), row("a")], true, None),
            Some("repeated_page")
        );
    }
    #[test]
    fn collect_list_no_scroll_progress() {
        let mut c = Collection::new();
        assert_eq!(
            c.page(&Request::default(), &[row("a")], true, Some(false)),
            Some("no_scroll_progress")
        );
    }
    #[test]
    fn collect_list_page_budget() {
        let mut c = Collection::new();
        assert_eq!(
            c.page(
                &Request {
                    max_pages: 1,
                    ..Default::default()
                },
                &[row("a")],
                true,
                None
            ),
            Some("page_budget")
        );
    }
    #[test]
    fn collect_list_coverage_verified() {
        let mut c = Collection::new();
        let r = Request {
            end_label: Some("end".into()),
            ..Default::default()
        };
        c.page(&r, &[row("end")], true, None);
        assert_eq!(c.response("end_marker")["coverage_verified"], true);
        for reason in [
            "page_budget",
            "deadline",
            "repeated_page",
            "no_scroll_progress",
            "alert",
            "app_changed",
        ] {
            assert_eq!(c.response(reason)["coverage_verified"], false);
        }
        c.all_settled = false;
        assert_eq!(c.response("end_marker")["coverage_verified"], false);
    }
    #[test]
    fn collect_list_validation() {
        for s in [
            r#"{"max_pages":0}"#,
            r#"{"max_pages":11}"#,
            r#"{"direction":"left"}"#,
            r#"{"element":1}"#,
        ] {
            assert!(serde_json::from_str::<Request>(s)
                .unwrap()
                .validate()
                .is_err());
        }
        assert!(Request::default().validate().is_ok());
    }
}
