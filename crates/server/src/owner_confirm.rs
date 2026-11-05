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
    serde_json::to_string(
        &label
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect::<String>(),
    )
    .unwrap()
}
#[derive(Clone, Serialize)]
pub struct Pending {
    pub id: String,
    pub kind: String,
    pub summary: String,
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
