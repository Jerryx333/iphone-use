//! Phone-only, opaque-body relay. Never log this module's inputs or HTTP errors.
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header, Client, Method, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{net::IpAddr, time::Duration};

pub const BODY_LIMIT: usize = 256 * 1024;
pub const REQUEST_LIMIT: usize = 16 * 1024;

// Deliberately no Debug: both URL and credential are configuration secrets.
#[derive(Clone)]
pub struct OwnerRelay {
    config: Result<(Client, Url, header::HeaderValue), &'static str>,
}
impl Default for OwnerRelay {
    fn default() -> Self {
        Self {
            config: Err("owner_unconfigured"),
        }
    }
}
impl OwnerRelay {
    /// Uses the same loopback-only validation and transport policy as environment configuration.
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            config: Self::configured(url, token),
        }
    }
    pub fn from_env() -> Self {
        let url = std::env::var("PHONE_GATEWAY_OWNER_URL").ok();
        let token = std::env::var("PHONE_GATEWAY_OWNER_TOKEN").ok();
        let file = std::env::var_os("PHONE_GATEWAY_OWNER_TOKEN_FILE");
        if url.is_none() && token.is_none() && file.is_none() {
            return Self::default();
        }
        let config = (|| {
            let url = url.ok_or("owner_config_invalid")?;
            let token = match (token, file) {
                (Some(token), None) => token,
                (None, Some(file)) => {
                    use std::io::Read;
                    let mut bytes = Vec::new();
                    std::fs::File::open(file)
                        .map_err(|_| "owner_config_invalid")?
                        .take(8193)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "owner_config_invalid")?;
                    if bytes.len() > 8192 {
                        return Err("owner_config_invalid");
                    }
                    String::from_utf8(bytes)
                        .map_err(|_| "owner_config_invalid")?
                        .trim_end_matches(['\r', '\n'])
                        .to_owned()
                }
                _ => return Err("owner_config_invalid"),
            };
            Self::configured(&url, &token)
        })();
        Self { config }
    }
    fn configured(
        url: &str,
        token: &str,
    ) -> Result<(Client, Url, header::HeaderValue), &'static str> {
        let url = Url::parse(url).map_err(|_| "owner_config_invalid")?;
        // Numeric loopback only: no DNS, proxies, redirects, URL credentials or path injection.
        let host = url.host_str().ok_or("owner_config_invalid")?;
        let ip: IpAddr = host
            .trim_matches(['[', ']'])
            .parse()
            .map_err(|_| "owner_config_invalid")?;
        if url.scheme() != "http"
            || !ip.is_loopback()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || token.is_empty()
            || token.len() > 8192
            || !token.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err("owner_config_invalid");
        }
        let mut auth = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "owner_config_invalid")?;
        auth.set_sensitive(true);
        let client = Client::builder()
            .no_proxy()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| "owner_config_invalid")?;
        Ok((client, url, auth))
    }
    async fn http(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), &'static str> {
        self.http_with_timeout(method, path, body, Duration::from_secs(3))
            .await
    }
    async fn http_with_timeout(
        &self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        budget: Duration,
    ) -> Result<(u16, Vec<u8>), &'static str> {
        let (client, base, auth) = self.config.as_ref().map_err(|code| *code)?;
        let mut url = base.clone();
        url.set_path(path);
        let mut response = client
            .request(method, url)
            .timeout(budget)
            .header(header::AUTHORIZATION, auth.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| "owner_daemon_unavailable")?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|n| n > BODY_LIMIT as u64)
        {
            return Err("owner_response_too_large");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "owner_daemon_unavailable")?
        {
            if body.len() + chunk.len() > BODY_LIMIT {
                return Err("owner_response_too_large");
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, body))
    }
    pub async fn request(&self, frame: &Value) -> Value {
        let id = frame["id"].as_str().filter(|id| valid_id(id)).unwrap_or("");
        let result = async {
            if id.is_empty() {
                return Err("owner_invalid_request");
            }
            let (method, path, body) = decode_request(frame)?;
            self.http(method, &path, body).await
        }
        .await;
        match result {
            Ok((status, body)) => {
                json!({"t":"owner_res","id":id,"status":status,"body_b64":STANDARD.encode(body)})
            }
            Err(code) => failure(id, code),
        }
    }
    /// Fail closed, retaining only the daemon's closed admission schema.
    pub async fn guard(&self, action: &str, args: &Value) -> Result<(), Value> {
        let result = self
            .http_with_timeout(
                Method::POST,
                "/guard/gateway-command",
                serde_json::to_vec(&json!({"action":action,"args":args})).unwrap(),
                Duration::from_secs(10),
            )
            .await;
        let (status, bytes) = result.map_err(|code| json!({"code":if code == "owner_unconfigured" {"guard_unconfigured"} else {"guard_unavailable"}}))?;
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| json!({"code":"guard_unavailable"}))?;
        if status == 200 && body == json!({"allow":true}) {
            return Ok(());
        }
        if status == 403 && body["error"] == "owner_confirmation_required" {
            if let Some(id) = body["owner_confirm_id"].as_str().filter(|id| valid_id(id)) {
                return Err(
                    json!({"code":"owner_confirm_required","owner_confirm_id":id,
                    "hint":"请主人在手机 Self Runner 刷脸允许后，两分钟内原样重发同一命令"}),
                );
            }
        }
        if status == 429 && body["error"] == "guard_busy" {
            return Err(json!({"code":"guard_busy"}));
        }
        Err(
            json!({"code":if status == 403 && body["error"] == "owner_app_protected" {"owner_app_protected"} else {"guard_unavailable"}}),
        )
    }
    pub async fn pending(&self) -> Result<Vec<u8>, &'static str> {
        let (status, body) = self.http(Method::GET, "/owner/pending", Vec::new()).await?;
        if status != 200 {
            return Err("owner_pending_unavailable");
        }
        Ok(body)
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
}
pub fn failure(id: &str, code: &str) -> Value {
    let id = if valid_id(id) { id } else { "" };
    json!({"t":"owner_res","id":id,"error":{"code":code}})
}
fn decode_request(frame: &Value) -> Result<(Method, String, Vec<u8>), &'static str> {
    let method = frame["method"].as_str().ok_or("owner_invalid_request")?;
    let path = frame["path"].as_str().ok_or("owner_invalid_request")?;
    let allowed = match (method, path) {
        ("GET", "/owner/key" | "/owner/pending") | ("POST", "/owner/key") => true,
        ("POST", path) => path.strip_prefix("/owner/confirm/").is_some_and(valid_id),
        _ => false,
    };
    if !allowed {
        return Err("owner_invalid_request");
    }
    let encoded = frame["body_b64"].as_str().ok_or("owner_invalid_request")?;
    if encoded.len() > REQUEST_LIMIT.div_ceil(3) * 4 {
        return Err("owner_request_too_large");
    }
    let body = STANDARD
        .decode(encoded)
        .map_err(|_| "owner_invalid_request")?;
    if body.len() > REQUEST_LIMIT {
        return Err("owner_request_too_large");
    }
    if method == "GET" && !body.is_empty() {
        return Err("owner_invalid_request");
    }
    Ok((
        if method == "GET" {
            Method::GET
        } else {
            Method::POST
        },
        path.into(),
        body,
    ))
}

/// Only a digest is retained. Neither signatures nor pending bodies are cached.
#[derive(Default)]
pub struct PendingChanges(Option<[u8; 32]>);
impl PendingChanges {
    pub fn update(&mut self, body: &[u8]) -> Result<Option<Value>, &'static str> {
        let value: Value = serde_json::from_slice(body).map_err(|_| "owner_pending_invalid")?;
        let pending = value["pending"].as_array().ok_or("owner_pending_invalid")?;
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(pending).map_err(|_| "owner_pending_invalid")?)
                .into();
        if self.0 == Some(digest) {
            return Ok(None);
        }
        self.0 = Some(digest);
        Ok(Some(
            json!({"t":"owner_pending","body_b64":STANDARD.encode(body)}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_errors_never_include_credentials_or_url() {
        let secret = "FAKE-OWNER-PASSWORD";
        for url in [
            format!("http://{secret}@127.0.0.1"),
            format!("http://192.0.2.1/{secret}"),
            secret.into(),
        ] {
            let error = OwnerRelay::configured(&url, secret).err().unwrap();
            assert_eq!(error, "owner_config_invalid");
            assert!(!error.contains(secret));
        }
        assert!(OwnerRelay::configured("http://[::1]:1234", secret).is_ok());
        assert!(OwnerRelay::configured("http://127.0.0.1:1234", "bad\r\nsecret").is_err());
    }
    #[test]
    fn pending_only_pushes_changes_and_preserves_original_bytes() {
        let mut changes = PendingChanges::default();
        let first =
            br#"{ "pending": [{"id":"a","kind":"intent","summary":"test","expires_at_ms":1}] }"#;
        let push = changes.update(first).unwrap().unwrap();
        assert_eq!(
            STANDARD.decode(push["body_b64"].as_str().unwrap()).unwrap(),
            first
        );
        assert!(changes.update(first).unwrap().is_none());
        let same = serde_json::to_vec(&serde_json::from_slice::<Value>(first).unwrap()).unwrap();
        assert!(changes.update(&same).unwrap().is_none());
        assert!(changes.update(br#"{"pending":[]}"#).unwrap().is_some());
        assert!(changes.update(br#"{"pending":[]}"#).unwrap().is_none());
        // A new connection starts with current state, never retained frames.
        assert!(PendingChanges::default()
            .update(br#"{"pending":[]}"#)
            .unwrap()
            .is_some());
    }
    #[test]
    fn request_allowlist_and_size_bounds_keep_bodies_opaque() {
        for (method, path) in [
            ("GET", "/owner/key"),
            ("GET", "/owner/pending"),
            ("POST", "/owner/key"),
            ("POST", "/owner/confirm/a-b_c:1"),
        ] {
            let body = if method == "GET" {
                Vec::new()
            } else {
                vec![0, 255, b'\n', b' ']
            };
            let frame = json!({"method":method,"path":path,"body_b64":STANDARD.encode(&body)});
            let (actual_method, actual_path, actual_body) = decode_request(&frame).unwrap();
            assert_eq!(actual_method.as_str(), method);
            assert_eq!(actual_path, path);
            assert_eq!(actual_body, body);
        }
        for size in [REQUEST_LIMIT, REQUEST_LIMIT + 1] {
            let frame = json!({"method":"POST","path":"/owner/key","body_b64":STANDARD.encode(vec![b' ';size])});
            assert_eq!(decode_request(&frame).is_ok(), size == REQUEST_LIMIT);
        }
        for (method, path, body) in [
            ("DELETE", "/owner/key", ""),
            ("POST", "/owner/pending", ""),
            ("GET", "/owner/key", "eA=="),
            ("POST", "/owner/confirm/", ""),
            ("POST", "/owner/key", "invalid!"),
        ] {
            assert!(decode_request(&json!({"method":method,"path":path,"body_b64":body})).is_err());
        }
    }
    #[tokio::test]
    async fn missing_config_and_bad_paths_fail_closed() {
        let relay = OwnerRelay::default();
        let mut frame =
            json!({"t":"owner_req","id":"r","method":"GET","path":"/owner/key","body_b64":""});
        assert_eq!(
            relay.request(&frame).await["error"]["code"],
            "owner_unconfigured"
        );
        for path in [
            "/control",
            "/owner/key?x=y",
            "/owner/confirm/../key",
            "/owner/confirm/x%2fy",
        ] {
            frame["path"] = json!(path);
            frame["method"] = json!("POST");
            assert_eq!(
                relay.request(&frame).await["error"]["code"],
                "owner_invalid_request"
            );
        }
    }
    #[tokio::test]
    async fn forwards_request_and_response_byte_for_byte_without_error_leaks() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let secret = "FAKE-OWNER-PASSWORD";
        let relay = OwnerRelay {
            config: OwnerRelay::configured(&url, secret),
        };
        let request = b"{ \"signature\": \"ABC+/=\", \"approve\": true }\n";
        let response = b"{ \"error\": \"invalid_owner_signature\" }\n";
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let split = loop {
                bytes.push(stream.read_u8().await.unwrap());
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
            };
            let headers = String::from_utf8(bytes).unwrap();
            assert!(headers.starts_with("POST /owner/confirm/test HTTP/1.1"));
            assert!(headers.contains(&format!("authorization: Bearer {secret}")));
            assert!(split > 0);
            let mut body = vec![0; request.len()];
            stream.read_exact(&mut body).await.unwrap();
            assert_eq!(body, request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(response).await.unwrap();
        });
        let frame = json!({"t":"owner_req","id":"test","method":"POST","path":"/owner/confirm/test","body_b64":STANDARD.encode(request)});
        let result = relay.request(&frame).await;
        server.await.unwrap();
        assert_eq!(result["status"], 403);
        assert_eq!(
            STANDARD
                .decode(result["body_b64"].as_str().unwrap())
                .unwrap(),
            response
        );
        assert!(!result.to_string().contains(secret));
        let failed = relay.request(&frame).await;
        assert_eq!(failed["error"]["code"], "owner_daemon_unavailable");
        assert!(!failed.to_string().contains(secret));
        assert!(!failed.to_string().contains(&url));
    }
}
