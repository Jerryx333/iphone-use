//! Offline test client. It authenticates the TLS signature as well as the pinned leaf.
use crate::{
    config::{fingerprint, Pairing},
    protocol, Result,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

#[derive(Debug)]
struct Pin {
    hash: String,
}
impl ServerCertVerifier for Pin {
    /// Swift equivalent: take ONLY the leaf SecCertificate DER bytes
    /// (`SecCertificateCopyData`), SHA-256 all bytes, compare the 32-byte digest
    /// with decoded `cert_sha256`. No PEM hashing, SPKI hashing, CA or hostname
    /// fallback. Return false from the Network.framework verify block on mismatch.
    fn verify_server_cert(
        &self,
        leaf: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if protocol::token_matches(&self.hash, &fingerprint(leaf.as_ref())) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("leaf pin mismatch".into()))
        }
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 disabled".into()))
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            msg,
            cert,
            sig,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub async fn tls(address: &str, pin: String) -> Result<TlsStream<TcpStream>> {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(Pin { hash: pin }))
    .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config))
        .connect(
            ServerName::try_from("phone-gateway")?.to_owned(),
            TcpStream::connect(address).await?,
        )
        .await?)
}
pub fn hello(pair: &Pairing, challenge: &Value) -> Result<Value> {
    let nonce = STANDARD.decode(challenge["nonce"].as_str().ok_or("missing nonce")?)?;
    if nonce.len() != 32 {
        return Err("bad nonce".into());
    }
    Ok(
        json!({"t":"hello","v":1,"device_id":pair.device_id,"mac":STANDARD.encode(protocol::mac(&STANDARD.decode(&pair.psk)?, &nonce, &pair.device_id)),"app_version":"fake","session":{"ready":true}}),
    )
}
pub async fn connect(pair: &Pairing) -> Result<TlsStream<TcpStream>> {
    let mut stream = tls(
        &format!("{}:{}", pair.host, pair.port),
        pair.cert_sha256.clone(),
    )
    .await?;
    let challenge = protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?;
    protocol::write_frame(&mut stream, &hello(pair, &challenge)?).await?;
    if protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?["t"] != "welcome" {
        return Err("welcome missing".into());
    }
    Ok(stream)
}

pub fn tunnel_hello(pair: &Pairing, challenge: &Value) -> Result<Value> {
    let nonce = STANDARD.decode(challenge["nonce"].as_str().ok_or("missing nonce")?)?;
    if nonce.len() != 32 {
        return Err("bad nonce".into());
    }
    Ok(
        json!({"t":"hello","v":1,"role":"tunnel","device_id":pair.device_id,
        "mac":STANDARD.encode(protocol::tunnel_mac(&STANDARD.decode(&pair.psk)?, &nonce, &pair.device_id))}),
    )
}
pub async fn connect_tunnel(pair: &Pairing) -> Result<TlsStream<TcpStream>> {
    let mut stream = tls(
        &format!("{}:{}", pair.host, pair.port),
        pair.cert_sha256.clone(),
    )
    .await?;
    let challenge = protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?;
    protocol::write_frame(&mut stream, &tunnel_hello(pair, &challenge)?).await?;
    if protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?["t"] != "welcome" {
        return Err("welcome missing".into());
    }
    Ok(stream)
}
#[derive(Default)]
pub struct TunnelOptions {
    pub targets: std::collections::HashMap<u16, std::net::SocketAddr>,
    pub reject: Option<u16>,
    pub stall: bool,
}
/// Handle one open, then switch to raw bytes. Missing targets are rejected.
pub async fn tunnel(mut stream: TlsStream<TcpStream>, options: TunnelOptions) -> Result<()> {
    let frame = protocol::read_frame(&mut stream, protocol::PRE_AUTH_LIMIT).await?;
    if frame["t"] != "open" {
        return Err("open missing".into());
    }
    if options.stall {
        // Keep the connection alive until the gateway closes it.
        let mut byte = [0];
        use tokio::io::AsyncReadExt;
        let _ = stream.read(&mut byte).await;
        return Ok(());
    }
    let port = frame["port"].as_u64().and_then(|p| u16::try_from(p).ok());
    let target = match port {
        Some(port) if options.reject != Some(port) => options.targets.get(&port),
        _ => None,
    };
    let mut target = match target {
        Some(address) => TcpStream::connect(address).await.ok(),
        None => None,
    };
    protocol::write_frame(&mut stream, &json!({"t":"opened","ok":target.is_some()})).await?;
    if let Some(target) = target.as_mut() {
        tokio::io::copy_bidirectional(&mut stream, target).await?;
    }
    Ok(())
}
