//! IDE-side bootstrap and opt-in personalized DDI mounting; never launches a test plan.
mod archive;
mod dtx;
pub mod network;
mod tss;
pub mod xctest_trace;
pub use network::LocalBind;
pub mod ffi;
use idevice::{
    IdeviceService, ReadWrite, RsdService,
    cryptexd::{CryptexdClient, NONCE_DOMAIN_CRYPTEX, NonceDomain},
    dvt::{
        message::{AuxValue, Message},
        xctest::{dtx_services::*, types::XCTCapabilities},
    },
    heartbeat::HeartbeatClient,
    mobile_image_mounter::ImageMounter,
    provider::RsdProvider,
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use serde::Serialize;
use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use uuid::Uuid;
pub type Result<T> = std::result::Result<T, Error>;
const DEADLINE: Duration = Duration::from_secs(30);
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("{stage}: {source}")]
    Stage { stage: String, source: Box<Error> },
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error(transparent)]
    Device(#[from] idevice::IdeviceError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Plist(#[from] plist::Error),
}
#[derive(Clone, Debug, Serialize)]
pub enum ConnectionState {
    Connected,
    KeepaliveDegraded(String),
    Disconnected(String),
    Closed,
}
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub source: String,
    pub kind: String,
    pub detail: String,
}
impl Event {
    fn message(source: &str, message: &Message) -> Self {
        Self {
            source: source.into(),
            kind: "dtx".into(),
            detail: format!("{message:?}"),
        }
    }
    fn error(source: &str, detail: String) -> Self {
        Self {
            source: source.into(),
            kind: "disconnected".into(),
            detail,
        }
    }
}
#[derive(Debug, Serialize)]
pub struct DdiStatus {
    pub mounted: bool,
    pub developer_mode: bool,
    pub images: Vec<plist::Value>,
}
/// Keep this alive until all TestSessions have been dropped; dropping it closes the tunnel.
pub struct Session {
    pub rsd: RsdHandshake,
    adapter: AdapterHandle,
    state: Arc<watch::Sender<ConnectionState>>,
    events: broadcast::Sender<Event>,
    heartbeat: Option<JoinHandle<()>>,
    // Keep the RPPairing control connection alive alongside its tunnel.
    _pairing: Option<RemotePairingClient<RpPairingSocket<tokio::net::TcpStream>>>,
}
// Keep only the leading identifier from an enum's Debug representation.
fn error_variant(debug: &str) -> String {
    debug
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}
// Error payloads may contain identifiers or decoded replies: retain only enum tags.
fn probe_error(error: idevice::IdeviceError) -> String {
    use idevice::{IdeviceError as E, cryptexd::CryptexdError as C};
    match error {
        E::Cryptexd(C::RoutineFailed { .. }) => "Cryptexd.RoutineFailed",
        E::Cryptexd(C::MissingField(_)) => "Cryptexd.MissingField",
        E::Cryptexd(C::MalformedNonce(_)) => "Cryptexd.MalformedNonce",
        E::Cryptexd(_) => "Cryptexd.Other",
        E::NotFound => "NotFound",
        E::ServiceNotFound => "ServiceNotFound",
        E::Timeout => "Timeout",
        E::Socket(_) => "Socket",
        E::UnexpectedResponse(_) => "UnexpectedResponse",
        E::UnknownErrorType(_) => "UnknownErrorType",
        E::GetProhibited => "GetProhibited",
        E::Plist(_) => "Plist",
        E::Xpc(_) => "Xpc",
        E::DeviceLocked => "DeviceLocked",
        error => return error_variant(&format!("{error:?}")),
    }
    .into()
}
// Redact before truncating so a clipped identifier cannot leak its prefix.
fn probe_error_detail(error: &idevice::IdeviceError) -> Option<String> {
    let idevice::IdeviceError::InternalError(detail) = error else {
        return None;
    };
    let mut rest = detail.as_str();
    let mut redacted = String::new();
    while !rest.is_empty() {
        let bytes = rest.as_bytes();
        let uuid = bytes.len() >= 36
            && bytes[..36].iter().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    *b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                }
            });
        // Decimal runs are a subset of hexadecimal runs.
        let hex_len = bytes.iter().take_while(|b| b.is_ascii_hexdigit()).count();
        let length = if uuid {
            36
        } else if hex_len >= 8 {
            hex_len
        } else {
            0
        };
        if length > 0 {
            redacted.push_str("<redacted>");
            rest = &rest[length..];
        } else {
            let ch = rest.chars().next().unwrap();
            redacted.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    Some(redacted.chars().take(300).collect())
}
#[derive(Debug, Serialize)]
struct ProbeError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}
impl From<String> for ProbeError {
    fn from(error: String) -> Self {
        Self {
            error,
            detail: None,
        }
    }
}
impl From<&str> for ProbeError {
    fn from(error: &str) -> Self {
        error.to_owned().into()
    }
}
impl ProbeError {
    fn report(&self) -> serde_json::Value {
        serde_json::json!(self)
    }
    fn cache_report(&self) -> serde_json::Value {
        let mut report = self.report();
        report["status"] = serde_json::json!(if self.error == "NotFound" {
            "NotFound"
        } else {
            "error"
        });
        report
    }
}
async fn probe_deadline<T>(
    future: impl std::future::Future<Output = std::result::Result<T, idevice::IdeviceError>>,
) -> std::result::Result<T, ProbeError> {
    tokio::time::timeout(DEADLINE, future)
        .await
        .map_err(|_| ProbeError::from("DeadlineExceeded"))?
        .map_err(|error| ProbeError {
            detail: probe_error_detail(&error),
            error: probe_error(error),
        })
}
fn probe_report<T>(
    result: std::result::Result<T, ProbeError>,
    summarize: impl FnOnce(T) -> serde_json::Value,
) -> serde_json::Value {
    match result {
        Ok(value) => summarize(value),
        Err(error) => error.report(),
    }
}
fn probe_keys(value: &plist::Value) -> Vec<String> {
    value
        .as_dictionary()
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default()
}
// Scalars use their canonical byte width; containers use item count; strings
// use UTF-8 bytes. No nested scalar values are serialized.
fn probe_shape(value: &plist::Value) -> serde_json::Value {
    use plist::Value as V;
    let (kind, length) = match value {
        V::Data(v) => ("data", v.len()),
        V::String(v) => ("string", v.len()),
        V::Array(v) => ("array", v.len()),
        V::Dictionary(v) => ("dictionary", v.len()),
        V::Boolean(_) => ("boolean", 1),
        V::Integer(_) => ("integer", 8),
        V::Real(_) => ("real", 8),
        V::Date(_) => ("date", 8),
        V::Uid(_) => ("uid", 8),
        _ => ("unknown", 0),
    };
    serde_json::json!({"type": kind, "length": length})
}
fn probe_identifiers(values: &plist::Dictionary) -> serde_json::Value {
    let fields: serde_json::Map<String, serde_json::Value> = values
        .iter()
        .map(|(key, value)| (key.clone(), probe_shape(value)))
        .collect();
    serde_json::json!({"fields": fields})
}
fn probe_hash_candidates(value: &plist::Value, path: &str, output: &mut Vec<(String, Vec<u8>)>) {
    match value {
        plist::Value::Data(data) if data.len() == 48 => output.push((path.into(), data.clone())),
        plist::Value::Dictionary(dict) => {
            for (key, value) in dict {
                probe_hash_candidates(value, &format!("{path}.{key}"), output);
            }
        }
        plist::Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                probe_hash_candidates(value, &format!("{path}[{index}]"), output);
            }
        }
        _ => {}
    }
}
fn probe_bytes(bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({"type": "data", "length": bytes.len(), "sha256_8": probe_sha256_8(bytes)})
}
// SHA-256 compression (FIPS 180-4). Kept local to avoid changing the workspace
// lockfile or adding an unavailable dependency for this offline probe.
fn probe_sha256_8(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&(bytes.len() as u64).wrapping_mul(8).to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
    format!("{:08x}", state[0])
}
fn stage_error(stage: &str, source: impl Into<Error>) -> Error {
    Error::Stage {
        stage: stage.into(),
        source: Box::new(source.into()),
    }
}
async fn rp_stage<T>(
    stage: &str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout(DEADLINE, future)
        .await
        .map_err(|_| stage_error(stage, Error::Timeout("stage deadline")))?
        .map_err(|e| stage_error(stage, e))
}
pub async fn connect_rp(
    pairing_file: impl AsRef<Path>,
    addr: SocketAddr,
    binding: Option<LocalBind>,
) -> Result<Session> {
    let mut file = RpPairingFile::read_from_file(pairing_file)
        .await
        .map_err(|e| stage_error("pairing record", e))?;
    let via = binding
        .as_ref()
        .map(|b| format!(" via {}/{}", b.interface, b.local_ip))
        .unwrap_or_default();
    let stream = rp_stage(
        &format!("connect[pairing {addr}{via}]"),
        network::connect(addr, binding.as_ref()),
    )
    .await?;
    let mut rp = RemotePairingClient::new(RpPairingSocket::new(stream), "self-boot");
    // Verify the existing record only: no PIN prompt, pair-setup, or changes to disk.
    rp_stage("pair verify", async {
        rp.attempt_pair_verify().await?;
        rp.validate_pairing(&mut file).await?;
        Ok(())
    })
    .await?;
    let port = rp_stage("create tunnel listener", async {
        Ok(rp.create_tcp_listener().await?)
    })
    .await?;
    let tunnel_addr = SocketAddr::new(addr.ip(), port);
    let stream = rp_stage(
        &format!("connect[tunnel {tunnel_addr}{via}]"),
        network::connect(tunnel_addr, binding.as_ref()),
    )
    .await?;
    let tunnel = rp_stage("TLS", async {
        Ok(connect_tls_psk_tunnel_native(stream, rp.encryption_key()).await?)
    })
    .await?;
    let info = tunnel.info.clone();
    rp_stage("RSD", async {
        let mut adapter = Adapter::new(
            Box::new(tunnel.into_inner()),
            info.client_address
                .parse()
                .map_err(|e| Error::Protocol(format!("client IP: {e}")))?,
            info.server_address
                .parse()
                .map_err(|e| Error::Protocol(format!("server IP: {e}")))?,
        );
        adapter.set_mss(usize::from(info.mtu).saturating_sub(60));
        Session::establish(adapter.to_async_handle(), info.server_rsd_port, Some(rp)).await
    })
    .await
}
/// USB entry point. Uses idevice's portable usbmuxd address selection (environment override supported).
pub async fn connect_usb(udid: &str) -> Result<Session> {
    if udid.trim().is_empty() {
        return Err(Error::InvalidArgument("UDID is empty".into()));
    }
    tokio::time::timeout(DEADLINE, async {
        let addr = idevice::usbmuxd::UsbmuxdAddr::from_env_var()
            .map_err(|e| Error::InvalidArgument(e.to_string()))?;
        let mut mux = addr.connect(0).await?;
        let device = mux
            .get_devices()
            .await?
            .into_iter()
            .find(|d| {
                d.udid == udid && matches!(d.connection_type, idevice::usbmuxd::Connection::Usb)
            })
            .ok_or_else(|| Error::Protocol(format!("USB device not found: {udid}")))?;
        let provider = device.to_provider(addr, "self-boot-probe");
        let proxy = idevice::core_device_proxy::CoreDeviceProxy::connect(&provider).await?;
        let port = proxy.tunnel_info().server_rsd_port;
        Session::establish(
            proxy.create_software_tunnel()?.to_async_handle(),
            port,
            None,
        )
        .await
    })
    .await
    .map_err(|_| Error::Timeout("USB connection"))?
}
impl Session {
    async fn establish(
        mut adapter: AdapterHandle,
        port: u16,
        pairing: Option<RemotePairingClient<RpPairingSocket<tokio::net::TcpStream>>>,
    ) -> Result<Self> {
        let rsd = RsdHandshake::new(adapter.connect_to_service_port(port).await?).await?;
        let (events, _) = broadcast::channel(256);
        let (state, _) = watch::channel(ConnectionState::Connected);
        let mut session = Self {
            rsd,
            adapter,
            state: Arc::new(state),
            events,
            heartbeat: None,
            _pairing: pairing,
        };
        let heartbeat = tokio::time::timeout(
            DEADLINE,
            HeartbeatClient::connect_rsd(&mut session.adapter, &mut session.rsd),
        )
        .await;
        let mut heartbeat = match heartbeat {
            Ok(Ok(client)) => client,
            failed => {
                let detail = format!("heartbeat RSD connection failed: {failed:?}");
                keepalive_failed(&session.state, detail.clone());
                let _ = session.events.send(Event {
                    source: "heartbeat".into(),
                    kind: "keepalive_degraded".into(),
                    detail,
                });
                return Ok(session);
            }
        };
        let state = session.state.clone();
        let events = session.events.clone();
        session.heartbeat = Some(tokio::spawn(async move {
            let result: Result<()> = async {
                let mut interval = 30;
                loop {
                    interval = heartbeat.get_marco(interval).await?.saturating_add(5);
                    heartbeat.send_polo().await?;
                    let _ = events.send(Event {
                        source: "heartbeat".into(),
                        kind: "keepalive".into(),
                        detail: format!("Marco/Polo interval={}s", interval.saturating_sub(5)),
                    });
                }
            }
            .await;
            if let Err(e) = result {
                keepalive_failed(&state, format!("{e:?}"));
                let _ = events.send(Event {
                    source: "heartbeat".into(),
                    kind: "keepalive_degraded".into(),
                    detail: format!("{e:?}"),
                });
            }
        }));
        Ok(session)
    }
    pub fn state(&self) -> ConnectionState {
        self.state.borrow().clone()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
    pub fn watch_state(&self) -> watch::Receiver<ConnectionState> {
        self.state.subscribe()
    }
    pub async fn ddi_status(&mut self) -> Result<DdiStatus> {
        tokio::time::timeout(DEADLINE, async {
            let mut mounter = ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
            let images = mounter.copy_devices().await?;
            let mounted = images.iter().any(is_developer_image);
            let developer_mode = mounter.query_developer_mode_status().await?;
            Ok(DdiStatus {
                mounted,
                developer_mode,
                images,
            })
        })
        .await
        .map_err(|_| Error::Timeout("DDI status"))?
    }
    /// Fetch a fresh DDI ticket without changing device image state.
    pub async fn ddi_ticket(
        &mut self,
        ddi_dir: &Path,
        store_dir: &Path,
    ) -> Result<serde_json::Value> {
        self.ddi_operation(ddi_dir, store_dir, false).await
    }

    /// Single attempt: refuse disabled Developer Mode and skip existing developer images.
    pub async fn ddi_mount(
        &mut self,
        ddi_dir: &Path,
        store_dir: &Path,
    ) -> Result<serde_json::Value> {
        self.ddi_operation(ddi_dir, store_dir, true).await
    }

    async fn ddi_operation(
        &mut self,
        ddi_dir: &Path,
        store_dir: &Path,
        mount: bool,
    ) -> Result<serde_json::Value> {
        use serde_json::json;
        use tracing::instrument::WithSubscriber;
        async {
            let mut report = json!({"status":"error", "phases":[]});
            let outcome: std::result::Result<(), ProbeError> = async {
                if mount {
                    let images = ddi_phase(&mut report, "copy_devices", async {
                        probe_deadline(async {
                            let mut client =
                                ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                            client.copy_devices().await
                        })
                        .await
                    })
                    .await?;
                    if let Some(skipped) = ddi_skip(&images) {
                        report["status"] = skipped["status"].clone();
                        report["reason"] = skipped["reason"].clone();
                        return Ok(());
                    }
                    ddi_phase(&mut report, "developer_mode", async {
                        let enabled = probe_deadline(async {
                            let mut client =
                                ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                            client.query_developer_mode_status().await
                        })
                        .await?;
                        if enabled {
                            Ok(())
                        } else {
                            Err("DeveloperModeDisabled".into())
                        }
                    })
                    .await?;
                }
                let (image, trustcache, ticket) = self
                    .ddi_signature(ddi_dir, store_dir, mount, &mut report)
                    .await?;
                if mount {
                    ddi_phase(&mut report, "upload", async {
                        ddi_device_deadline(Duration::from_secs(120), async {
                            let mut client =
                                ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                            client
                                .upload_image("Personalized", &image, ticket.clone())
                                .await
                        })
                        .await
                    })
                    .await?;
                    ddi_phase(&mut report, "mount", async {
                        ddi_device_deadline(Duration::from_secs(60), async {
                            let mut client =
                                ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                            client
                                .mount_image("Personalized", ticket, Some(trustcache), None)
                                .await
                        })
                        .await
                    })
                    .await?;
                    ddi_phase(&mut report, "verify_mounted", async {
                        let images = probe_deadline(async {
                            let mut client =
                                ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                            client.copy_devices().await
                        })
                        .await?;
                        if ddi_skip(&images).is_some() {
                            Ok(())
                        } else {
                            Err("NotMounted".into())
                        }
                    })
                    .await?;
                }
                report["status"] = json!(if mount { "mounted" } else { "success" });
                Ok(())
            }
            .await;
            if let Err(error) = outcome {
                report["error"] = json!(error.error);
                if let Some(detail) = error.detail {
                    report["detail"] = json!(detail);
                }
            }
            Ok(report)
        }
        .with_subscriber(tracing::subscriber::NoSubscriber::default())
        .await
    }

    // Shared P1/P2 preparation. P1 always fetches a fresh ticket; P2 prefers device cache.
    async fn ddi_signature(
        &mut self,
        ddi_dir: &Path,
        store_dir: &Path,
        prefer_cache: bool,
        report: &mut serde_json::Value,
    ) -> std::result::Result<(Vec<u8>, Vec<u8>, Vec<u8>), ProbeError> {
        use serde_json::json;
        use sha2::{Digest, Sha384};
        let (manifest, image, trustcache, image_hash, assets) =
            ddi_phase(report, "assets", async {
                let mut assets = serde_json::Map::new();
                let mut contents = Vec::new();
                for name in ["BuildManifest.plist", "Image.dmg", "Image.dmg.trustcache"] {
                    let bytes =
                        std::fs::read(ddi_dir.join(name)).map_err(|_| "AssetRead".to_owned())?;
                    assets.insert(name.into(), probe_bytes(&bytes));
                    contents.push(bytes);
                }
                let manifest: plist::Dictionary =
                    plist::from_bytes(&contents[0]).map_err(|_| "BuildManifestParse".to_owned())?;
                let image_hash = Sha384::digest(&contents[1]).to_vec();
                let trustcache = contents.pop().unwrap();
                let image = contents.pop().unwrap();
                Ok((manifest, image, trustcache, image_hash, assets))
            })
            .await?;
        report["assets"] = json!(assets);
        macro_rules! mounter {
            ($call:ident $(, $arg:expr)*) => {
                probe_deadline(async {
                    let mut client = ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                    client.$call($($arg),*).await
                })
            };
        }
        let identifiers = ddi_phase(
            report,
            "identifiers",
            mounter!(query_personalization_identifiers, None),
        )
        .await?;
        report["identifiers"] = probe_identifiers(&identifiers);
        let identity = ddi_phase(report, "identity", async {
            let number = |key| {
                identifiers
                    .get(key)
                    .and_then(plist::Value::as_unsigned_integer)
                    .ok_or_else(|| "MissingIdentifier".to_owned())
            };
            let identity = idevice::tss::select_build_identity(
                &manifest,
                number("BoardId")?,
                number("ChipID")?,
                None,
            )
            .map_err(probe_error)?;
            number("UniqueChipID")?;
            Ok(identity)
        })
        .await?;
        report["variant"] = json!(
            identity
                .get("Info")
                .and_then(plist::Value::as_dictionary)
                .and_then(|i| i.get("Variant"))
                .and_then(plist::Value::as_string)
        );
        let cached = ddi_phase(report, "cache", async {
            match mounter!(
                query_personalization_manifest,
                "DeveloperDiskImage",
                image_hash.clone()
            )
            .await
            {
                Ok(data) if data.is_empty() => Err("EmptyTicket".into()),
                Ok(data) => Ok((Some(data), None)),
                Err(error) if matches!(error.error.as_str(), "NotFound" | "InternalError") => {
                    Ok((None, Some(error.cache_report())))
                }
                Err(error) => Err(error),
            }
        })
        .await?;
        report["cache"] = match &cached {
            (Some(data), _) => json!({"status":"found", "manifest":probe_bytes(data)}),
            (_, Some(error)) => error.clone(),
            _ => unreachable!(),
        };
        let ticket = if let (true, Some(data)) = (prefer_cache, &cached.0) {
            report["signature_source"] = json!("device_cache");
            data.clone()
        } else {
            // A failed cache query closes its socket: nonce always uses a new connection.
            let nonce = ddi_phase(
                report,
                "nonce",
                mounter!(query_nonce, Some("DeveloperDiskImage")),
            )
            .await?;
            report["nonce"] = probe_bytes(&nonce);
            let request = ddi_phase(report, "tss_request", async {
                tss::request(&identifiers, nonce, identity).map_err(ProbeError::from)
            })
            .await?;
            let started = std::time::Instant::now();
            let (result, timings) = tss::fetch(&request).await;
            let result = result.map_err(|error| {
                report["tss"] = error.report();
                timings.attach(&mut report["tss"]);
                ProbeError::from("TssFailure")
            }).and_then(|response| {
                report["tss"] = json!({"status":"success", "response_keys":response.keys().collect::<Vec<_>>()});
                timings.attach(&mut report["tss"]);
                response.get("ApImg4Ticket").and_then(plist::Value::as_data).filter(|ticket| !ticket.is_empty()).map(<[u8]>::to_vec).ok_or_else(|| "MissingTicket".into())
            });
            ddi_record_phase(report, "tss", started, &result);
            let ticket = result?;
            report["signature_source"] = json!("tss");
            if let Some(cached) = cached.0 {
                report["matches_device_manifest"] = json!(cached == ticket);
            }
            ticket
        };
        report["ticket"] = probe_bytes(&ticket);
        let stored = ddi_phase(report, "store_ticket", async {
            store_ddi_ticket(store_dir, &image_hash, &ticket)
        })
        .await?;
        report["stored"] = stored;
        Ok((image, trustcache, ticket))
    }

    /// Read-only DDI capability probe; each request uses a fresh connection.
    pub async fn ddi_probe(&mut self) -> Result<serde_json::Value> {
        use serde_json::json;
        use tracing::instrument::WithSubscriber;
        // idevice logs full decoded plist/XPC at debug level. WithDispatch enters
        // a no-op subscriber on EVERY poll, including after task migration.
        async {
            let mut names: Vec<_> = self.rsd.services.iter().collect();
            names.sort_by_key(|(name, _)| *name);
            let services: Vec<_> = names.into_iter()
                .filter(|(n, _)| n.contains("mobile_image_mounter") || n.contains("cryptexd"))
                .map(|(name, service)| json!({
                    "name": name, "has_entitlement": true,
                    "port_parser_valid": true, "port_nonzero": service.port != 0,
                    "features": service.features, "service_version": service.service_version,
                    "properties_keys": null,
                    "properties_keys_note": "parser retains only selected service Properties fields"
                })).collect();
            let mut property_keys: Vec<_> = self.rsd.properties.keys().collect();
            property_keys.sort();
            let rsd = json!({
                "source": "BootHandle parsed RsdHandshake", "raw_available": false,
                "raw_service_count": null, "parser_discarded_count": null,
                "parsed_service_count": self.rsd.services.len(), "services": services,
                "properties_keys": property_keys,
                "features_keys": self.rsd.properties.get("Features").map(probe_keys),
                "note": "raw Services and root Features are not retained by idevice RsdHandshake; absence in parsed table does not establish service absence"
            });
            // Each timeout covers connection establishment AND the request.
            macro_rules! mounter {
                ($call:ident $(, $arg:expr)*) => {
                    probe_deadline(async {
                        let mut client = ImageMounter::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                        client.$call($($arg),*).await
                    }).await
                };
            }
            macro_rules! cryptex {
                ($call:ident $(, $arg:expr)*) => {
                    probe_deadline(async {
                        let client = CryptexdClient::connect_rsd(&mut self.adapter, &mut self.rsd).await?;
                        client.$call($($arg),*).await
                    }).await
                };
            }
            let devices = mounter!(copy_devices);
            let mut candidates = Vec::new();
            let copy_devices = match devices {
                Ok(entries) => {
                    let summaries: Vec<_> = entries.iter().enumerate().map(|(index, entry)| {
                        probe_hash_candidates(entry, &format!("entry[{index}]"), &mut candidates);
                        json!({"keys": probe_keys(entry), "type": probe_shape(entry),
                            "image_type": entry.as_dictionary().and_then(|d| d.get("ImageType"))
                                .and_then(plist::Value::as_string),
                            "developer": is_developer_image(entry)})
                    }).collect();
                    json!({"count": entries.len(), "entries": summaries})
                }
                Err(error) => error.report(),
            };
            let developer_mode = probe_report(mounter!(query_developer_mode_status), |v| json!({"enabled": v}));
            let identifiers = probe_report(mounter!(query_personalization_identifiers, Some("DeveloperDiskImage")), |v| probe_identifiers(&v));
            let nonce = probe_report(mounter!(query_nonce, Some("DeveloperDiskImage")), |v| probe_bytes(&v));
            let mut manifests = Vec::new();
            for (source, hash) in candidates {
                let result = mounter!(query_personalization_manifest, "DeveloperDiskImage", hash.clone());
                let report = match result {
                    Ok(data) => json!({"status": "found", "manifest": probe_bytes(&data)}),
                    Err(error) => error.cache_report(),
                };
                manifests.push(json!({"source": source, "hash": probe_bytes(&hash), "result": report}));
            }
            let cryptex_identifiers = probe_report(cryptex!(read_personalization_identifiers), |v| probe_identifiers(&v));
            // copy_installed loses the distinction between absent and empty arrays.
            // Invoke its exact read-only routine instead and retain shape metadata.
            let copied = cryptex!(invoke, "copy-installed", idevice::xpc::Dictionary::new());
            let copied = probe_report(copied, |value| {
                let array_value = value.as_ref().and_then(plist::Value::as_dictionary)
                    .and_then(|d| d.get("remote-cryptex-array"));
                json!({"response": value.as_ref().map(probe_shape),
                    "array_present": array_value.is_some(),
                    "array_type": array_value.map(probe_shape),
                    "count": array_value.and_then(plist::Value::as_array).map(Vec::len),
                    "entries": array_value.and_then(plist::Value::as_array)
                        .map(|a| a.iter().map(probe_shape).collect::<Vec<_>>())})
            });
            let index_nonce = probe_report(cryptex!(get_nonce, NonceDomain::Index(NONCE_DOMAIN_CRYPTEX)), |v| probe_bytes(&v));
            Ok(json!({"schema_version": 1, "read_only": true, "rsd": rsd,
                "mounter": {"copy_devices": copy_devices,
                    "query_developer_mode_status": developer_mode,
                    "query_personalization_identifiers": identifiers, "query_nonce": nonce,
                    "query_personalization_manifest": {"candidates": manifests,
                        "note": "queries use every 48-byte Data field recursively found in CopyDevices; candidate is not verified as personalized image SHA-384"}},
                "cryptexd": {"read_personalization_identifiers": cryptex_identifiers,
                    "copy_installed": copied,
                    "get_nonce": {"Index": {"domain": NONCE_DOMAIN_CRYPTEX, "result": index_nonce},
                        "Handle": {"error": "UnavailableManifestHandle",
                            "note": "requires Cryptex1 Manifest handle; CopyDevices is not a build Manifest; no constant substituted"}}}}))
        }.with_subscriber(tracing::subscriber::NoSubscriber::default()).await
    }
    pub async fn start_test_sessions(&mut self, session_uuid: Uuid) -> Result<TestSessions> {
        let control = self.open_dtx("control").await?;
        control.channel(XCTEST_PROXY_IDE_TO_DAEMON).await?;
        let control_reply = control
            .call(
                1,
                IDE_INITIATE_CTRL_SESSION_WITH_CAPABILITIES,
                vec![AuxValue::Array(archive::capabilities(
                    XCTCapabilities::empty().capabilities,
                )?)],
            )
            .await?;
        let test = self.open_dtx("test").await?;
        test.channel(XCTEST_PROXY_IDE_TO_DAEMON).await?;
        let test_reply = test
            .call(
                1,
                IDE_INITIATE_SESSION_WITH_IDENTIFIER_CAPABILITIES,
                vec![
                    AuxValue::Array(archive::nsuuid(session_uuid)?),
                    AuxValue::Array(archive::capabilities(
                        XCTCapabilities::ide_defaults().capabilities,
                    )?),
                ],
            )
            .await?;
        Ok(TestSessions {
            control,
            _test: test,
            session_uuid,
            control_reply: archive::readable_reply(&control_reply)?,
            test_reply: archive::readable_reply(&test_reply)?,
        })
    }
    async fn open_dtx(&mut self, label: &'static str) -> Result<dtx::Dtx> {
        let port = self
            .rsd
            .services
            .get(TESTMANAGERD_RSD_SERVICE)
            .ok_or_else(|| {
                Error::Protocol(format!("missing RSD service {TESTMANAGERD_RSD_SERVICE}"))
            })?
            .port;
        let stream: Box<dyn ReadWrite> =
            tokio::time::timeout(DEADLINE, self.adapter.connect_to_service_port(port))
                .await
                .map_err(|_| Error::Timeout("testmanagerd connection"))??;
        dtx::Dtx::open(stream, label, self.events.clone(), self.state.clone()).await
    }
    /// Drop TestSessions first. Explicit shutdown also terminates the adapter task.
    pub async fn close(&mut self) -> Result<()> {
        if let Some(task) = self.heartbeat.take() {
            task.abort();
            let _ = task.await;
        }
        let result = self.adapter.close().await;
        self._pairing.take();
        self.state.send_replace(ConnectionState::Closed);
        result.map_err(Error::from)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = &self.heartbeat {
            task.abort();
        }
        self.state.send_replace(ConnectionState::Closed);
    }
}
fn keepalive_failed(state: &watch::Sender<ConnectionState>, detail: String) {
    state.send_if_modified(|current| {
        if matches!(current, ConnectionState::Connected) {
            *current = ConnectionState::KeepaliveDegraded(detail);
            true
        } else {
            false
        }
    });
}
fn ddi_skip(images: &[plist::Value]) -> Option<serde_json::Value> {
    images
        .iter()
        .any(is_developer_image)
        .then(|| serde_json::json!({"status":"skipped", "reason":"AlreadyMounted"}))
}

fn ddi_record_phase<T>(
    report: &mut serde_json::Value,
    phase: &str,
    started: std::time::Instant,
    result: &std::result::Result<T, ProbeError>,
) {
    let mut entry = serde_json::json!({"phase":phase, "ms":started.elapsed().as_millis() as u64, "ok":result.is_ok()});
    if let Err(error) = result {
        entry["error"] = serde_json::json!(error.error);
        if let Some(detail) = &error.detail {
            entry["detail"] = serde_json::json!(detail);
        }
    }
    report["phases"].as_array_mut().unwrap().push(entry);
}

async fn ddi_phase<T>(
    report: &mut serde_json::Value,
    phase: &str,
    operation: impl std::future::Future<Output = std::result::Result<T, ProbeError>>,
) -> std::result::Result<T, ProbeError> {
    let started = std::time::Instant::now();
    let result = operation.await;
    ddi_record_phase(report, phase, started, &result);
    result
}

async fn ddi_device_deadline<T>(
    deadline: Duration,
    future: impl std::future::Future<Output = std::result::Result<T, idevice::IdeviceError>>,
) -> std::result::Result<T, ProbeError> {
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| ProbeError::from("DeadlineExceeded"))?
        .map_err(|error| ProbeError {
            detail: probe_error_detail(&error),
            error: probe_error(error),
        })
}

fn store_ddi_ticket(
    store_dir: &Path,
    image_hash: &[u8],
    ticket: &[u8],
) -> std::result::Result<serde_json::Value, ProbeError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let prefix: String = image_hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    let filename = format!("ddi-ticket-{prefix}.im4m");
    let temp = store_dir.join(format!(".ddi-ticket-{}.tmp", Uuid::new_v4()));
    let saved = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(ticket)?;
        file.sync_all()?;
        std::fs::rename(&temp, store_dir.join(&filename))?;
        Ok(())
    })();
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
        return Err("TicketStore".into());
    }
    Ok(serde_json::json!({"filename":filename, "length":ticket.len()}))
}

fn is_developer_image(image: &plist::Value) -> bool {
    image.as_dictionary().is_some_and(|d| {
        d.get("ImageType")
            .and_then(plist::Value::as_string)
            .is_some_and(|s| matches!(s, "Developer" | "Personalized"))
            || d.get("MountPath")
                .and_then(plist::Value::as_string)
                .is_some_and(|s| matches!(s, "/Developer" | "/System/Developer"))
    })
}
pub struct TestSessions {
    control: dtx::Dtx,
    _test: dtx::Dtx,
    pub session_uuid: Uuid,
    pub control_reply: serde_json::Value,
    pub test_reply: serde_json::Value,
}
impl TestSessions {
    pub async fn authorize_pid(&self, pid: u32) -> Result<String> {
        if pid == 0 {
            return Err(Error::InvalidArgument("PID must be positive".into()));
        }
        let reply = self
            .control
            .call(
                1,
                IDE_AUTHORIZE_TEST_SESSION,
                vec![AuxValue::archived_value(plist::Value::Integer(
                    u64::from(pid).into(),
                ))],
            )
            .await?;
        if reply.data.as_ref().and_then(plist::Value::as_boolean) == Some(false) {
            return Err(Error::Protocol(
                "testmanagerd refused PID authorization".into(),
            ));
        }
        Ok(format!("{reply:?}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keepalive_failure_is_degraded_and_cannot_overwrite_disconnect_or_close() {
        let (state, _) = watch::channel(ConnectionState::Connected);
        keepalive_failed(&state, "heartbeat EOF".into());
        assert!(matches!(
            &*state.borrow(),
            ConnectionState::KeepaliveDegraded(_)
        ));
        state.send_replace(ConnectionState::Disconnected("transport EOF".into()));
        keepalive_failed(&state, "late heartbeat failure".into());
        assert!(
            matches!(&*state.borrow(), ConnectionState::Disconnected(reason) if reason == "transport EOF")
        );
        state.send_replace(ConnectionState::Closed);
        keepalive_failed(&state, "late heartbeat failure".into());
        assert!(matches!(&*state.borrow(), ConnectionState::Closed));
    }
    #[test]
    fn probe_error_fallback_discards_payloads() {
        let result = probe_error(idevice::IdeviceError::InternalError(
            "sensitive-value".into(),
        ));
        assert_eq!(result, "InternalError");
        assert!(!result.contains("sensitive-value"));
        for debug in [
            "Variant(\"sensitive-value\")",
            "Variant { secret: sensitive-value }",
            "Variant sensitive-value",
        ] {
            assert_eq!(error_variant(debug), "Variant");
        }
    }
    #[tokio::test]
    async fn internal_error_detail_is_redacted_and_reported() {
        let secrets = [
            "abcdef0123456789",
            "123456789012",
            "01234567-89ab-cdef-0123-456789abcdef",
        ];
        let message = format!(
            "Manifest rejected: {} {} {}",
            secrets[0], secrets[1], secrets[2]
        );
        let failure =
            probe_deadline(async { Err::<bool, _>(idevice::IdeviceError::InternalError(message)) })
                .await
                .unwrap_err();
        let report = failure.cache_report();
        assert_eq!(report["error"], "InternalError");
        assert_eq!(report["status"], "error");
        assert_eq!(
            report["detail"],
            "Manifest rejected: <redacted> <redacted> <redacted>"
        );
        for secret in secrets {
            assert!(!report.to_string().contains(secret));
        }
        assert_eq!(
            probe_report(Err::<bool, _>(failure), |v| serde_json::json!(v))["detail"],
            report["detail"]
        );
        assert_eq!(
            probe_error_detail(&idevice::IdeviceError::InternalError(
                "Manifest unavailable".into()
            ))
            .as_deref(),
            Some("Manifest unavailable")
        );
        assert_eq!(probe_error_detail(&idevice::IdeviceError::NotFound), None);
        let long = format!("{}abcdef0123456789", "界".repeat(295));
        let detail = probe_error_detail(&idevice::IdeviceError::InternalError(long)).unwrap();
        assert_eq!(detail.chars().count(), 300);
        assert!(!detail.contains("abcdef"));
    }
    #[test]
    fn probe_sha256_vectors_and_privacy() {
        assert_eq!(probe_sha256_8(b""), "e3b0c442");
        assert_eq!(probe_sha256_8(b"abc"), "ba7816bf");
        assert_eq!(probe_sha256_8(&[b'a'; 1000]), "41edece4");
        let identifiers: plist::Dictionary = [(
            String::from("ECID"),
            plist::Value::String("sensitive-value".into()),
        )]
        .into_iter()
        .collect();
        let report = probe_identifiers(&identifiers).to_string();
        assert!(!report.contains("sensitive-value"));
        assert_eq!(
            probe_error(idevice::IdeviceError::UnexpectedResponse(
                "sensitive-value".into()
            )),
            "UnexpectedResponse"
        );
        let nested = plist::Value::Array(vec![
            plist::Value::Data(vec![7; 48]),
            plist::Value::Data(vec![8; 47]),
        ]);
        let mut candidates = Vec::new();
        probe_hash_candidates(&nested, "entry", &mut candidates);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "entry[0]");
    }
    #[tokio::test]
    async fn probe_failure_remains_json() {
        let failure =
            probe_deadline(async { Err::<bool, _>(idevice::IdeviceError::NotFound) }).await;
        assert_eq!(
            probe_report(failure, |v| serde_json::json!(v)),
            serde_json::json!({"error": "NotFound"})
        );
    }
    #[test]
    fn ddi_mount_skips_existing_developer_image() {
        let image = plist::Value::Dictionary(
            [(
                "MountPath".to_owned(),
                plist::Value::String("/System/Developer".into()),
            )]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            ddi_skip(&[image]),
            Some(serde_json::json!({"status":"skipped", "reason":"AlreadyMounted"}))
        );
        assert_eq!(ddi_skip(&[]), None);
        let other = plist::Value::Dictionary(
            [(
                "ImageType".to_owned(),
                plist::Value::String("Cryptex".into()),
            )]
            .into_iter()
            .collect(),
        );
        assert_eq!(ddi_skip(&[other]), None);
    }

    #[test]
    fn ddi_phases_never_serialize_success_payloads() {
        let raw = b"raw-ticket-nonce-ECID-123456789012".to_vec();
        let mut report = serde_json::json!({"phases":[], "ticket":probe_bytes(&raw)});
        ddi_record_phase(
            &mut report,
            "cache",
            std::time::Instant::now(),
            &Ok(raw.clone()),
        );
        let error = ProbeError {
            error: "InternalError".into(),
            detail: probe_error_detail(&idevice::IdeviceError::InternalError(
                "ECID 123456789012".into(),
            )),
        };
        ddi_record_phase(
            &mut report,
            "mount",
            std::time::Instant::now(),
            &Err::<Vec<u8>, _>(error),
        );
        let serialized = report.to_string();
        assert!(!serialized.contains("123456789012"));
        assert!(!serialized.contains("raw-ticket"));
        assert!(!serialized.contains(&serde_json::to_string(&raw).unwrap()));
        assert_eq!(report["phases"][0]["ok"], true);
        assert_eq!(report["phases"][1]["ok"], false);
    }

    #[test]
    fn ddi_classifies_only_developer_images() {
        let make = |t: &str| {
            plist::Value::Dictionary(
                [("ImageType".to_owned(), plist::Value::String(t.into()))]
                    .into_iter()
                    .collect(),
            )
        };
        assert!(is_developer_image(&make("Personalized")));
        assert!(is_developer_image(&make("Developer")));
        assert!(!is_developer_image(&make("Cryptex")));
    }
    #[tokio::test]
    async fn empty_udid_fails_before_io() {
        assert!(matches!(
            connect_usb(" ").await,
            Err(Error::InvalidArgument(_))
        ));
    }
    #[test]
    fn errors_keep_context() {
        assert_eq!(
            Error::Timeout("DDI status").to_string(),
            "timed out: DDI status"
        );
        assert_eq!(
            Error::InvalidArgument("PID".into()).to_string(),
            "invalid argument: PID"
        );
    }
}
