//! Short-lived replay authority. Tokens never enter action bodies or receipts.
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub const HEADER: &str = "x-phone-flow-grant";
const TTL: Duration = Duration::from_secs(120);
const MAX_GRANTS: usize = 1024;

type GrantEntry = ([u8; 32], [u8; 32], Instant, Option<serde_json::Value>);

#[derive(Default)]
pub struct FlowGrants {
    entries: Vec<GrantEntry>,
}

fn digest(body: &str) -> Option<[u8; 32]> {
    // serde_json's ordered object maps canonicalize key order and whitespace.
    let mut value: serde_json::Value = serde_json::from_str(body).ok()?;
    fn sort(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for child in map.values_mut() {
                    sort(child);
                }
                let sorted: std::collections::BTreeMap<_, _> =
                    std::mem::take(map).into_iter().collect();
                map.extend(sorted);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(sort),
            _ => {}
        }
    }
    sort(&mut value);
    Some(Sha256::digest(serde_json::to_vec(&value).ok()?).into())
}

impl FlowGrants {
    pub fn issue(&mut self, body: &str) -> Option<String> {
        self.issue_at(body, Instant::now())
    }

    /// Explicit clock for deterministic expiry tests; never caller controlled over HTTP.
    #[doc(hidden)]
    pub fn issue_at(&mut self, body: &str, now: Instant) -> Option<String> {
        self.entries.retain(|entry| entry.2 > now);
        if self.entries.len() >= MAX_GRANTS {
            return None;
        }
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).ok()?;
        let token: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        self.entries.push((
            Sha256::digest(token.as_bytes()).into(),
            digest(body)?,
            now + TTL,
            None,
        ));
        Some(token)
    }

    pub fn issue_flow(&mut self, body: &str, flow: serde_json::Value) -> Option<String> {
        let token = self.issue(body)?;
        self.entries.last_mut()?.3 = Some(flow);
        Some(token)
    }
    pub fn consume_flow(&mut self, token: &str, body: &str) -> Option<serde_json::Value> {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let i = self.entries.iter().position(|e| e.0 == candidate)?;
        let (_, expected, expires, flow) = self.entries.swap_remove(i);
        if expires > Instant::now() && digest(body) == Some(expected) {
            flow
        } else {
            None
        }
    }

    pub fn consume(&mut self, token: &str, body: &str) -> bool {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let index = self.entries.iter().position(|entry| {
            ring::constant_time::verify_slices_are_equal(&entry.0, &candidate).is_ok()
        });
        let Some(index) = index else {
            return false;
        };
        // Even a mismatched body consumes the capability, before any dispatch.
        let (_, expected, expires, _) = self.entries.swap_remove(index);
        expires > Instant::now() && digest(body) == Some(expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expiration_binding_and_single_use() {
        let mut grants = FlowGrants::default();
        let body = r#"{"steps":[],"observe":false}"#;
        let expired = grants.issue_at(body, Instant::now() - TTL).unwrap();
        assert!(!grants.consume(&expired, body));
        let token = grants.issue(body).unwrap();
        assert!(!grants.consume(&token, r#"{"steps":[1]}"#));
        assert!(!grants.consume(&token, body));
        let token = grants.issue(body).unwrap();
        assert!(grants.consume(&token, r#"{ "observe": false, "steps": [] }"#));
        assert!(!grants.consume(&token, body));
        assert!(!grants.consume("1", body));
    }
}
