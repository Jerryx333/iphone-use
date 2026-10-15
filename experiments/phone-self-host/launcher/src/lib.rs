mod image;
// On-device RemotePairing/RSD host. Credential bytes never enter public status.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures::FutureExt;
use idevice::{
    IdeviceError, RsdService,
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    services::{
        dvt::xctest::{TestConfig, XCUITestService, listener::XCUITestListener},
        installation_proxy::InstallationProxyClient,
        lockdown::LockdownClient,
    },
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;
use std::{
    ffi::{CStr, CString, c_char},
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    runtime::Runtime,
    task::JoinHandle,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Deserialize)]
struct Config {
    mode: String,
    device_host: String,
    #[serde(default = "pairing_port")]
    device_port: u16,
    #[serde(default)]
    bridge_port: Option<u16>,
    #[serde(default)]
    bridge_token: Option<String>,
    #[serde(default)]
    pair_record_base64: Option<String>,
    #[serde(default)]
    runner_bundle: Option<String>,
    #[serde(default)]
    runner_token: Option<String>,
    #[serde(default)]
    daemon_token: Option<String>,
    #[serde(default)]
    daemon_password: Option<String>,
    #[serde(default)]
    developer_image_root: Option<String>,
}
fn pairing_port() -> u16 {
    49152
}
impl Drop for Config {
    fn drop(&mut self) {
        for field in [
            &mut self.bridge_token,
            &mut self.pair_record_base64,
            &mut self.runner_token,
            &mut self.daemon_token,
            &mut self.daemon_password,
        ] {
            if let Some(secret) = field {
                secret.zeroize();
            }
        }
    }
}
#[derive(Clone, Serialize)]
struct Status {
    phase: String,
    build_id: &'static str,
    error_code: Option<String>,
    protocol_error_code: Option<i32>,
    protocol_error_subcode: Option<i32>,
    pair_record_available: bool,
    ios: Option<String>,
    developer_services: bool,
    test_plan_started: bool,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            phase: "stopped".into(),
            build_id: env!("IPU_HOST_BUILD_ID"),
            error_code: None,
            protocol_error_code: None,
            protocol_error_subcode: None,
            pair_record_available: false,
            ios: None,
            developer_services: false,
            test_plan_started: false,
        }
    }
}
#[derive(Default)]
struct Host {
    generation: u64,
    status: Status,
    task: Option<JoinHandle<()>>,
    record: Option<Zeroizing<Vec<u8>>>,
}
tokio::task_local! { static SESSION_GENERATION: u64; }
static HOST: OnceLock<Mutex<Host>> = OnceLock::new();
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
fn host() -> &'static Mutex<Host> {
    HOST.get_or_init(|| Mutex::new(Host::default()))
}
fn update(operation: impl FnOnce(&mut Host)) {
    let Ok(generation) = SESSION_GENERATION.try_with(|id| *id) else {
        return;
    };
    if let Ok(mut host) = host().lock() {
        if host.generation == generation {
            operation(&mut host);
        }
    }
}
fn set_phase(phase: &str) {
    update(|host| host.status.phase = phase.into());
}
fn fail(code: &str) {
    update(|host| {
        host.status.phase = "failed".into();
        if host.status.error_code.is_none() {
            host.status.error_code = Some(code.into());
        }
    });
}
fn valid_token(token: Option<&str>) -> bool {
    token.is_some_and(|v| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit()))
}

async fn connect(cfg: &Config, port: u16) -> Result<TcpStream, &'static str> {
    let mut socket = if let Some(bridge_port) = cfg.bridge_port {
        TcpStream::connect(("127.0.0.1", bridge_port))
            .await
            .map_err(|_| "bridge_connect_failed")?
    } else {
        TcpStream::connect((cfg.device_host.as_str(), port))
            .await
            .map_err(|_| "device_connect_failed")?
    };
    if cfg.bridge_port.is_some() {
        let mut request = Zeroizing::new(
            serde_json::to_vec(&serde_json::json!({"port":port,"token":cfg.bridge_token}))
                .map_err(|_| "bridge_request_failed")?,
        );
        request.push(b'\n');
        socket
            .write_all(&request)
            .await
            .map_err(|_| "bridge_write_failed")?;
        let mut ack = [0u8; 1];
        socket
            .read_exact(&mut ack)
            .await
            .map_err(|_| "bridge_ack_failed")?;
        if ack[0] != 1 {
            return Err("bridge_rejected");
        }
    }
    Ok(socket)
}

async fn tunnel(cfg: &Config) -> Result<(AdapterHandle, RsdHandshake, u16), &'static str> {
    let mut record = if let Some(encoded) = &cfg.pair_record_base64 {
        let bytes = Zeroizing::new(B64.decode(encoded).map_err(|_| "invalid_pair_record")?);
        RpPairingFile::from_bytes(&bytes).map_err(|_| "invalid_pair_record")?
    } else if cfg.mode == "pair" {
        RpPairingFile::generate("PhoneUseSelfHost")
    } else {
        return Err("pairing_required");
    };
    set_phase("connecting");
    let socket = connect(cfg, cfg.device_port).await?;
    let mut rpc = RemotePairingClient::new(RpPairingSocket::new(socket), "PhoneUseSelfHost");
    rpc.attempt_pair_verify()
        .await
        .map_err(|_| "pairing_handshake_failed")?;
    set_phase("verifying_pairing");
    if rpc.validate_pairing(&mut record).await.is_err() {
        if cfg.mode != "pair" {
            return Err("pairing_invalid");
        }
        set_phase("awaiting_pairing_approval");
        rpc.pair(&mut record, || async { "000000".to_string() })
            .await
            .map_err(|error| {
                // Numeric domains identify the failure without exposing the
                // peer's error text, device metadata or pairing material.
                update(|host| {
                    host.status.protocol_error_code = Some(error.code());
                    host.status.protocol_error_subcode = Some(error.sub_code());
                });
                "pairing_approval_failed"
            })?;
    }
    let bytes = record.to_bytes();
    update(|host| {
        host.record = Some(Zeroizing::new(bytes));
        host.status.pair_record_available = true;
    });
    set_phase("opening_tunnel");
    let port = rpc
        .create_tcp_listener()
        .await
        .map_err(|_| "tunnel_listener_failed")?;
    let socket = connect(cfg, port).await?;
    let tunnel = connect_tls_psk_tunnel_native(socket, rpc.encryption_key())
        .await
        .map_err(|_| "tunnel_handshake_failed")?;
    let client = tunnel
        .info
        .client_address
        .parse()
        .map_err(|_| "tunnel_address_failed")?;
    let device = tunnel
        .info
        .server_address
        .parse()
        .map_err(|_| "tunnel_address_failed")?;
    let mtu = tunnel.info.mtu as usize;
    let rsd_port = tunnel.info.server_rsd_port;
    let mut adapter = Adapter::new(Box::new(tunnel.into_inner()), client, device);
    adapter.set_mss(mtu.saturating_sub(60));
    let mut handle = adapter.to_async_handle();
    let stream = handle
        .connect(rsd_port)
        .await
        .map_err(|_| "rsd_connect_failed")?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|_| "rsd_handshake_failed")?;
    Ok((handle, handshake, rsd_port))
}
struct Events;
impl XCUITestListener for Events {
    async fn did_begin_executing_test_plan(&mut self) -> Result<(), IdeviceError> {
        update(|host| {
            host.status.phase = "running".into();
            host.status.test_plan_started = true;
        });
        Ok(())
    }
    async fn did_fail_to_bootstrap(&mut self, _: &str) -> Result<(), IdeviceError> {
        fail("runner_bootstrap_failed");
        Ok(())
    }
    async fn initialization_for_ui_testing_did_fail(
        &mut self,
        _: &str,
    ) -> Result<(), IdeviceError> {
        fail("automation_init_failed");
        Ok(())
    }
}
async fn session(cfg: Config) -> Result<(), &'static str> {
    let (mut adapter, mut handshake, rsd_port) =
        tokio::time::timeout(Duration::from_secs(120), tunnel(&cfg))
            .await
            .map_err(|_| "tunnel_timeout")??;
    set_phase("reading_device");
    let mut lockdown = LockdownClient::connect_rsd(&mut adapter, &mut handshake)
        .await
        .map_err(|_| "device_info_connect_failed")?;
    let version = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(|_| "device_info_failed")?;
    let version = version
        .as_string()
        .ok_or("device_version_invalid")?
        .to_owned();
    let major = version
        .split('.')
        .next()
        .and_then(|v| v.parse::<u8>().ok())
        .ok_or("device_version_invalid")?;
    let mut developer_services = handshake
        .services
        .contains_key("com.apple.dt.testmanagerd.remote")
        && handshake
            .services
            .contains_key("com.apple.instruments.dtservicehub");
    update(|host| {
        host.status.ios = Some(version);
        host.status.developer_services = developer_services;
    });
    if cfg.mode != "launch" {
        set_phase("rsd_verified");
        return Ok(());
    }
    if !developer_services {
        let root = cfg
            .developer_image_root
            .as_deref()
            .ok_or("developer_image_required")?;
        set_phase("preparing_developer_image");
        let chip = lockdown
            .get_value(Some("UniqueChipID"), None)
            .await
            .map_err(|_| "device_identity_required")?
            .as_unsigned_integer()
            .ok_or("device_identity_required")?;
        tokio::time::timeout(
            Duration::from_secs(180),
            crate::image::mount(&mut adapter, &mut handshake, root, chip),
        )
        .await
        .map_err(|_| "developer_image_timeout")??;
        let stream = adapter
            .connect(rsd_port)
            .await
            .map_err(|_| "rsd_refresh_failed")?;
        handshake = RsdHandshake::new(stream)
            .await
            .map_err(|_| "rsd_refresh_failed")?;
        developer_services = handshake
            .services
            .contains_key("com.apple.dt.testmanagerd.remote")
            && handshake
                .services
                .contains_key("com.apple.instruments.dtservicehub");
        update(|host| host.status.developer_services = developer_services);
        if !developer_services {
            return Err("developer_image_services_missing");
        }
    }

    #[cfg(target_os = "ios")]
    if tokio::time::timeout(
        Duration::from_secs(1),
        TcpStream::connect(("127.0.0.1", 8100)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
    {
        return Err("external_automation_active");
    }
    set_phase("resolving_runner");
    let mut installation = InstallationProxyClient::connect_rsd(&mut adapter, &mut handshake)
        .await
        .map_err(|_| "installation_proxy_failed")?;
    let bundle = cfg
        .runner_bundle
        .as_deref()
        .ok_or("runner_bundle_required")?;
    let mut test = TestConfig::from_installation_proxy(&mut installation, bundle, None)
        .await
        .map_err(|_| "runner_not_installed")?;
    drop(installation);
    drop(lockdown);
    let mut env = plist::Dictionary::new();
    for (key, value) in [
        ("IPU_RUNNER_TOKEN", &cfg.runner_token),
        ("IPU_DAEMON_TOKEN", &cfg.daemon_token),
        ("IPU_DAEMON_PASSWORD", &cfg.daemon_password),
    ] {
        env.insert(
            key.into(),
            plist::Value::String(value.clone().ok_or("runner_credentials_required")?),
        );
    }
    for (key, value) in [
        ("IPU_RUNNER_PORT", "18100"),
        ("IPU_RUNNER_MJPEG_PORT", "19100"),
        ("IPU_DAEMON_PORT", "18787"),
    ] {
        env.insert(key.into(), plist::Value::String(value.into()));
    }
    env.insert(
        "IPU_DAEMON_HOST".into(),
        plist::Value::String("0.0.0.0".into()),
    );
    test.runner_env = Some(env);
    set_phase("starting_runner");
    XCUITestService::run_rsd(adapter, handshake, major, test, &mut Events, None)
        .await
        .map_err(|_| "xctest_failed")?;
    set_phase("runner_exited");
    Ok(())
}

/// Takes an in-memory config, never emits it or enables upstream trace logging.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_host_start(input: *const c_char) -> i32 {
    if input.is_null() {
        return -1;
    }
    let Ok(raw) = (unsafe { CStr::from_ptr(input) }).to_str() else {
        return -1;
    };
    if raw.len() > 262_144 {
        return -1;
    }
    let Ok(cfg) = serde_json::from_str::<Config>(raw) else {
        return -1;
    };
    if !["pair", "probe", "launch"].contains(&cfg.mode.as_str())
        || cfg.device_port == 0
        || cfg.device_host.is_empty()
    {
        return -1;
    }
    if cfg.bridge_port.is_some()
        && (!valid_token(cfg.bridge_token.as_deref()) || cfg.bridge_port == Some(0))
    {
        return -1;
    }
    if cfg.mode == "launch"
        && (!valid_token(cfg.runner_token.as_deref())
            || !valid_token(cfg.daemon_token.as_deref())
            || cfg.daemon_password.as_deref().is_none_or(str::is_empty))
    {
        return -1;
    }
    let mut host = match host().lock() {
        Ok(h) => h,
        Err(_) => return -1,
    };
    if host.task.as_ref().is_some_and(|task| !task.is_finished()) {
        return 1;
    }
    if RUNTIME.get().is_none() {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return -1;
        };
        let _ = RUNTIME.set(runtime);
    }
    host.generation = host.generation.wrapping_add(1);
    let generation = host.generation;
    host.status = Status {
        phase: "starting".into(),
        ..Status::default()
    };
    host.record = None;
    host.task = Some(RUNTIME.get().unwrap().spawn(SESSION_GENERATION.scope(
        generation,
        async move {
            match AssertUnwindSafe(session(cfg)).catch_unwind().await {
                Ok(Err(code)) => fail(code),
                Err(_) => fail("host_panicked"),
                Ok(Ok(())) => {}
            }
        },
    )));
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn ipu_host_stop() {
    if let Ok(mut host) = host().lock() {
        host.generation = host.generation.wrapping_add(1);
        if let Some(task) = host.task.take() {
            task.abort();
        }
        host.status.phase = "stopped".into();
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn ipu_host_status() -> *mut c_char {
    let status = host().lock().map(|h| h.status.clone()).unwrap_or_default();
    CString::new(serde_json::to_string(&status).unwrap_or_else(|_| "{}".into()))
        .unwrap()
        .into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_host_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
/// Returns required capacity when out is NULL; copies only into the caller's private buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ipu_host_copy_pair_record(out: *mut u8, capacity: usize) -> isize {
    let Ok(host) = host().lock() else {
        return -1;
    };
    let Some(bytes) = &host.record else {
        return 0;
    };
    if out.is_null() {
        return bytes.len() as isize;
    }
    if capacity < bytes.len() {
        return -1;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len()) };
    bytes.len() as isize
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_status_never_serializes_credentials() {
        let status = Status::default();
        let json = serde_json::to_value(status).unwrap();
        let public_fields = [
            "phase",
            "build_id",
            "error_code",
            "protocol_error_code",
            "protocol_error_subcode",
            "pair_record_available",
            "ios",
            "developer_services",
            "test_plan_started",
        ];
        assert!(
            json.as_object()
                .unwrap()
                .keys()
                .all(|key| public_fields.contains(&key.as_str()))
        );
        for key in [
            "pair_record_base64",
            "runner_token",
            "daemon_token",
            "daemon_password",
            "bridge_token",
        ] {
            assert!(json.get(key).is_none());
        }
    }
    #[test]
    fn abi_rejects_invalid_configs_before_connecting() {
        let value = CString::new(r#"{"mode":"launch","device_host":"127.0.0.1","runner_token":"weak","daemon_token":"weak","daemon_password":"synthetic"}"#).unwrap();
        assert_eq!(unsafe { ipu_host_start(value.as_ptr()) }, -1);
        assert_eq!(unsafe { ipu_host_start(std::ptr::null()) }, -1);
    }
    #[test]
    fn token_validation_rejects_weak_credentials() {
        assert!(!valid_token(None));
        assert!(!valid_token(Some("password")));
        assert!(!valid_token(Some(&"z".repeat(64))));
        assert!(valid_token(Some(&"a".repeat(64))));
    }
}
