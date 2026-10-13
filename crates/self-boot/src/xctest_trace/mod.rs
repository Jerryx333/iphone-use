//! USB XCTest reference session with complete DTX JSONL recording; no HTTP requests.
mod orchestration;
mod process_control;
mod remote_server;

use base64::{Engine, engine::general_purpose::STANDARD};
use idevice::{
    IdeviceService,
    dvt::message::{AuxValue, Message},
    installation_proxy::InstallationProxyClient,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

tokio::task_local! { static RECORDER: Arc<Recorder>; }
#[derive(Debug)]
struct Recorder {
    start: Instant,
    state: Mutex<Recording>,
}
struct Recording {
    out: Box<dyn Write + Send>,
    channels: HashMap<(String, u32), String>,
    error: Option<String>,
}
impl std::fmt::Debug for Recording {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recording").finish_non_exhaustive()
    }
}
fn blob(bytes: &[u8]) -> Value {
    let end = bytes.len().min(1024);
    json!({"base64": STANDARD.encode(&bytes[..end]), "byte_length": bytes.len(), "truncated": end < bytes.len()})
}
fn archived(bytes: &[u8]) -> Value {
    crate::archive::decode_json(bytes).unwrap_or_else(|_| blob(bytes))
}
fn aux(value: &AuxValue) -> Value {
    match value {
        AuxValue::Null => Value::Null,
        AuxValue::String(s) => json!(s),
        AuxValue::U32(n) => json!(n),
        AuxValue::I64(n) => json!(n),
        AuxValue::Double(n) => json!(n),
        AuxValue::Array(bytes) => archived(bytes),
        AuxValue::PrimitiveDictionary(entries) => {
            json!({"primitive_dictionary": entries.iter().map(|(key, values)| json!({"key": aux(key), "values": values.iter().map(aux).collect::<Vec<_>>()})).collect::<Vec<_>>()})
        }
    }
}
// idevice keeps these header accessors crate-private; serialization is public.
trait HeaderFields {
    fn identifier(&self) -> u32;
    fn conversation_index(&self) -> u32;
    fn expects_reply(&self) -> bool;
}
impl HeaderFields for idevice::dvt::message::MessageHeader {
    fn identifier(&self) -> u32 {
        u32::from_le_bytes(self.serialize()[16..20].try_into().unwrap())
    }
    fn conversation_index(&self) -> u32 {
        u32::from_le_bytes(self.serialize()[20..24].try_into().unwrap())
    }
    fn expects_reply(&self) -> bool {
        self.serialize()[28] != 0
    }
}
impl Recorder {
    fn record(
        &self,
        connection: &str,
        direction: &str,
        msg: &Message,
    ) -> Result<(), idevice::IdeviceError> {
        let mut state = self.state.lock().unwrap();
        let values = msg
            .aux
            .as_ref()
            .map(|a| a.values.iter().map(aux).collect::<Vec<_>>())
            .unwrap_or_default();
        let payload = msg
            .raw_data
            .as_ref()
            .map(|b| archived(b))
            .unwrap_or(Value::Null);
        let selector = msg.data.as_ref().and_then(plist::Value::as_string);
        if selector == Some("_requestChannelWithCode:identifier:")
            && let (Some(code), Some(name)) = (
                values.first().and_then(Value::as_u64),
                values.get(1).and_then(Value::as_str),
            )
        {
            state
                .channels
                .insert((connection.into(), code as u32), name.into());
        }
        let channel = msg.message_header.channel;
        let name = if channel == 0 {
            Some("root")
        } else {
            state
                .channels
                .get(&(connection.into(), channel.unsigned_abs()))
                .map(String::as_str)
        };
        let role = if connection.contains("ctrl") {
            "control"
        } else if connection.contains("main") {
            "test"
        } else {
            "other"
        };
        let record = json!({"timestamp_unix_us": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros(),
            "elapsed_us": self.start.elapsed().as_micros(), "direction": direction, "connection": connection,
            "connection_role": role, "channel": channel, "channel_name": name,
            "identifier": msg.message_header.identifier(), "conversation_index": msg.message_header.conversation_index(),
            "expects_reply": msg.message_header.expects_reply(), "message_type": msg.payload_header.serialize()[0],
            "selector": selector, "aux": values, "payload": payload});
        let result = writeln!(state.out, "{record}").and_then(|_| state.out.flush());
        if let Err(e) = &result {
            state.error = Some(e.to_string());
        }
        result.map_err(Into::into)
    }
}

/// Run the installed formal runner through USB → CoreDeviceProxy → RSD.
/// `None` output writes JSONL to stdout; diagnostics use stderr.
pub async fn run(
    udid: &str,
    bundle: Option<&str>,
    observe: Duration,
    out: Option<&Path>,
) -> crate::Result<()> {
    let writer: Box<dyn Write + Send> = match out {
        Some(path) => Box::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)?,
        ),
        None => Box::new(std::io::stdout()),
    };
    let recorder = Arc::new(Recorder {
        start: Instant::now(),
        state: Mutex::new(Recording {
            out: writer,
            channels: HashMap::new(),
            error: None,
        }),
    });
    let addr = idevice::usbmuxd::UsbmuxdAddr::from_env_var()
        .map_err(|e| crate::Error::InvalidArgument(e.to_string()))?;
    let mut mux = addr.connect(0).await?;
    let device = mux
        .get_devices()
        .await?
        .into_iter()
        .find(|d| d.udid == udid && matches!(d.connection_type, idevice::usbmuxd::Connection::Usb))
        .ok_or_else(|| crate::Error::Protocol(format!("USB device not found: {udid}")))?;
    let provider = device.to_provider(addr, "self-boot-xctest-trace");
    let mut proxy = InstallationProxyClient::connect(&provider).await?;
    let apps = proxy.get_apps(None, None).await?;
    let id = select_bundle(apps.keys().map(String::as_str), bundle)?;
    let cfg = orchestration::TestConfig::from_installation_proxy(&mut proxy, &id, None).await?;
    drop(proxy);
    eprintln!(
        "xctest-trace runner={id}; observe={}s after launch; no HTTP requests",
        observe.as_secs()
    );
    struct Listener;
    impl idevice::dvt::xctest::listener::XCUITestListener for Listener {}
    let service = orchestration::XCUITestService::new(Arc::new(provider));
    let result = RECORDER
        .scope(recorder.clone(), service.run(cfg, &mut Listener, observe))
        .await;
    if let Some(error) = &recorder.state.lock().unwrap().error {
        return Err(crate::Error::Protocol(format!("trace output: {error}")));
    }
    result?;
    Ok(())
}
fn select_bundle<'a>(
    ids: impl Iterator<Item = &'a str>,
    requested: Option<&str>,
) -> crate::Result<String> {
    let mut found: Vec<_> = ids
        .filter(|id| {
            requested.map_or_else(
                || id.contains("com.leeguoo.iphone-use.runner") && id.ends_with(".xctrunner"),
                |wanted| *id == wanted,
            )
        })
        .collect();
    found.sort_unstable();
    match found.as_slice() {
        [id] => Ok((*id).into()),
        [] => Err(crate::Error::Protocol(
            "installed runner not found; specify --bundle with installation_proxy bundle id".into(),
        )),
        _ => Err(crate::Error::Protocol(format!(
            "multiple installed runners: {}; specify --bundle",
            found.join(", ")
        ))),
    }
}

// Preserve undecodable payloads instead of losing the message in idevice's strict decoder.
async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Message, idevice::IdeviceError> {
    use tokio::io::AsyncReadExt;
    let invalid = || idevice::IdeviceError::UnexpectedResponse("invalid DTX frame".into());
    let mut packet = Vec::new();
    let mut previous = None;
    let mut header = loop {
        let mut header = [0u8; 32];
        reader.read_exact(&mut header).await?;
        let at = |n| u32::from_le_bytes(header[n..n + 4].try_into().unwrap());
        let fragment = u16::from_le_bytes(header[8..10].try_into().unwrap());
        let count = u16::from_le_bytes(header[10..12].try_into().unwrap());
        let length = at(12) as usize;
        if at(0) != 0x1f3d5b79
            || at(4) != 32
            || count == 0
            || fragment >= count
            || previous.map_or(fragment != 0, |p| fragment != p + 1)
            || packet.len().saturating_add(length) > 64 * 1024 * 1024
        {
            return Err(invalid());
        }
        previous = Some(fragment);
        // Fragment zero of a fragmented message contains only the header (idevice framing).
        if count > 1 && fragment == 0 {
            continue;
        }
        let offset = packet.len();
        packet.resize(offset + length, 0);
        reader.read_exact(&mut packet[offset..]).await?;
        if fragment == count - 1 {
            break header;
        }
    };
    if packet.len() < 16 {
        return Err(invalid());
    }
    let aux_len = u32::from_le_bytes(packet[4..8].try_into().unwrap()) as usize;
    let total = u32::from_le_bytes(packet[8..12].try_into().unwrap()) as usize;
    if total < aux_len || total.checked_add(16) != Some(packet.len()) {
        return Err(invalid());
    }
    let raw = packet[16 + aux_len..].to_vec();
    let decoded_aux = if aux_len == 0 {
        None
    } else {
        let bytes = packet[16..16 + aux_len].to_vec();
        Some(
            idevice::dvt::message::Aux::from_bytes(bytes.clone()).unwrap_or_else(|_| {
                idevice::dvt::message::Aux::from_values(vec![AuxValue::Array(bytes)])
            }),
        )
    };
    packet.truncate(16);
    packet[4..12].fill(0);
    header[8..12].copy_from_slice(&[0, 0, 1, 0]);
    header[12..16].copy_from_slice(&16u32.to_le_bytes());
    let mut stripped = header.to_vec();
    stripped.extend(packet);
    let mut message = Message::from_reader(&mut stripped.as_slice()).await?;
    message.aux = decoded_aux;
    if !raw.is_empty() {
        message.data = ns_keyed_archive::decode::from_bytes(&raw).ok();
        message.raw_data = Some(raw);
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use idevice::dvt::{
        message::{Aux, MessageHeader, PayloadHeader},
        xctest::TestConfig,
    };
    #[derive(Clone)]
    struct Output(Arc<Mutex<Vec<u8>>>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn jsonl_records_both_directions_channels_and_configuration_reply() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            start: Instant::now(),
            state: Mutex::new(Recording {
                out: Box::new(Output(bytes.clone())),
                channels: HashMap::new(),
                error: None,
            }),
        };
        let request = Message::new(
            MessageHeader::new(0, 1, 42, 0, 0, true),
            PayloadHeader::method_invocation(),
            Some(Aux::from_values(vec![
                AuxValue::U32(7),
                AuxValue::archived_value("XCTestDriverInterface"),
            ])),
            Some("_requestChannelWithCode:identifier:".into()),
        );
        recorder
            .record("testmanagerd-main", "receive", &request)
            .unwrap();
        let cfg = TestConfig {
            runner_bundle_id: "com.leeguoo.iphone-use.runner.xctrunner".into(),
            runner_app_path: "/Apps/Runner.app".into(),
            runner_app_container: "/Container".into(),
            runner_bundle_executable: "Runner-Runner".into(),
            target_bundle_id: None,
            target_app_path: None,
            target_app_env: None,
            target_app_args: None,
            tests_to_run: None,
            tests_to_skip: None,
            runner_env: None,
            runner_args: None,
        };
        let archive = cfg
            .build_xctest_configuration(uuid::Uuid::nil(), 27)
            .unwrap()
            .to_archive_bytes()
            .unwrap();
        let frame = remote_server::build_raw_reply(7, 99, 0, &archive);
        let reply = read_message(&mut frame.as_slice()).await.unwrap();
        recorder
            .record("testmanagerd-main", "send", &reply)
            .unwrap();
        let lines: Vec<Value> = String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["selector"], "_requestChannelWithCode:identifier:");
        assert_eq!(lines[0]["expects_reply"], true);
        assert_eq!(lines[1]["channel_name"], "XCTestDriverInterface");
        assert_eq!(lines[1]["connection_role"], "test");
        assert_eq!(lines[1]["direction"], "send");
        assert_eq!(lines[1]["identifier"], 99);
        assert_eq!(lines[1]["conversation_index"], 1);
        assert_eq!(
            lines[1]["payload"]["testBundleURL"]["NS.relative"],
            "file:///Apps/Runner.app/PlugIns/Runner.xctest"
        );
        assert!(lines[1]["timestamp_unix_us"].as_u64().unwrap() > 0);
        assert!(lines[1]["payload"].get("base64").is_none());
    }
    #[tokio::test]
    async fn unknown_payload_is_preserved_and_truncated() {
        let raw = vec![0xff; 2048];
        let frame = remote_server::build_raw_reply(1, 3, 0, &raw);
        let msg = read_message(&mut frame.as_slice()).await.unwrap();
        assert!(msg.data.is_none());
        let fallback = archived(msg.raw_data.as_ref().unwrap());
        assert_eq!(fallback["truncated"], true);
        assert_eq!(fallback["byte_length"], 2048);
        assert_eq!(
            STANDARD
                .decode(fallback["base64"].as_str().unwrap())
                .unwrap()
                .len(),
            1024
        );
    }
    #[tokio::test]
    async fn fragmented_payload_and_unknown_aux_survive() {
        let message = Message::new(
            MessageHeader::new(0, 1, 10, 0, 2, true),
            PayloadHeader::method_invocation(),
            None,
            Some("callback:".into()),
        );
        let frame = message.serialize();
        let body = &frame[32..];
        let mut fragments = Vec::new();
        for (index, payload) in [None, Some(&body[..8]), Some(&body[8..])]
            .into_iter()
            .enumerate()
        {
            let mut h = frame[..32].to_vec();
            h[8..10].copy_from_slice(&(index as u16).to_le_bytes());
            h[10..12].copy_from_slice(&3u16.to_le_bytes());
            h[12..16].copy_from_slice(&(payload.map_or(0, |p| p.len()) as u32).to_le_bytes());
            fragments.extend(h);
            if let Some(payload) = payload {
                fragments.extend(payload);
            }
        }
        let parsed = read_message(&mut fragments.as_slice()).await.unwrap();
        assert_eq!(
            parsed.data.as_ref().and_then(plist::Value::as_string),
            Some("callback:")
        );
        assert_eq!(parsed.message_header.identifier(), 10);
        let mut invalid_aux = remote_server::build_raw_reply(1, 11, 0, &[0xff; 4]);
        invalid_aux[36..40].copy_from_slice(&4u32.to_le_bytes());
        let parsed = read_message(&mut invalid_aux.as_slice()).await.unwrap();
        assert_eq!(aux(&parsed.aux.unwrap().values[0])["byte_length"], 4);
    }
    #[test]
    fn installed_bundle_selection_is_unambiguous() {
        let id = "com.leeguoo.iphone-use.runner.xctrunner";
        assert_eq!(
            select_bundle(["unrelated", id].into_iter(), None).unwrap(),
            id
        );
        assert!(
            select_bundle(
                [id, "x.com.leeguoo.iphone-use.runner.xctrunner"].into_iter(),
                None
            )
            .is_err()
        );
        assert!(select_bundle([id].into_iter(), Some("absent")).is_err());
        assert_eq!(
            select_bundle(["custom"].into_iter(), Some("custom")).unwrap(),
            "custom"
        );
    }
}
