//! Protocol sequence from idevice tools/src/rppairing.rs at rev 41c388b.
use idevice::provider::RsdProvider;
use idevice::{
    IdeviceService, RemoteXpcClient,
    core_device_proxy::CoreDeviceProxy,
    remote_pairing::{RemotePairingClient, RpPairingFile},
    rsd::RsdHandshake,
};
use self_boot::{Error, Result};
use std::{
    io::{BufRead, Write},
    path::Path,
    time::Duration,
};

fn save_new(path: &Path, record: &RpPairingFile) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(&record.to_bytes())?;
        file.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result?;
    Ok(())
}
async fn pin() -> String {
    // idevice slices the first six bytes: never hand it a short/non-ASCII PIN.
    tokio::task::spawn_blocking(|| {
        loop {
            eprint!("Enter the six-digit pairing PIN shown on the phone (unlock and tap Trust if prompted): ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
                eprintln!("No PIN input; cancelling pairing.");
                std::process::exit(1);
            }
            let value = line.trim();
            if value.len() == 6 && value.bytes().all(|c| c.is_ascii_digit()) { return value.to_owned(); }
            eprintln!("PIN must contain exactly six ASCII digits.");
        }
    }).await.expect("PIN reader panicked")
}
pub async fn pair(udid: &str, name: &str, out: &Path) -> Result<()> {
    if out.try_exists()? {
        return Err(Error::InvalidArgument(
            "output exists; choose a new file".into(),
        ));
    }
    eprintln!(
        "Unlock the USB phone. Accept Trust and enter the device passcode if requested. Pairing deadline: 180 s."
    );
    tokio::time::timeout(Duration::from_secs(180), async {
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
        let provider = device.to_provider(addr, "self-boot-rp-pair");
        let proxy = CoreDeviceProxy::connect(&provider).await?;
        let port = proxy.tunnel_info().server_rsd_port;
        let mut adapter = proxy.create_software_tunnel()?.to_async_handle();
        let result: Result<()> = async {
            let rsd = RsdHandshake::new(adapter.connect_to_service_port(port).await?).await?;
            let service = rsd
                .services
                .get("com.apple.internal.dt.coredevice.untrusted.tunnelservice")
                .ok_or_else(|| Error::Protocol("untrusted tunnel service missing".into()))?;
            let mut xpc =
                RemoteXpcClient::new(adapter.connect_to_service_port(service.port).await?).await?;
            xpc.do_handshake().await?;
            let _ = xpc.recv_root().await?;
            let mut record = RpPairingFile::generate(name);
            let mut client = RemotePairingClient::new(xpc, name);
            // iOS consent normally supplies 000000 internally; the callback is for a real PIN challenge.
            client.connect(&mut record, pin).await?;
            save_new(out, &record)?;
            super::output(serde_json::json!({"step":"rp_pair","out":out,"mode":"0600"}));
            Ok(())
        }
        .await;
        let closed = adapter.close().await;
        result?;
        closed?;
        Ok(())
    })
    .await
    .map_err(|_| Error::Timeout("USB RemotePairing (Trust/PIN)"))?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_is_private_roundtrips_and_refuses_overwrite() {
        let path =
            std::env::temp_dir().join(format!("self-boot-pair-{}.plist", uuid::Uuid::new_v4()));
        let record = RpPairingFile::generate("offline-test");
        save_new(&path, &record).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            RpPairingFile::from_bytes(&bytes).unwrap().identifier,
            record.identifier
        );
        assert!(save_new(&path, &RpPairingFile::generate("other")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }
}
