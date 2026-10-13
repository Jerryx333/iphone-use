//! DDI TSS dictionary ported from idevice (MIT), revision 41c388b:
//! idevice/src/services/mobile_image_mounter.rs and idevice/src/tss.rs.
//! Transport is local: verified HTTPS to a fixed Apple host, with no redirects.
use plist::{Dictionary, Value};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_rustls::{TlsConnector, rustls};

pub(crate) fn request(
    identifiers: &Dictionary,
    nonce: Vec<u8>,
    identity: &Dictionary,
) -> Result<Dictionary, &'static str> {
    let mut dict = Dictionary::new();
    dict.insert("@HostPlatformInfo".into(), "mac".into());
    dict.insert("@VersionInfo".into(), "libauthinstall-1033.0.2".into());
    dict.insert(
        "@UUID".into(),
        uuid::Uuid::new_v4().to_string().to_uppercase().into(),
    );
    for (key, value) in identifiers {
        if key.starts_with("Ap,") {
            dict.insert(key.clone(), value.clone());
        }
    }
    dict.insert("@ApImg4Ticket".into(), true.into());
    dict.insert("@BBTicket".into(), true.into());
    for (source, target) in [
        ("BoardId", "ApBoardID"),
        ("ChipID", "ApChipID"),
        ("UniqueChipID", "ApECID"),
    ] {
        let n = identifiers
            .get(source)
            .and_then(Value::as_unsigned_integer)
            .ok_or("MissingIdentifier")?;
        dict.insert(target.into(), n.into());
    }
    dict.insert("ApProductionMode".into(), true.into());
    dict.insert("ApSecurityDomain".into(), 1u64.into());
    dict.insert("ApSecurityMode".into(), true.into());
    dict.insert("UID_MODE".into(), false.into());
    dict.insert("ApNonce".into(), Value::Data(nonce));
    dict.insert("SepNonce".into(), Value::Data(vec![0; 20]));
    let parameters = plist_macro::plist!(dict {
        "ApProductionMode": true, "ApSecurityMode": true, "ApSupportsImg4": true
    });
    let manifest = identity
        .get("Manifest")
        .and_then(Value::as_dictionary)
        .ok_or("BadBuildManifest")?;
    let rules = manifest
        .get("LoadableTrustCache")
        .and_then(Value::as_dictionary)
        .and_then(|v| v.get("Info"))
        .and_then(Value::as_dictionary)
        .and_then(|v| v.get("RestoreRequestRules"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    for (key, value) in manifest {
        let Some(item) = value.as_dictionary() else {
            continue;
        };
        if item.get("Info").and_then(Value::as_dictionary).is_none()
            || item.get("Trusted").and_then(Value::as_boolean) != Some(true)
        {
            continue;
        }
        let mut entry = item.clone();
        entry.remove("Info");
        // Pure rule helper; the enclosing operation has NoSubscriber installed.
        idevice::tss::apply_restore_request_rules(&mut entry, &parameters, rules);
        if !item.contains_key("Digest") {
            entry.insert("Digest".into(), Value::Data(Vec::new()));
        }
        dict.insert(key.clone(), Value::Dictionary(entry));
    }
    Ok(dict)
}

#[derive(Debug)]
pub(crate) enum Failure {
    Timeout { phase: &'static str },
    Network,
    Tls(String),
    Http(u16),
    HttpFraming,
    TooLarge,
    Plist,
    Apple(Option<i64>),
}
impl Failure {
    pub(crate) fn report(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::Timeout { phase } => json!({"error":"Timeout", "phase":phase}),
            Self::Tls(category) => json!({"error":"Tls", "category":category}),
            Self::Http(n) => json!({"error":"HttpStatus", "http_status":n}),
            Self::Apple(n) => json!({"error":"AppleStatus", "apple_status":n}),
            _ => json!({"error":format!("{self:?}")}),
        }
    }
}
const APPLE_ROOT_CA: &[u8] = include_bytes!("../certs/apple-root-ca.der");
fn tls_failure(error: rustls::Error) -> Failure {
    Failure::Tls(crate::error_variant(&format!("{error:?}")))
}
fn tls_io_failure(error: std::io::Error) -> Failure {
    let category = match error
        .get_ref()
        .and_then(|e| e.downcast_ref::<rustls::Error>())
    {
        Some(error) => crate::error_variant(&format!("{error:?}")),
        None => crate::error_variant(&format!("{:?}", error.kind())),
    };
    Failure::Tls(category)
}
const LIMIT: usize = 4 * 1024 * 1024;
// A bounded line reader avoids allocating an unbounded HTTP header/chunk line.
async fn line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<String, Failure> {
    let mut bytes = Vec::new();
    loop {
        let byte = reader.read_u8().await.map_err(|_| Failure::HttpFraming)?;
        bytes.push(byte);
        if bytes.len() > 8192 {
            return Err(Failure::HttpFraming);
        }
        if bytes.ends_with(b"\r\n") {
            break;
        }
    }
    String::from_utf8(bytes).map_err(|_| Failure::HttpFraming)
}
#[cfg(test)]
async fn response<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, Failure> {
    let status = line(reader).await?;
    response_body(reader, &status).await
}
async fn response_body<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    status: &str,
) -> Result<Vec<u8>, Failure> {
    let mut parts = status.split_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err(Failure::HttpFraming);
    }
    let code: u16 = parts
        .next()
        .ok_or(Failure::HttpFraming)?
        .parse()
        .map_err(|_| Failure::HttpFraming)?;
    if code != 200 {
        return Err(Failure::Http(code));
    }
    let mut length = None;
    let mut chunked = false;
    let mut total = 0;
    loop {
        let header = line(reader).await?;
        total += header.len();
        if total > 65536 {
            return Err(Failure::HttpFraming);
        }
        if header == "\r\n" {
            break;
        }
        let (key, value) = header.split_once(':').ok_or(Failure::HttpFraming)?;
        let value = value.trim();
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(Failure::HttpFraming);
            }
            length = Some(value.parse::<usize>().map_err(|_| Failure::HttpFraming)?);
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(Failure::HttpFraming);
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err(Failure::HttpFraming);
    }
    let mut body = Vec::new();
    if chunked {
        loop {
            let size = line(reader).await?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| Failure::HttpFraming)?;
            if n == 0 {
                break;
            }
            if n > LIMIT - body.len() {
                return Err(Failure::TooLarge);
            }
            let start = body.len();
            body.resize(start + n, 0);
            reader
                .read_exact(&mut body[start..])
                .await
                .map_err(|_| Failure::HttpFraming)?;
            if line(reader).await? != "\r\n" {
                return Err(Failure::HttpFraming);
            }
        }
    } else if let Some(n) = length {
        if n > LIMIT {
            return Err(Failure::TooLarge);
        }
        body.resize(n, 0);
        reader
            .read_exact(&mut body)
            .await
            .map_err(|_| Failure::HttpFraming)?;
    } else {
        reader
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut body)
            .await
            .map_err(|_| Failure::Network)?;
        if body.len() > LIMIT {
            return Err(Failure::TooLarge);
        }
    }
    Ok(body)
}
fn parse(body: &[u8]) -> Result<Dictionary, Failure> {
    let text = std::str::from_utf8(body).map_err(|_| Failure::Plist)?;
    let (prefix, xml) = text.split_once("&REQUEST_STRING=").unwrap_or((text, ""));
    let status = prefix
        .split('&')
        .find_map(|v| v.strip_prefix("STATUS="))
        .and_then(|v| v.parse::<i64>().ok());
    if status != Some(0) {
        return Err(Failure::Apple(status));
    }
    if !prefix.split('&').any(|v| v == "MESSAGE=SUCCESS") {
        return Err(Failure::Apple(status));
    }
    let value = Value::from_reader_xml(xml.as_bytes()).map_err(|_| Failure::Plist)?;
    value.into_dictionary().ok_or(Failure::Plist)
}
// Per-phase durations, including elapsed time on errors/timeouts. Unentered phases
// are omitted. No request or response contents are stored here.
#[derive(Debug, Default)]
pub(crate) struct Timings {
    pub(crate) timings_ms: serde_json::Map<String, serde_json::Value>,
    pub(crate) request_bytes: usize,
}
impl Timings {
    pub(crate) fn attach(&self, report: &mut serde_json::Value) {
        report["timings_ms"] = serde_json::Value::Object(self.timings_ms.clone());
        report["request_bytes"] = self.request_bytes.into();
    }

    async fn phase<T>(
        &mut self,
        phase: &'static str,
        seconds: u64,
        operation: impl Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(seconds), operation).await;
        self.timings_ms
            .insert(phase.into(), (started.elapsed().as_millis() as u64).into());
        result.map_err(|_| Failure::Timeout { phase })?
    }
}

pub(crate) async fn fetch(dict: &Dictionary) -> (Result<Dictionary, Failure>, Timings) {
    let mut timings = Timings::default();
    let result = async {
        let mut body = Vec::new();
        let serialized = Value::Dictionary(dict.clone()).to_writer_xml(&mut body);
        timings.request_bytes = body.len();
        serialized.map_err(|_| Failure::Plist)?;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(APPLE_ROOT_CA)).map_err(tls_failure)?;
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions().map_err(tls_failure)?
            .with_root_certificates(roots).with_no_client_auth();
        let tcp = timings.phase("connect", 20, async {
            tokio::net::TcpStream::connect(("gs.apple.com", 443)).await.map_err(|_| Failure::Network)
        }).await?;
        let name = rustls::pki_types::ServerName::try_from("gs.apple.com").map_err(|_| Failure::Tls("InvalidDnsNameError".into()))?;
        let mut tls = timings.phase("tls", 20, async {
            TlsConnector::from(Arc::new(config)).connect(name, tcp).await.map_err(tls_io_failure)
        }).await?;
        let header = format!("POST /TSS/controller?action=2 HTTP/1.1\r\nHost: gs.apple.com\r\nCache-Control: no-cache\r\nContent-type: text/xml; charset=\"utf-8\"\r\nUser-Agent: InetURL/1.0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        timings.phase("write", 20, async {
            tls.write_all(header.as_bytes()).await.map_err(|_| Failure::Network)?;
            tls.write_all(&body).await.map_err(|_| Failure::Network)?;
            tls.flush().await.map_err(|_| Failure::Network)
        }).await?;
        let mut reader = BufReader::new(tls);
        let status = timings.phase("first_byte", 90, line(&mut reader)).await?;
        let body = timings.phase("body", 30, response_body(&mut reader, &status)).await?;
        parse(&body)
    }.await;
    (result, timings)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn phase_reports_timeout_and_failure_timings() {
        let mut timings = Timings {
            request_bytes: 123,
            ..Timings::default()
        };
        let failure = timings
            .phase::<()>("first_byte", 0, std::future::pending())
            .await
            .unwrap_err();
        let mut report = failure.report();
        timings.attach(&mut report);
        assert_eq!(report["error"], "Timeout");
        assert_eq!(report["phase"], "first_byte");
        assert_eq!(report["request_bytes"], 123);
        assert!(report["timings_ms"]["first_byte"].is_u64());
        assert!(report["timings_ms"].get("body").is_none());
        assert!(matches!(
            timings
                .phase::<()>("body", 30, async { Err(Failure::Http(503)) })
                .await,
            Err(Failure::Http(503))
        ));
        assert!(timings.timings_ms["body"].is_u64());
        assert_eq!(
            timings.phase("write", 20, async { Ok(42) }).await.unwrap(),
            42
        );
        assert!(timings.timings_ms["write"].is_u64());
    }

    #[test]
    fn apple_root_fingerprint_and_store() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:X}", Sha256::digest(APPLE_ROOT_CA)),
            "B0B1730ECBC7FF4505142C49F1295E6EDA6BCAED7E2C68C5BE91B5A11001F024"
        );
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(APPLE_ROOT_CA))
            .unwrap();
        assert_eq!(roots.len(), 1);
    }
    #[test]
    fn tls_category_discards_payload() {
        let error = rustls::Error::General("sensitive-value".into());
        let failure = tls_io_failure(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        assert_eq!(
            failure.report(),
            serde_json::json!({"error":"Tls", "category":"General"})
        );
        assert!(!failure.report().to_string().contains("sensitive-value"));
    }
    #[test]
    fn equivalent_to_idevice() {
        let identifiers = plist_macro::plist!(dict { "BoardId": 12u64, "ChipID": 33056u64, "UniqueChipID": 123u64, "Ap,Test": true });
        let identity = plist_macro::plist!(dict { "Manifest": {
            "LoadableTrustCache": { "Trusted": true, "Digest": Value::Data(vec![1, 2]), "Info": { "RestoreRequestRules": [
                { "Conditions": { "ApRequiresImage4": true }, "Actions": { "EPRO": true, "Ignore": 255u64 } },
                { "Conditions": { "ApRawSecurityMode": false }, "Actions": { "Mismatch": true } }
            ] } },
            "PersonalizedDMG": { "Trusted": true, "Info": {} },
            "Untrusted": { "Trusted": false, "Info": {} }, "NoInfo": { "Trusted": true }
        } });
        let nonce = vec![3; 32];
        let mut own = request(&identifiers, nonce.clone(), &identity).unwrap();
        own.remove("@UUID");
        let mut reference = idevice::tss::TSSRequest::new();
        reference.add_ap_personalization_identifiers(&identifiers);
        reference.set_ap_img4_ticket(true);
        reference.set_bb_ticket(true);
        reference.add_common_tags(12, 33056, 123, Some(nonce), Some(vec![0; 20]));
        let parameters = plist_macro::plist!(dict { "ApProductionMode": true, "ApSecurityMode": true, "ApSupportsImg4": true });
        let rules = identity["Manifest"].as_dictionary().unwrap()["LoadableTrustCache"]
            .as_dictionary()
            .unwrap()["Info"]
            .as_dictionary()
            .unwrap()["RestoreRequestRules"]
            .as_array()
            .unwrap();
        reference
            .populate_from_manifest(&identity, &parameters, Some(rules))
            .unwrap();
        reference.remove("@UUID");
        assert_eq!(
            format!("TSSRequest {{ inner: {own:?} }}"),
            format!("{reference:?}")
        );
    }
    #[tokio::test]
    async fn transport_boundaries() {
        assert!(matches!(
            response(&mut &b"HTTP/1.1 307 Redirect\r\n"[..]).await,
            Err(Failure::Http(307))
        ));
        assert!(matches!(
            response(&mut &b"HTTP/1.1 200 OK\r\nContent-Length: 4194305\r\n\r\n"[..]).await,
            Err(Failure::TooLarge)
        ));
        assert_eq!(
            response(
                &mut &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"
                    [..]
            )
            .await
            .unwrap(),
            b"abc"
        );
        assert!(matches!(
            parse(b"STATUS=94&MESSAGE=private content"),
            Err(Failure::Apple(Some(94)))
        ));
    }
}

#[cfg(test)]
mod network_tests {
    // Manual: cargo test -p self-boot --offline -- --ignored tss_endpoint_reachable
    // Sends an empty request (no device data); Apple answers with a non-zero STATUS.
    #[tokio::test]
    #[ignore]
    async fn tss_endpoint_reachable() {
        let started = std::time::Instant::now();
        let (result, timings) = super::fetch(&plist::Dictionary::new()).await;
        eprintln!("{:?}, {timings:?} in {:?}", result.err(), started.elapsed());
    }
}
