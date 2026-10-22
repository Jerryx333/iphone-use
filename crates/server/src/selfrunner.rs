//! Optional loopback phone-gateway diagnostics. Credentials never leave this client.
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub struct Gateway {
    base: String,
    token_file: PathBuf,
    http: reqwest::Client,
    cache: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

impl Gateway {
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("IPHONE_USE_SELFRUNNER_GATEWAY").ok()?;
        let token_file = std::env::var_os("IPHONE_USE_SELFRUNNER_GATEWAY_TOKEN_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("PHONE_GATEWAY_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                            .join(".config/phone-gateway")
                    })
                    .join("client-token")
            });
        match Self::new(&base, token_file) {
            Some(gateway) => Some(gateway),
            None => {
                tracing::warn!("ignoring invalid IPHONE_USE_SELFRUNNER_GATEWAY: a loopback HTTP address is required");
                None
            }
        }
    }

    pub(crate) fn new(base: &str, token_file: PathBuf) -> Option<Self> {
        let url = reqwest::Url::parse(base).ok()?;
        let host = url.host_str()?.trim_matches(['[', ']']);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !loopback
            || url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return None;
        }
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok()?;
        Some(Self {
            base: base.trim_end_matches('/').into(),
            token_file,
            http,
            cache: tokio::sync::Mutex::new(None),
        })
    }

    async fn request(&self, path: &str, body: Value) -> Result<Value, ()> {
        // Do not propagate reqwest errors: they can contain request headers.
        let token = std::fs::read_to_string(&self.token_file).map_err(|_| ())?;
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token.trim())
            .json(&body)
            .send()
            .await
            .map_err(|_| ())?
            .error_for_status()
            .map_err(|_| ())?
            .json()
            .await
            .map_err(|_| ())
    }

    pub async fn status(&self) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let Ok(mut cache) = tokio::time::timeout_at(deadline, self.cache.lock()).await else {
            return json!({"gateway":"down", "transport":null, "needs_passcode":null,
                "needs_relaunch":null, "runner_core_running":null, "vpn":null, "tunnels_idle":null, "tunnels_active":null});
        };
        if let Some((at, value)) = &*cache {
            if at.elapsed() < Duration::from_secs(5) {
                return value.clone();
            }
        }
        let mut value = json!({"gateway":"down", "transport":null, "needs_passcode":null,
            "needs_relaunch":null, "runner_core_running":null, "vpn":null, "tunnels_idle":null, "tunnels_active":null});
        let _ = tokio::time::timeout_at(deadline, async {
            let Ok(status) = self.request("/v1/status", json!({})).await else {
                return;
            };
            value["gateway"] = json!("up");
            value["transport"] = status["transport"].clone();
            value["tunnels_idle"] = status["tunnels"]["idle"].clone();
            value["tunnels_active"] = status["tunnels"]["active"].clone();
            if status["transport"] != "online" {
                return;
            }
            let Ok(command) = self
                .request("/v1/command", json!({"action":"status","deadline_ms":2000}))
                .await
            else {
                return;
            };
            if command["ok"] != true {
                return;
            }
            let result = &command["result"];
            for field in ["needs_passcode", "needs_relaunch", "vpn"] {
                value[field] = result[field].clone();
            }
            value["runner_core_running"] = result["runner_core"]["running"].clone();
            value["tunnels_idle"] = result["runner_tunnel"]["idle"].clone();
            value["tunnels_active"] = result["runner_tunnel"]["active"].clone();
        })
        .await;
        *cache = Some((Instant::now(), value.clone()));
        value
    }

    pub async fn unlock(&self) -> Result<(), String> {
        let response = tokio::time::timeout(
            Duration::from_secs(16),
            self.request(
                "/v1/command",
                json!({"action":"unlock", "deadline_ms":15000}),
            ),
        )
        .await;
        *self.cache.lock().await = None;
        match response {
            Ok(Ok(value)) if value["ok"] == true => Ok(()),
            // The gateway answers {"ok":false,"error":{"code":…}}.
            Ok(Ok(value)) => Err(value["error"]["code"]
                .as_str()
                .unwrap_or("gateway_unlock_failed")
                .to_owned()),
            _ => Err("gateway_unavailable".into()),
        }
    }
}

pub fn guidance(value: &Value, locked: bool) -> Option<(&'static str, &'static str)> {
    if value["gateway"] == "down" {
        Some((
            "本机 phone-gateway 没在运行，请启动它",
            "The local phone-gateway is not running; start it",
        ))
    } else if value["transport"] == "offline" {
        Some((
            "请在手机上打开 Self Runner App，并确认 LocalDevVPN 已开",
            "Self Runner is disconnected; open Self Runner on the phone and enable LocalDevVPN",
        ))
    } else if value["needs_relaunch"] == true {
        // Unlocking the screen is not enough: the automation session itself
        // has to be started again by someone holding the phone.
        Some((
            "在手机上把 Self Runner 从多任务界面上滑关掉，重新打开后点 Start，按提示输一次密码；远程做不到",
            "On the phone, swipe Self Runner away in the app switcher, reopen it, tap Start and enter the passcode if asked; this cannot be done remotely",
        ))
    } else if value["needs_passcode"] == true {
        Some((
            "在手机上打开 Self Runner 点 Start，按提示输一次密码；远程做不到",
            "Open Self Runner on the phone, tap Start and enter the passcode if asked; this cannot be done remotely",
        ))
    } else if value["runner_core_running"] == false
        // Every tunnel busy is a working runner, not a missing one.
        || (value["tunnels_idle"] == 0 && value["tunnels_active"] == 0)
    {
        Some((
            "请在 Self Runner 里打开「Runner API 8100/9100」，或等待通道建立",
            "Enable Runner API 8100/9100 in Self Runner, or wait for its tunnels to connect",
        ))
    } else if locked {
        Some(("手机已锁屏，调用一次 phone_reconnect；Self Runner 会用 Mac 钥匙串里的密码远程解锁", "The phone is locked; call phone_reconnect once to unlock remotely using the passcode in the Mac Keychain"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stopped_session_names_the_start_button() {
        let up = |field: &str| json!({"gateway": "up", "transport": "online", field: true});
        let (zh, en) = guidance(&up("needs_passcode"), true).unwrap();
        assert!(
            zh.contains("点 Start") && en.contains("tap Start"),
            "{zh} / {en}"
        );
        assert!(
            !zh.contains("上滑"),
            "a plain restart needs no relaunch: {zh}"
        );
        let (zh, en) = guidance(&up("needs_relaunch"), false).unwrap();
        assert!(zh.contains("上滑关掉") && zh.contains("点 Start"), "{zh}");
        assert!(en.contains("swipe Self Runner away"), "{en}");
    }

    #[test]
    fn accepts_only_loopback() {
        for base in [
            "http://127.0.0.1:7382",
            "http://[::1]:7382",
            "http://localhost:7382",
        ] {
            assert!(Gateway::new(base, PathBuf::new()).is_some());
        }
        for base in [
            "http://example.com",
            "http://192.168.1.1",
            "http://user:secret@localhost",
            "http://localhost/x",
            "https://localhost",
        ] {
            assert!(Gateway::new(base, PathBuf::new()).is_none());
        }
    }
}
