use crate::{config::random, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PRE_AUTH_LIMIT: usize = 4096;
pub const FRAME_LIMIT: usize = 16 * 1024 * 1024;
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, limit: usize) -> Result<Value> {
    let len = r.read_u32().await? as usize;
    if len == 0 || len > limit {
        return Err("frame length rejected".into());
    }
    let mut bytes = vec![0; len];
    r.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > FRAME_LIMIT {
        return Err("frame too large".into());
    }
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}
pub fn mac(psk: &[u8], challenge: &[u8], device_id: &str) -> Vec<u8> {
    let mut h = Hmac::<Sha256>::new_from_slice(psk).expect("HMAC accepts any key length");
    h.update(b"selfrunner-v1");
    h.update(challenge);
    h.update(device_id.as_bytes());
    h.finalize().into_bytes().to_vec()
}
pub fn token_matches(expected: &str, supplied: &str) -> bool {
    // HMAC verification compares fixed-size tags in constant time, even for unequal input lengths.
    let mut h = Hmac::<Sha256>::new_from_slice(b"phone-gateway-token-comparison").unwrap();
    h.update(expected.as_bytes());
    let tag = h.finalize().into_bytes();
    let mut h = Hmac::<Sha256>::new_from_slice(b"phone-gateway-token-comparison").unwrap();
    h.update(supplied.as_bytes());
    h.verify_slice(&tag).is_ok()
}
pub struct Challenge {
    nonce: [u8; 32],
    consumed: bool,
}
impl Challenge {
    pub fn new() -> Result<Self> {
        Ok(Self {
            nonce: random()?,
            consumed: false,
        })
    }
    pub fn frame(&self) -> Value {
        json!({"t":"challenge", "nonce":STANDARD.encode(self.nonce)})
    }
    pub fn verify(&mut self, hello: &Value, psk: &[u8], device: &str) -> bool {
        if std::mem::replace(&mut self.consumed, true) {
            return false;
        }
        if hello["t"] != "hello" || hello["v"] != 1 || hello["device_id"] != device {
            return false;
        }
        let Some(encoded) = hello["mac"].as_str() else {
            return false;
        };
        let Ok(tag) = STANDARD.decode(encoded) else {
            return false;
        };
        let mut h = Hmac::<Sha256>::new_from_slice(psk).unwrap();
        h.update(b"selfrunner-v1");
        h.update(&self.nonce);
        h.update(device.as_bytes());
        h.verify_slice(&tag).is_ok()
    }
}
