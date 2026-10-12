use std::{
    ffi::{CStr, CString},
    path::Path,
    time::{Duration, Instant},
};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 5 && args[1] == "--usb-pair" && args[3] == "--pair-out" {
        #[cfg(feature = "usb-provision")]
        provision_usb(&args[2], Path::new(&args[4]));
        #[cfg(not(feature = "usb-provision"))]
        {
            eprintln!("Build with --features usb-provision for initial USB pairing");
            std::process::exit(2);
        }
    }
    if args.len() < 3 || args[1] != "--config" {
        eprintln!("usage: ipu-phone-host --config PRIVATE_JSON [--pair-out PRIVATE_PLIST]");
        std::process::exit(2);
    }
    let path = Path::new(&args[2]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path).unwrap().permissions().mode() & 0o077 != 0 {
            eprintln!("Config must be private (chmod 600)");
            std::process::exit(2);
        }
    }
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    if let Some(path) = config.get("pair_record_path").and_then(|x| x.as_str()) {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let bytes = std::fs::read(path).unwrap();
        config["pair_record_base64"] = STANDARD.encode(bytes).into();
    }
    let config = CString::new(config.to_string()).unwrap();
    if unsafe { ipu_phone_host::ipu_host_start(config.as_ptr()) } != 0 {
        eprintln!("Invalid config or host already active");
        std::process::exit(2);
    }
    let started = Instant::now();
    let mut previous = String::new();
    loop {
        let ptr = ipu_phone_host::ipu_host_status();
        let status = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe {
            ipu_phone_host::ipu_host_string_free(ptr);
        }
        if status != previous {
            println!("{status}");
            previous = status.clone();
        }
        let value: serde_json::Value = serde_json::from_str(&status).unwrap();
        let phase = value["phase"].as_str().unwrap_or("");
        if value["pair_record_available"] == true && args.len() == 5 && args[3] == "--pair-out" {
            let length =
                unsafe { ipu_phone_host::ipu_host_copy_pair_record(std::ptr::null_mut(), 0) };
            if length > 0 && !Path::new(&args[4]).exists() {
                let mut record = vec![0u8; length as usize];
                unsafe {
                    ipu_phone_host::ipu_host_copy_pair_record(record.as_mut_ptr(), record.len());
                }
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                use std::io::Write;
                opts.open(&args[4]).unwrap().write_all(&record).unwrap();
                use zeroize::Zeroize;
                record.zeroize();
            }
        }
        if phase == "rsd_verified" || phase == "runner_exited" {
            return;
        }
        if phase == "failed" || started.elapsed() > Duration::from_secs(180) && phase != "running" {
            ipu_phone_host::ipu_host_stop();
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(feature = "usb-provision")]
fn provision_usb(udid: &str, output: &Path) -> ! {
    use idevice::{
        IdeviceError, IdeviceService,
        remote_pairing::{RemotePairingLockdownService, RpPairingFile},
        usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let result = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let mut mux = UsbmuxdConnection::default().await?;
            let device = mux
                .get_devices()
                .await?
                .into_iter()
                .find(|d| d.udid == udid && d.connection_type == Connection::Usb)
                .ok_or(IdeviceError::DeviceNotFound)?;
            let provider = device.to_provider(UsbmuxdAddr::default(), "PhoneUseSelfHost");
            println!("{{\"phase\":\"connecting_usb_pairing_service\"}}");
            let service = RemotePairingLockdownService::connect(&provider).await?;
            let mut rpc = service.into_client("PhoneUseSelfHost")?;
            let mut record = RpPairingFile::generate("PhoneUseSelfHost");
            println!("{{\"phase\":\"awaiting_usb_pairing\"}}");
            rpc.connect(&mut record, || async { "000000".to_string() })
                .await?;
            Ok::<_, IdeviceError>(record.to_bytes())
        })
        .await
    });
    match result {
        Ok(Ok(bytes)) => {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let bytes = zeroize::Zeroizing::new(bytes);
            let saved = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)
                .and_then(|mut f| f.write_all(&bytes));
            if saved.is_err() {
                eprintln!("Private pairing record write failed");
                std::process::exit(1);
            }
            println!("{{\"phase\":\"usb_paired\",\"pair_record_available\":true}}");
            std::process::exit(0);
        }
        Ok(Err(error)) => {
            println!(
                "{{\"phase\":\"failed\",\"protocol_error_code\":{},\"protocol_error_subcode\":{}}}",
                error.code(),
                error.sub_code()
            );
        }
        Err(_) => println!("{{\"phase\":\"failed\",\"error_code\":\"usb_pairing_timeout\"}}"),
    }
    std::process::exit(1);
}
