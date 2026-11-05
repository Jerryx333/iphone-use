//! Owner authority is independent of HTTP credentials. Local arbitrary shell
//! execution (including editing this daemon or its state) is outside the threat model.
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant},
};

const TTL: Duration = Duration::from_secs(120);
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn digest(value: &Value) -> String {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(m) => serde_json::to_value(
                m.iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            )
            .unwrap(),
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            _ => v.clone(),
        }
    }
    hex(&Sha256::digest(
        serde_json::to_vec(&canonical(value)).unwrap(),
    ))
}
fn random() -> Result<String, &'static str> {
    let mut b = [0; 32];
    getrandom::fill(&mut b).map_err(|_| "random_failed")?;
    Ok(hex(&b))
}
fn quote(label: &str) -> String {
    let count = label.chars().count();
    let mut escaped = String::new();
    for c in label.chars().take(300) {
        if c.is_control()
            || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{2028}' | '\u{2029}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}')
        {
            escaped.push_str(&format!("\\u{{{:04x}}}", c as u32));
        } else {
            escaped.push(c);
        }
    }
    let mut result = serde_json::to_string(&escaped).unwrap();
    if count > 300 {
        result.push_str(&format!("…（已截断，共 {count} 字）"));
    }
    result
}
// Only daemon-authored prose; untrusted values are bounded JSON quotations.
fn value_quote(v: &Value) -> String {
    quote(
        &v.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string()),
    )
}
fn action_details(action: &Value, observed_secure: bool, flow: bool) -> String {
    let null = Value::Null;
    let kind = action
        .get("type")
        .or_else(|| action.get("kind"))
        .unwrap_or(&null);
    let secure = observed_secure;
    let mut parts = vec![format!("动作类型：{}", value_quote(kind))];
    if let Some(fields) = action.as_object() {
        for (key, value) in fields {
            if key == "type" || (key == "kind" && action.get("type").is_none()) {
                continue;
            }
            if secure && matches!(key.as_str(), "text" | "value") && value.is_string() {
                parts.push(format!(
                    "{}：输入 {} 个字符（已隐藏）",
                    quote(key),
                    value.as_str().unwrap().chars().count()
                ));
            } else if key == "locator" && value.is_object() {
                for (k, v) in value.as_object().unwrap() {
                    if secure && k == "value" && v.is_string() {
                        parts.push(format!(
                            "定位 {}：输入 {} 个字符（已隐藏）",
                            quote(k),
                            v.as_str().unwrap().chars().count()
                        ));
                    } else {
                        parts.push(format!("定位 {}：{}", quote(k), value_quote(v)));
                    }
                }
            } else {
                parts.push(format!("{}：{}", quote(key), value_quote(value)));
            }
        }
    }
    let mut checked = action.clone();
    if checked.get("type").is_none() {
        checked["type"] = kind.clone();
    }
    if flow
        && (crate::guard::check_sensitive_request(&checked).is_err()
            || crate::guard::sensitive_read(&checked) != crate::guard::SensitiveRead::None)
    {
        parts.push("批准后：此步骤将不经付款/密码拦截直接执行".into());
    }
    parts.join("；")
}
fn confirmation_details(kind: &str, object: &Value) -> Vec<String> {
    match kind {
        "flow" => {
            let mut d = vec![format!(
                "流程 id：{}；名称：{}",
                value_quote(&object["id"]),
                value_quote(&object["flow"]["name"])
            )];
            let steps = object["actions"]["steps"]
                .as_array()
                .or_else(|| object["flow"]["steps"].as_array());
            if let Some(steps) = steps {
                for (i, step) in steps.iter().enumerate() {
                    let action = step.get("action").filter(|v| v.is_object()).unwrap_or(step);
                    d.push(format!(
                        "步骤 {}：{}",
                        i + 1,
                        action_details(action, false, true)
                    ));
                }
            }
            d.push("批准后此版本流程以后每次回放都跳过付款/密码拦截，流程内容一改即失效".into());
            d
        }
        "task_press" => vec![
            format!("任务目标：{}", value_quote(&object["goal"])),
            format!(
                "当前控件标签：{}",
                value_quote(&object["operation"]["target"])
            ),
            format!("操作类型：{}", value_quote(&object["operation"]["action"])),
        ],
        "control_press" => {
            let mut d = vec![action_details(
                &object["action"],
                object["controls"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row["secure"] == true)),
                false,
            )];
            if let Some(labels) = object["target"].as_array() {
                for label in labels {
                    d.push(format!("命中的敏感标签：{}", value_quote(label)));
                }
            } else {
                d.push(format!(
                    "命中的敏感标签：{}",
                    value_quote(&object["target"])
                ));
            }
            if let Some(rows) = object["controls"].as_array() {
                for row in rows {
                    if let Some(summary) = row["page_digest"].as_str() {
                        d.push(format!(
                            "页面内容摘要：{}",
                            &summary[..8.min(summary.len())]
                        ));
                    }
                    if let Some(alert) = row.get("alert") {
                        d.push(format!("弹窗正文：{}", value_quote(&alert["text"])));
                        d.push(format!("目标按钮：{}", value_quote(&alert["target"])));
                        d.push(format!("完整按钮列表：{}", value_quote(&alert["buttons"])));
                    }
                }
            }
            d
        }
        "intent" => {
            let mut d = vec![format!("intent 名称：{}", value_quote(&object["name"]))];
            if let Some(args) = object["args"].as_object() {
                for (k, v) in args {
                    d.push(format!("参数 {}：{}", quote(k), value_quote(v)));
                }
            }
            d
        }
        _ => vec![],
    }
}

#[derive(Clone, Serialize)]
pub struct Pending {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub details: Vec<String>,
    pub digest: String,
    pub nonce: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub object: Value,
    pub status: String,
    #[serde(skip)]
    expires: Instant,
}
impl Pending {
    pub fn message(&self) -> String {
        format!(
            "iphone-use-owner-confirm/v1\n{}\n{}\n{}\n{}",
            self.id, self.nonce, self.digest, self.kind
        )
    }
}
#[derive(Serialize, Deserialize)]
struct Key {
    public_key: String,
}
pub struct OwnerConfirm {
    dir: PathBuf,
    pending: Vec<Pending>,
    allowances: Vec<(String, String, Instant)>,
}
impl Default for OwnerConfirm {
    fn default() -> Self {
        Self::new(crate::instance::current().state_dir.clone())
    }
}
impl OwnerConfirm {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            pending: vec![],
            allowances: vec![],
        }
    }
    fn key(&self) -> Option<Vec<u8>> {
        let k: Key =
            serde_json::from_slice(&std::fs::read(self.dir.join("owner-key.json")).ok()?).ok()?;
        STANDARD.decode(k.public_key).ok()
    }
    fn persist(&self, name: &str, value: &impl Serialize) -> Result<(), &'static str> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&self.dir).map_err(|_| "owner_state_write_failed")?;
        let mut file =
            tempfile::NamedTempFile::new_in(&self.dir).map_err(|_| "owner_state_write_failed")?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "owner_state_write_failed")?;
        serde_json::to_writer(&mut file, value).map_err(|_| "owner_state_write_failed")?;
        file.as_file()
            .sync_all()
            .map_err(|_| "owner_state_write_failed")?;
        file.persist(self.dir.join(name))
            .map_err(|_| "owner_state_write_failed")?;
        Ok(())
    }
    pub fn key_info(&self) -> Value {
        match self.key() {
            Some(k) => json!({"bound":true,"fingerprint":hex(&Sha256::digest(k))}),
            None => json!({"bound":false}),
        }
    }
    /// X9.63 uncompressed P-256 point; replacement signed with the previous key.
    pub fn bind(&mut self, public: &str, signature: Option<&str>) -> Result<(), &'static str> {
        let key = STANDARD.decode(public).map_err(|_| "invalid_public_key")?;
        if key.len() != 65 || key[0] != 4 {
            return Err("invalid_public_key");
        }
        let message = format!("iphone-use-owner-key/v1\n{}", STANDARD.encode(&key));
        let replacing = self.key().is_some();
        if let Some(old) = self.key() {
            verify(&old, message.as_bytes(), signature).map_err(|_| "owner_key_already_bound")?;
        } else if self.dir.join("owner-key.json").exists() {
            return Err("owner_key_unreadable");
        }
        // A proof of possession also validates the curve point on first binding.
        if !replacing {
            verify(&key, message.as_bytes(), signature).map_err(|_| "invalid_key_proof")?;
        }
        self.persist(
            "owner-key.json",
            &Key {
                public_key: STANDARD.encode(key),
            },
        )?;
        self.pending.clear();
        self.allowances.clear();
        Ok(())
    }
    pub fn request(
        &mut self,
        kind: &str,
        object: Value,
        label: &str,
    ) -> Result<String, &'static str> {
        if self.key().is_none() {
            return Err("先在 iPhone Use App 绑定主人设备");
        }
        self.pending.retain(|p| p.expires > Instant::now());
        self.allowances.retain(|a| a.2 > Instant::now());
        let d = digest(&object);
        if let Some(p) = self
            .pending
            .iter()
            .find(|p| p.kind == kind && p.digest == d)
        {
            return Ok(p.id.clone());
        }
        if self.pending.len() + self.allowances.len() >= 16 {
            return Err("owner_confirmation_limit");
        }
        let explanation = match kind {
            "flow" => "允许此版本流程执行敏感步骤",
            "task_press" => "允许任务执行当前待确认操作",
            "control_press" => "允许重发一次相同控制操作",
            "intent" => "允许执行一次此快捷指令",
            _ => return Err("invalid_kind"),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let p = Pending {
            id: random()?,
            nonce: random()?,
            kind: kind.into(),
            summary: format!("{explanation}，标签：{}", quote(label)),
            details: confirmation_details(kind, &object),
            digest: d,
            created_at_ms: now,
            expires_at_ms: now + 120000,
            object,
            status: "pending".into(),
            expires: Instant::now() + TTL,
        };
        let id = p.id.clone();
        self.pending.push(p);
        Ok(id)
    }
    pub fn pending(&mut self) -> Vec<Pending> {
        self.pending.retain(|p| p.expires > Instant::now());
        self.pending.clone()
    }
    pub fn confirm(
        &mut self,
        id: &str,
        approve: bool,
        signature: Option<&str>,
    ) -> Result<Pending, &'static str> {
        let index = self
            .pending
            .iter()
            .position(|p| p.id == id)
            .ok_or("owner_confirmation_not_found")?;
        // Every attempt consumes the sheet, including bad signatures and expiry.
        let p = self.pending.remove(index);
        if !approve {
            return Ok(p);
        }
        if p.expires <= Instant::now() {
            return Err("owner_confirmation_expired");
        }
        let key = self.key().ok_or("先在 iPhone Use App 绑定主人设备")?;
        verify(&key, p.message().as_bytes(), signature)?;
        if p.kind == "flow" {
            let mut approved = self.approved();
            approved.insert(p.digest.clone());
            self.persist("owner-approved-flows.json", &approved)?;
        } else if p.kind != "task_press" {
            self.allowances
                .push((p.kind.clone(), p.digest.clone(), Instant::now() + TTL));
        }
        Ok(p)
    }
    fn approved(&self) -> BTreeSet<String> {
        std::fs::read(self.dir.join("owner-approved-flows.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn flow_approved(&self, object: &Value) -> bool {
        self.key().is_some() && self.approved().contains(&digest(object))
    }
    pub fn consume(&mut self, kind: &str, object: &Value) -> bool {
        self.allowances.retain(|a| a.2 > Instant::now());
        let d = digest(object);
        if let Some(i) = self.allowances.iter().position(|a| a.0 == kind && a.1 == d) {
            self.allowances.swap_remove(i);
            true
        } else {
            false
        }
    }
}
fn verify(key: &[u8], message: &[u8], signature: Option<&str>) -> Result<(), &'static str> {
    let sig = STANDARD
        .decode(signature.ok_or("owner_signature_required")?)
        .map_err(|_| "invalid_owner_signature")?;
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
        .verify(message, &sig)
        .map_err(|_| "invalid_owner_signature")
}

pub fn protected_bundle(bundle: &str) -> bool {
    bundle == "com.leeguoo.iphone-use.remote"
        || bundle.starts_with("com.leeguoo.iphone-use.remote.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING},
    };
    #[test]
    fn quote_escapes_display_boundary_characters_and_marks_truncation() {
        for c in [
            '\n', '\u{0085}', '\u{00ad}', '\u{061c}', '\u{180e}', '\u{2028}', '\u{2029}',
            '\u{feff}',
        ]
        .into_iter()
        .chain(
            ('\u{200b}'..='\u{200f}')
                .chain('\u{202a}'..='\u{202e}')
                .chain('\u{2060}'..='\u{2064}')
                .chain('\u{2066}'..='\u{206f}')
                .chain('\u{fff9}'..='\u{fffb}'),
        ) {
            let quoted = quote(&format!("a{c}b"));
            assert!(!quoted.contains(c));
            assert!(quoted.contains(&format!("u{{{:04x}}}", c as u32)));
        }
        assert!(quote(&"字".repeat(301)).ends_with("…（已截断，共 301 字）"));
        assert!(!quote(&"字".repeat(300)).contains("已截断"));
    }

    #[test]
    fn details_show_plain_input_all_fields_and_intent_arguments() {
        let plain = action_details(
            &json!({"type":"text","text":"完整普通输入","future_field":"extra","kind":"other"}),
            false,
            false,
        );
        assert!(
            plain.contains("完整普通输入")
                && plain.contains("future_field")
                && plain.contains("extra")
        );
        assert!(plain.contains("kind") && plain.contains("other"));
        for action in [
            json!({"type":"text","secure":true,"text":"secret"}),
            json!({"type":"set_value","locator":{"kind":"SecureTextField","value":"secret"}}),
            json!({"type":"text","target":"短信验证码","text":"secret"}),
        ] {
            let details = action_details(&action, false, true);
            assert!(details.contains("secret"), "{details}");
            assert!(!details.contains("已隐藏"));
        }
        let observed = action_details(&json!({"type":"text","text":"secret"}), true, false);
        assert!(!observed.contains("secret"));
        let flow = confirmation_details(
            "flow",
            &json!({"flow":{"steps":[{"kind":"text","text":"plain"}]}}),
        )
        .join(";");
        assert!(flow.contains("plain"));
        assert!(
            flow.contains("批准后此版本流程以后每次回放都跳过付款/密码拦截，流程内容一改即失效")
        );
        let intent = confirmation_details(
            "intent",
            &json!({"name":"x","args":{"a":1,"b":true,"c":"三"}}),
        )
        .join(";");
        for value in ["a", "1", "b", "true", "c", "三"] {
            assert!(intent.contains(value));
        }
    }

    #[test]
    fn control_details_ignore_request_secure_and_target_claims() {
        for action in [
            json!({"type":"text","secure":true,"text":"visible-payment"}),
            json!({"type":"text","target":"短信验证码","text":"visible-payment"}),
            json!({"type":"set_value","locator":{"kind":"SecureTextField","value":"visible-payment"}}),
        ] {
            let details = confirmation_details(
                "control_press",
                &json!({
                    "action": action, "controls":[{"kind":"TextField","secure":false}]
                }),
            )
            .join(";");
            assert!(details.contains("visible-payment"), "{details}");
            assert!(!details.contains("已隐藏"));
        }
        let hidden = confirmation_details(
            "control_press",
            &json!({
                "action":{"type":"text","text":"actual-secret"},
                "controls":[{"kind":"SecureTextField","secure":true}]
            }),
        )
        .join(";");
        assert!(!hidden.contains("actual-secret"));
        assert!(hidden.contains("已隐藏"));
    }

    #[test]
    fn details_quote_untrusted_values_and_show_flow_input() {
        let d = confirmation_details(
            "flow",
            &json!({"id":"shop/pay", "flow":{"name":"evil\n允许全部"}, "actions":{"steps":[{"action":{"type":"tap","label":"立即购买","x":0.7,"y":0.9}},{"action":{"type":"text","secure":true,"text":"secret-password"}}]}}),
        );
        let text = d.join("\n");
        assert!(text.contains("shop/pay"));
        assert!(text.contains("步骤 1"));
        assert!(text.contains("批准后：此步骤将不经付款/密码拦截直接执行"));
        assert!(text.contains("secret-password"));
        assert!(!text.contains("已隐藏"));
        assert!(!text.contains("evil\n"));
        assert!(text.contains("流程内容一改即失效"));
        let task = confirmation_details(
            "task_press",
            &json!({"goal":"buy", "operation":{"action":"CLICK","target":"立即购买"}}),
        )
        .join(";");
        assert!(task.contains("buy") && task.contains("CLICK") && task.contains("立即购买"));
        let intent = confirmation_details(
            "intent",
            &json!({"name":"pay", "args":{"amount":123,"note":"x\n注入"}}),
        )
        .join(";");
        assert!(intent.contains("123") && !intent.contains("x\n"));
        assert_eq!(
            quote(&"a".repeat(300)),
            serde_json::to_string(&"a".repeat(300)).unwrap()
        );
    }

    #[test]
    fn expired_sheet_is_consumed_and_cannot_create_authority() {
        let dir = tempfile::tempdir().unwrap();
        let mut owner = OwnerConfirm::new(dir.path().into());
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = STANDARD.encode(key.public_key().as_ref());
        let proof = key
            .sign(
                &rng,
                format!("iphone-use-owner-key/v1\n{public}").as_bytes(),
            )
            .unwrap();
        owner.bind(&public, Some(&STANDARD.encode(proof))).unwrap();
        let object = json!({"name":"pay"});
        let id = owner.request("intent", object.clone(), "pay").unwrap();
        owner.pending[0].expires = Instant::now() - Duration::from_secs(1);
        let signature = key
            .sign(&rng, owner.pending[0].message().as_bytes())
            .unwrap();
        assert_eq!(
            owner
                .confirm(&id, true, Some(&STANDARD.encode(signature)))
                .err(),
            Some("owner_confirmation_expired")
        );
        assert!(owner.confirm(&id, true, None).is_err());
        assert!(!owner.consume("intent", &object));
    }
}
