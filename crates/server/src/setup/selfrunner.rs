//! Pairing-derived credentials and readiness for an already installed Self Runner.
use std::path::{Path, PathBuf};

pub const PAIRING: &str = "selfrunner_pairing";
pub const NOT_STARTED: &str = "selfrunner_not_started";
pub const TOKEN_MISMATCH: &str = "selfrunner_token_mismatch";
pub const START_HINT: &str = "在手机上打开 Self Runner 点 Start，按提示输一次密码";
pub const PAIRING_HINT: &str = "先完成 phone-gateway 配对；参见 docs/selfrunner-remote-setup.md";

pub fn psk_path(ctx: &super::ctx::Ctx) -> PathBuf {
    std::env::var_os("PHONE_GATEWAY_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let saved = super::sys::plist_env(&ctx.runner_agent_plist, "PHONE_GATEWAY_HOME");
            (!saved.is_empty()).then(|| PathBuf::from(saved))
        })
        .unwrap_or_else(|| ctx.home.join(".config/phone-gateway"))
        .join("psk")
}

pub fn token_from_file(path: &Path) -> Result<String, &'static str> {
    let encoded = std::fs::read_to_string(path).map_err(|_| PAIRING)?;
    derive_token(&encoded)
}

pub fn derive_token(encoded: &str) -> Result<String, &'static str> {
    use base64::Engine as _;
    let psk = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| PAIRING)?;
    if psk.len() != 32 {
        return Err(PAIRING);
    }
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &psk);
    let digest = ring::hmac::sign(&key, b"selfrunner-runner-token-v1");
    Ok(digest.as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// None means continue polling; exhaustion is always NOT_STARTED.
pub fn readiness(response: Option<(u16, &[u8])>, timed_out: bool) -> Result<bool, &'static str> {
    if let Some((code, body)) = response {
        if code == 401 {
            return Err(TOKEN_MISMATCH);
        }
        if code == 200
            && serde_json::from_slice::<serde_json::Value>(body)
                .is_ok_and(|value| value["ready"] == true || value["value"]["ready"] == true)
        {
            return Ok(true);
        }
    }
    if timed_out {
        Err(NOT_STARTED)
    } else {
        Ok(false)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Hold,
    OpenAndRelay,
}

/// This host's only recovery action: open the installed app and repair relays.
pub fn recovery(
    response: Option<(u16, &[u8])>,
    relays_alive: bool,
) -> Result<Recovery, &'static str> {
    readiness(response, false).map(|ready| {
        if ready && relays_alive {
            Recovery::Hold
        } else {
            Recovery::OpenAndRelay
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::ctx::{selfrunner_bundle, HostPlan, RunnerHost};
    use super::*;
    #[test]
    fn host_selection_and_app_lifecycle_policy() {
        for raw in [None, Some(""), Some("xctest")] {
            let host = RunnerHost::parse(raw).unwrap();
            assert_eq!(host, RunnerHost::Xctest);
            assert!(host.manages_test_app());
        }
        let host = RunnerHost::parse(Some("selfrunner")).unwrap();
        assert!(!host.manages_test_app());
        assert_eq!(host.plan(), HostPlan::SelfRunner);
        assert_eq!(RunnerHost::Xctest.plan(), HostPlan::Xctest);
        assert!(RunnerHost::parse(Some("unknown")).is_err());
        assert!(selfrunner_bundle("dev.example.host").is_ok());
        for bad in ["host", "a..b", "a/b.c", "a.-b"] {
            assert!(selfrunner_bundle(bad).is_err());
        }
    }
    #[test]
    fn python_known_answer_and_pairing_errors() {
        use base64::Engine as _;
        let encoded =
            base64::engine::general_purpose::STANDARD.encode((0..32).collect::<Vec<u8>>());
        // Python: hmac.new(bytes(range(32)), b"selfrunner-runner-token-v1", hashlib.sha256).hexdigest()
        assert_eq!(
            derive_token(&encoded).unwrap(),
            "0ef4ed3fcbb2483a664871195051c59c8bab5fc4f23fd8eca10d5867a5a58c2c"
        );
        for encoded in ["", "invalid!", "YWJj"] {
            assert_eq!(derive_token(encoded), Err(PAIRING));
        }
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(token_from_file(&dir.path().join("missing")), Err(PAIRING));
    }
    #[test]
    fn selfrunner_recovery_cannot_select_rebuild() {
        for response in [
            None,
            Some((500, b"".as_slice())),
            Some((200, br#"{"ready":false}"#.as_slice())),
        ] {
            assert_eq!(recovery(response, true), Ok(Recovery::OpenAndRelay));
        }
        let ready = Some((200, br#"{"value":{"ready":true}}"#.as_slice()));
        assert_eq!(recovery(ready, true), Ok(Recovery::Hold));
        assert_eq!(recovery(ready, false), Ok(Recovery::OpenAndRelay));
        assert_eq!(recovery(Some((401, b"")), true), Err(TOKEN_MISMATCH));
    }
    #[test]
    fn readiness_verdicts() {
        assert_eq!(readiness(Some((401, b"")), false), Err(TOKEN_MISMATCH));
        assert_eq!(readiness(None, true), Err(NOT_STARTED));
        assert_eq!(
            readiness(Some((200, br#"{"value":{"ready":true}}"#)), false),
            Ok(true)
        );
        assert_eq!(
            readiness(Some((200, br#"{"ready":false}"#)), false),
            Ok(false)
        );
        assert_eq!(
            readiness(Some((500, br#"{"ready":true}"#)), false),
            Ok(false)
        );
    }
}
