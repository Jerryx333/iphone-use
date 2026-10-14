use crate::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone)]
pub struct Config {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub psk: Vec<u8>,
    pub device_id: String,
    pub token: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub v: u8,
    pub host: String,
    pub port: u16,
    pub cert_sha256: String,
    pub psk: String,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
}
pub fn home() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("PHONE_GATEWAY_HOME") {
        return Ok(p.into());
    }
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME is missing")?)
            .join(".config/phone-gateway"),
    )
}
pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.is_ascii() || !s.len().is_multiple_of(2) {
        return Err("invalid hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(&s[i..i + 2], 16)?))
        .collect()
}
pub fn fingerprint(der: &[u8]) -> String {
    hex(&Sha256::digest(der))
}
// Only the two PEM types emitted by `openssl req -newkey ec` are accepted.
pub fn pem(text: &str, label: &str) -> Result<Vec<u8>> {
    let start = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body = text
        .split_once(&start)
        .ok_or("missing PEM start")?
        .1
        .split_once(&end)
        .ok_or("missing PEM end")?
        .0;
    Ok(STANDARD.decode(
        body.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    )?)
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    // Write into a new 0600 inode, then rename: existing symlinks are never followed.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = parent.join(format!(".gateway-{}", hex(&random::<16>()?)));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let c = Self {
            cert_der: pem(&fs::read_to_string(dir.join("server.crt"))?, "CERTIFICATE")?,
            key_der: pem(&fs::read_to_string(dir.join("server.key"))?, "PRIVATE KEY")?,
            psk: STANDARD.decode(fs::read_to_string(dir.join("psk"))?.trim())?,
            device_id: fs::read_to_string(dir.join("device_id"))?.trim().into(),
            token: fs::read_to_string(dir.join("client-token"))?.trim().into(),
        };
        if c.psk.len() != 32
            || unhex(&c.device_id)?.len() != 16
            || STANDARD.decode(&c.token)?.len() != 32
        {
            return Err("invalid configuration lengths".into());
        }
        Ok(c)
    }
    pub fn pairing(&self, host: String, port: u16) -> Pairing {
        Pairing {
            ws_url: None,
            v: 1,
            host,
            port,
            cert_sha256: fingerprint(&self.cert_der),
            psk: STANDARD.encode(&self.psk),
            device_id: self.device_id.clone(),
        }
    }
}
pub fn pair(dir: &Path, rotate: bool, host: String, port: u16, out: &Path) -> Result<Pairing> {
    if fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("configuration directory is a symlink".into());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let names = [
        "server.crt",
        "server.key",
        "psk",
        "device_id",
        "client-token",
    ];
    let count = names.iter().filter(|n| dir.join(n).exists()).count();
    if !rotate && count != 0 && count != names.len() {
        return Err("incomplete configuration; use --rotate".into());
    }
    if rotate || count == 0 {
        let staging = dir.join(format!(".pair-{}", hex(&random::<16>()?)));
        fs::DirBuilder::new().mode(0o700).create(&staging)?;
        let result = (|| -> Result<()> {
            for n in ["server.crt", "server.key"] {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(staging.join(n))?;
            }
            let status = Command::new("/usr/bin/openssl")
                .args([
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    // LibreSSL otherwise writes explicit curve parameters, which rustls rejects.
                    "-pkeyopt",
                    "ec_param_enc:named_curve",
                    "-nodes",
                    "-days",
                    "3650",
                    "-subj",
                    "/CN=phone-gateway",
                    // Extensions make LibreSSL emit an X.509 v3 certificate; rustls rejects v1.
                    "-addext",
                    "basicConstraints=critical,CA:FALSE",
                    "-addext",
                    "subjectAltName=DNS:phone-gateway",
                    "-keyout",
                ])
                .arg(staging.join("server.key"))
                .arg("-out")
                .arg(staging.join("server.crt"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err("openssl certificate generation failed".into());
            }
            private_write(
                &staging.join("psk"),
                STANDARD.encode(random::<32>()?).as_bytes(),
            )?;
            private_write(&staging.join("device_id"), hex(&random::<16>()?).as_bytes())?;
            private_write(
                &staging.join("client-token"),
                STANDARD.encode(random::<32>()?).as_bytes(),
            )?;
            Config::load(&staging)?;
            for n in names {
                fs::rename(staging.join(n), dir.join(n))?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(staging);
        result?;
    }
    for n in names {
        if fs::symlink_metadata(dir.join(n))?.file_type().is_symlink() {
            return Err("configuration file is a symlink".into());
        }
        fs::set_permissions(dir.join(n), fs::Permissions::from_mode(0o600))?;
    }
    let pairing = Config::load(dir)?.pairing(host, port);
    private_write(out, &serde_json::to_vec(&pairing)?)?;
    Ok(pairing)
}

/// Validate the deliberately narrow pairing URL grammar without URL normalization.
pub fn validate_ws_url(url: &str) -> Result<String> {
    if url.len() > 512 {
        return Err("invalid_pairing: ws_url".into());
    }
    let (secure, authority) = if let Some(a) = url.strip_prefix("wss://") {
        (true, a)
    } else if let Some(a) = url.strip_prefix("ws://") {
        (false, a)
    } else {
        return Err("invalid_pairing: ws_url".into());
    };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(h, p)| (h, Some(p)));
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || (!secure && !matches!(host, "127.0.0.1" | "localhost"))
        || port.is_some_and(|p| {
            p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u16>().is_err()
        })
    {
        return Err("invalid_pairing: ws_url".into());
    }
    Ok(format!(
        "{}://{authority}",
        if secure { "wss" } else { "ws" }
    ))
}
