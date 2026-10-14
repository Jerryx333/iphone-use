//! PSK authenticated, forward-secret WebSocket byte streams (design §15).
use crate::{config, gateway::Timing, Result};
use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, KeyInit};
use futures_util::{SinkExt, StreamExt};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{mpsc, oneshot, watch},
    time::timeout,
};
use tokio_tungstenite::{
    tungstenite::{protocol::WebSocketConfig, Message},
    WebSocketStream,
};
use tokio_util::sync::{CancellationToken, PollSender};
use x25519_dalek::{EphemeralSecret, PublicKey};

pub const MAX_MESSAGE: usize = 65536 + 17;
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}
pub fn route(psk: &[u8]) -> String {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(psk).unwrap();
    h.update(b"selfrunner-ws-route-v1");
    config::hex(&h.finalize().into_bytes())[..32].into()
}
type DerivedKeys = ([u8; 32], [u8; 32], [u8; 32]);
fn keys(psk: &[u8], dh: &[u8; 32], m1: &[u8], m2: &[u8]) -> Result<DerivedKeys> {
    if dh.iter().all(|b| *b == 0) {
        return Err("zero DH".into());
    }
    let th: [u8; 32] = Sha256::digest([b"selfrunner-e2e-v1".as_slice(), m1, m2].concat()).into();
    let hk = Hkdf::<Sha256>::new(Some(&th), &[psk, dh].concat());
    let mut c = [0; 32];
    let mut s = [0; 32];
    hk.expand(b"selfrunner-e2e-v1 c2s", &mut c)
        .map_err(|_| "HKDF")?;
    hk.expand(b"selfrunner-e2e-v1 s2c", &mut s)
        .map_err(|_| "HKDF")?;
    Ok((c, s, th))
}
struct Records {
    cipher: ChaCha20Poly1305,
    seq: u64,
    eof: bool,
}
impl Records {
    fn new(key: [u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new((&key).into()),
            seq: 0,
            eof: false,
        }
    }
    fn nonce(&mut self) -> Result<[u8; 12]> {
        if self.eof || self.seq == u64::MAX {
            return Err("record after EOF or exhausted sequence".into());
        }
        let mut n = [0; 12];
        n[4..].copy_from_slice(&self.seq.to_be_bytes());
        self.seq += 1;
        Ok(n)
    }
    fn encrypt(&mut self, data: Option<&[u8]>) -> Result<Vec<u8>> {
        let nonce = self.nonce()?;
        let plain = match data {
            Some(d) if !d.is_empty() && d.len() <= 65536 => [b"\0".as_slice(), d].concat(),
            None => vec![1],
            _ => return Err("invalid DATA".into()),
        };
        self.eof = data.is_none();
        self.cipher
            .encrypt((&nonce).into(), plain.as_slice())
            .map_err(|_| "encrypt failed".into())
    }
    fn decrypt(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>> {
        if bytes.len() > MAX_MESSAGE {
            return Err("record too large".into());
        }
        let nonce = self.nonce()?;
        let p = self
            .cipher
            .decrypt((&nonce).into(), bytes)
            .map_err(|_| "decrypt failed")?;
        match p.first() {
            Some(0) if (2..=65537).contains(&p.len()) => Ok(Some(p[1..].to_vec())),
            Some(1) if p.len() == 1 => {
                self.eof = true;
                Ok(None)
            }
            _ => Err("invalid record".into()),
        }
    }
}
async fn binary<S: AsyncRead + AsyncWrite + Unpin>(ws: &mut WebSocketStream<S>) -> Result<Vec<u8>> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                ws.flush().await?;
            }
            _ => return Err("handshake message rejected".into()),
        }
    }
}
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut ws: WebSocketStream<S>,
    psk: &[u8],
    server: bool,
    timing: Timing,
) -> Result<Stream> {
    let secret = EphemeralSecret::random_from_rng(rand_core::OsRng);
    let public = PublicKey::from(&secret);
    let mut own = b"SRE1".to_vec();
    own.push(if server { 2 } else { 1 });
    own.extend_from_slice(public.as_bytes());
    let peer = if server {
        let p = binary(&mut ws).await?;
        validate_handshake(&p, 1)?;
        ws.send(Message::Binary(own.clone().into())).await?;
        p
    } else {
        ws.send(Message::Binary(own.clone().into())).await?;
        let p = binary(&mut ws).await?;
        validate_handshake(&p, 2)?;
        p
    };
    let peer_key: [u8; 32] = peer[5..].try_into()?;
    let dh = secret.diffie_hellman(&PublicKey::from(peer_key));
    let (m1, m2) = if server { (&peer, &own) } else { (&own, &peer) };
    let (c, s, _) = keys(psk, dh.as_bytes(), m1, m2)?;
    Ok(stream(
        ws,
        if server { c } else { s },
        if server { s } else { c },
        timing,
    ))
}
fn validate_handshake(p: &[u8], kind: u8) -> Result<()> {
    if p.len() != 37 || &p[..4] != b"SRE1" || p[4] != kind {
        return Err("invalid handshake".into());
    }
    Ok(())
}
fn broken() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "E2E transport failed")
}
enum Write {
    Data(Vec<u8>),
    Flush(oneshot::Sender<()>),
    Eof(oneshot::Sender<()>),
}
pub struct Stream {
    read: mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
    status: watch::Receiver<u8>,
    write: PollSender<Write>,
    ack: Option<oneshot::Receiver<()>>,
    ack_eof: bool,
    shutdown: bool,
    cancel: CancellationToken,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        // Records that arrived before an abnormal end are still delivered; the error (status 2) is
        // reported once the queue is drained, when the reader task has dropped its sender.
        if self.offset == self.current.len() {
            match self.read.poll_recv(cx) {
                Poll::Ready(Some(v)) => {
                    self.current = v;
                    self.offset = 0;
                }
                Poll::Ready(None) => {
                    return Poll::Ready(if *self.status.borrow() == 1 {
                        Ok(())
                    } else {
                        Err(broken())
                    })
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = buf.remaining().min(self.current.len() - self.offset);
        buf.put_slice(&self.current[self.offset..self.offset + n]);
        self.offset += n;
        Poll::Ready(Ok(()))
    }
}
impl Stream {
    fn poll_ack(&mut self, cx: &mut Context<'_>, eof: bool) -> Poll<io::Result<()>> {
        if eof && self.shutdown && self.ack.is_none() {
            return Poll::Ready(Ok(()));
        }
        if self.ack.is_none() {
            match self.write.poll_reserve(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => return Poll::Ready(Err(broken())),
                _ => {}
            }
            let (tx, rx) = oneshot::channel();
            if self
                .write
                .send_item(if eof {
                    Write::Eof(tx)
                } else {
                    Write::Flush(tx)
                })
                .is_err()
            {
                return Poll::Ready(Err(broken()));
            }
            self.ack = Some(rx);
            self.ack_eof = eof;
            if eof {
                self.shutdown = true;
            }
        }
        match Pin::new(self.ack.as_mut().unwrap()).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(r) => {
                self.ack = None;
                if r.is_err() {
                    return Poll::Ready(Err(broken()));
                }
                if eof && !self.ack_eof {
                    return self.poll_ack(cx, true);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.shutdown {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if b.is_empty() {
            return Poll::Ready(Ok(0));
        }
        match self.write.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(Err(broken())),
            _ => {
                let n = b.len().min(65536);
                Poll::Ready(
                    self.write
                        .send_item(Write::Data(b[..n].to_vec()))
                        .map(|_| n)
                        .map_err(|_| broken()),
                )
            }
        }
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_ack(cx, false)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_ack(cx, true)
    }
}
use std::future::Future;
fn stream<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    ws: WebSocketStream<S>,
    rx_key: [u8; 32],
    tx_key: [u8; 32],
    timing: Timing,
) -> Stream {
    let (mut sink, mut source) = ws.split();
    // Bounded backpressure: at most 16 records (~1 MiB) are queued. A consumer that stalls stops this
    // reader, so Pongs go uncounted and the keepalive closes the connection after about two periods.
    let (data_tx, data_rx) = mpsc::channel(16);
    let (write_tx, mut write_rx) = mpsc::channel(2);
    let (control_tx, mut control_rx) = mpsc::channel(8);
    let (status_tx, status_rx) = watch::channel(0u8);
    let (eof_tx, mut eof_rx) = watch::channel(false);
    let (pong_tx, mut pong_rx) = watch::channel(0u64);
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let status = status_tx.clone();
    tokio::spawn(async move {
        let mut records = Records::new(rx_key);
        let mut eof = false;
        let mut pongs = 0;
        let mut data_tx = Some(data_tx);
        loop {
            let msg = tokio::select! { _=stop.cancelled()=>break, m=source.next()=>m };
            match msg {
                Some(Ok(Message::Binary(b))) => match records.decrypt(&b) {
                    Ok(Some(d)) => {
                        let sent = tokio::select! {
                            _ = stop.cancelled() => false,
                            r = data_tx.as_ref().unwrap().send(d) => r.is_ok(),
                        };
                        if !sent {
                            break;
                        }
                    }
                    Ok(None) => {
                        eof = true;
                        let _ = status.send(1);
                        data_tx.take();
                        let _ = eof_tx.send(true);
                    }
                    Err(_) => break,
                },
                Some(Ok(Message::Ping(b))) => {
                    if control_tx.try_send(Message::Pong(b)).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Pong(_))) => {
                    pongs += 1;
                    let _ = pong_tx.send(pongs);
                }
                Some(Ok(Message::Close(_))) | None => {
                    break;
                }
                Some(Ok(Message::Frame(_))) => {}
                _ => break,
            }
            // EOF wakes readers immediately, but keep receiving WS control traffic.
        }
        if !eof {
            let _ = status.send(2);
        }
        stop.cancel();
    });
    let stop = cancel.clone();
    tokio::spawn(async move {
        let mut records = Records::new(tx_key);
        let jitter = config::random::<2>()
            .map(|b| u16::from_be_bytes(b) as f64 / 65535.0)
            .unwrap_or(0.5);
        let period = timing.ws_ping.mul_f64(0.9 + jitter * 0.2);
        let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        let mut last_pong = 0;
        let mut missed = 0;
        let mut received_eof = false;
        loop {
            let mut ack = None;
            let msg = tokio::select! { biased;
                _=stop.cancelled()=>break,
                Some(m)=control_rx.recv()=>m,
                _=eof_rx.changed(), if !received_eof => {received_eof=true; if records.eof && *eof_rx.borrow() {let _=timeout(timing.ws_write,sink.close()).await; break;} continue;},
                _=ping.tick()=> {
                    let p=*pong_rx.borrow_and_update();
                    if p!=last_pong {missed=0;}
                    last_pong=p;
                    if missed>=2 {break;}
                    missed+=1;
                    Message::Ping(Vec::new().into())
                },
                w=write_rx.recv()=>match w {
                    Some(Write::Data(d))=>match records.encrypt(Some(&d)){Ok(b)=>Message::Binary(b.into()),Err(_)=>break},
                    Some(Write::Eof(a))=>{ack=Some(a); match records.encrypt(None){Ok(b)=>Message::Binary(b.into()),Err(_)=>break}},
                    Some(Write::Flush(a))=>{let _=a.send(());continue;}, None=>break,
                }
            };
            if timeout(timing.ws_write, sink.send(msg))
                .await
                .map_or(true, |r| r.is_err())
            {
                break;
            }
            if let Some(a) = ack {
                let _ = a.send(());
            }
            if records.eof && *eof_rx.borrow() {
                let _ = timeout(timing.ws_write, sink.close()).await;
                break;
            }
        }
        let _ = timeout(timing.ws_write, sink.close()).await;
        if *status_tx.borrow() != 1 {
            let _ = status_tx.send(2);
        }
        stop.cancel();
    });
    Stream {
        read: data_rx,
        current: vec![],
        offset: 0,
        status: status_rx,
        write: PollSender::new(write_tx),
        ack: None,
        ack_eof: false,
        shutdown: false,
        cancel,
    }
}

/// Replays the bounded HTTP header bytes into tungstenite's handshake parser.
pub(crate) struct Prefixed<S> {
    prefix: std::io::Cursor<Vec<u8>>,
    inner: S,
}
impl<S> Prefixed<S> {
    pub(crate) fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix: std::io::Cursor::new(prefix),
            inner,
        }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if (self.prefix.position() as usize) < self.prefix.get_ref().len() {
            Pin::new(&mut self.prefix).poll_read(cx, b)
        } else {
            Pin::new(&mut self.inner).poll_read(cx, b)
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        b: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };
    fn hex(s: &str) -> Vec<u8> {
        config::unhex(s).unwrap()
    }
    #[test]
    fn specification_vectors() {
        let psk: Vec<u8> = (0..32).collect();
        let c: [u8; 32] = (32..64).collect::<Vec<_>>().try_into().unwrap();
        let s: [u8; 32] = (64..96).collect::<Vec<_>>().try_into().unwrap();
        let cp = x25519_dalek::x25519(c, x25519_dalek::X25519_BASEPOINT_BYTES);
        let sp = x25519_dalek::x25519(s, x25519_dalek::X25519_BASEPOINT_BYTES);
        let m1 = [b"SRE1\x01".as_slice(), &cp].concat();
        let m2 = [b"SRE1\x02".as_slice(), &sp].concat();
        assert_eq!(
            m1,
            hex("5352453101358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254")
        );
        assert_eq!(
            m2,
            hex("535245310279a631eede1bf9c98f12032cdeadd0e7a079398fc786b88cc846ec89af85a51a")
        );
        let dh = x25519_dalek::x25519(c, sp);
        assert_eq!(
            dh.to_vec(),
            hex("04c304fb1ca83cee75e206344231f33797e07d9929db670994b7c6fbeb1dc255")
        );
        assert_eq!(dh, x25519_dalek::x25519(s, cp));
        let (c, s, th) = keys(&psk, &dh, &m1, &m2).unwrap();
        assert_eq!(
            th.to_vec(),
            hex("418f81a9f94e6c194b81d5f083462fc6ecfd3c94df7f91bc14f055c14b48b219")
        );
        assert_eq!(
            c.to_vec(),
            hex("840004597a499caeca91c295843319f40c52774522be214b5d6571e378be3c5f")
        );
        assert_eq!(
            s.to_vec(),
            hex("c79a8bdb55636d741b168d40fe70b9afdc4e22f9933454f26f09e21f0c04ae62")
        );
        let mut enc = Records::new(s);
        assert_eq!(
            enc.encrypt(Some(b"hello")).unwrap(),
            hex("b80ae39be1e326570554cdbcd547fbc28382c664b5d6")
        );
        let mut enc = Records::new(c);
        let mut dec = Records::new(c);
        let world = enc.encrypt(Some(b"world")).unwrap();
        assert_eq!(world, hex("cd03d4ee6288919dea0bf25a474d3d5432331ef75b35"));
        assert_eq!(dec.decrypt(&world).unwrap(), Some(b"world".to_vec()));
        let eof = enc.encrypt(None).unwrap();
        assert_eq!(eof, hex("6fc3dc760a5c5ef263404a0f4436124ca8"));
        assert_eq!(dec.decrypt(&eof).unwrap(), None);
        assert!(dec.decrypt(&world).is_err());
        assert!(enc.encrypt(Some(b"after EOF")).is_err());
        assert_eq!(route(&psk), "e285a22ef3cf3f7b6c09bb2188bc75ee");
        assert!(keys(&psk, &[0; 32], &m1, &m2).is_err());
    }
    #[test]
    fn records_reject_tampering_replay_reordering_reflection_and_limits() {
        let key = [9; 32];
        for mode in 0..4 {
            let mut enc = Records::new(key);
            let mut dec = Records::new(key);
            let mut first = enc.encrypt(Some(b"first")).unwrap();
            let second = enc.encrypt(Some(b"second")).unwrap();
            match mode {
                0 => {
                    first[0] ^= 1;
                    assert!(dec.decrypt(&first).is_err());
                }
                1 => {
                    dec.decrypt(&first).unwrap();
                    assert!(dec.decrypt(&first).is_err());
                }
                2 => assert!(dec.decrypt(&second).is_err()),
                _ => assert!(Records::new([8; 32]).decrypt(&first).is_err()),
            }
        }
        let mut r = Records::new(key);
        r.seq = u64::MAX;
        assert!(r.encrypt(None).is_err());
        assert!(Records::new(key).encrypt(Some(&[])).is_err());
        assert!(Records::new(key).encrypt(Some(&vec![0; 65537])).is_err());
        for plain in [vec![0], vec![1, 0], vec![2]] {
            let cipher = ChaCha20Poly1305::new((&key).into());
            let bytes = cipher.encrypt((&[0; 12]).into(), plain.as_slice()).unwrap();
            assert!(Records::new(key).decrypt(&bytes).is_err());
        }
    }
    async fn raw(timing: Timing) -> (WebSocketStream<TcpStream>, Records, Stream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async_with_config(tcp, Some(ws_config()))
                .await
                .unwrap();
            handshake(ws, &[7; 32], true, timing).await.unwrap()
        });
        let (mut ws, _) = tokio_tungstenite::client_async(
            format!("ws://{addr}/"),
            TcpStream::connect(addr).await.unwrap(),
        )
        .await
        .unwrap();
        let secret = EphemeralSecret::random_from_rng(rand_core::OsRng);
        let m1 = [b"SRE1\x01".as_slice(), PublicKey::from(&secret).as_bytes()].concat();
        ws.send(Message::Binary(m1.clone().into())).await.unwrap();
        let m2 = binary(&mut ws).await.unwrap();
        let peer: [u8; 32] = m2[5..].try_into().unwrap();
        let dh = secret.diffie_hellman(&PublicKey::from(peer));
        let (c, _, _) = keys(&[7; 32], dh.as_bytes(), &m1, &m2).unwrap();
        (ws, Records::new(c), server.await.unwrap())
    }
    #[tokio::test]
    async fn abnormal_ws_end_is_io_error() {
        for mode in 0..9 {
            let (mut ws, mut enc, mut stream) = raw(Timing::default()).await;
            match mode {
                0 => ws.close(None).await.unwrap(),
                1 => ws.send(Message::Text("text".into())).await.unwrap(),
                2 => {
                    let mut b = enc.encrypt(Some(b"x")).unwrap();
                    b[0] ^= 1;
                    ws.send(Message::Binary(b.into())).await.unwrap();
                }
                3 => {
                    let b = enc.encrypt(Some(b"x")).unwrap();
                    ws.send(Message::Binary(b.clone().into())).await.unwrap();
                    ws.send(Message::Binary(b.into())).await.unwrap();
                }
                4 => {
                    enc.encrypt(Some(b"skip")).unwrap();
                    ws.send(Message::Binary(enc.encrypt(Some(b"x")).unwrap().into()))
                        .await
                        .unwrap();
                }
                5 => ws
                    .send(Message::Binary(vec![0; MAX_MESSAGE + 1].into()))
                    .await
                    .unwrap(),
                6 => {
                    drop(ws);
                }
                7 => {
                    use tokio_tungstenite::tungstenite::protocol::frame::{
                        coding::{Data, OpCode},
                        Frame,
                    };
                    ws.send(Message::Frame(Frame::message(
                        vec![0; 40000],
                        OpCode::Data(Data::Binary),
                        false,
                    )))
                    .await
                    .unwrap();
                    ws.send(Message::Frame(Frame::message(
                        vec![0; 40000],
                        OpCode::Data(Data::Continue),
                        true,
                    )))
                    .await
                    .unwrap();
                }
                _ => {
                    let wrong = Records::new([8; 32]).encrypt(Some(b"x")).unwrap();
                    ws.send(Message::Binary(wrong.into())).await.unwrap();
                }
            }
            let mut b = vec![];
            assert!(
                timeout(
                    std::time::Duration::from_secs(2),
                    stream.read_to_end(&mut b)
                )
                .await
                .unwrap()
                .is_err(),
                "mode {mode}"
            );
        }
    }
    #[tokio::test]
    async fn eof_half_close_and_fragmented_message() {
        let (mut ws, mut enc, mut stream) = raw(Timing::default()).await;
        let record = enc.encrypt(Some(b"hello")).unwrap();
        use tokio_tungstenite::tungstenite::protocol::frame::{
            coding::{Data, OpCode},
            Frame,
        };
        ws.send(Message::Frame(Frame::message(
            record[..10].to_vec(),
            OpCode::Data(Data::Binary),
            false,
        )))
        .await
        .unwrap();
        ws.send(Message::Frame(Frame::message(
            record[10..].to_vec(),
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
        ws.send(Message::Binary(enc.encrypt(None).unwrap().into()))
            .await
            .unwrap();
        let mut received = vec![];
        timeout(
            std::time::Duration::from_secs(2),
            stream.read_to_end(&mut received),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(received, b"hello");
        stream.write_all(b"response after EOF").await.unwrap();
        stream.shutdown().await.unwrap();
        assert!(matches!(ws.next().await, Some(Ok(Message::Binary(_)))));
        assert!(matches!(ws.next().await, Some(Ok(Message::Binary(_)))));
    }
    #[tokio::test]
    async fn ping_survives_unread_application_data_and_pong_keeps_alive() {
        // Long enough that decrypting the queued records in a debug build cannot delay a Pong past
        // two periods.
        let timing = Timing {
            ws_ping: std::time::Duration::from_millis(200),
            ..Timing::default()
        };
        let (mut ws, mut enc, _stream) = raw(timing).await;
        for _ in 0..8 {
            ws.send(Message::Binary(
                enc.encrypt(Some(&vec![0; 65536])).unwrap().into(),
            ))
            .await
            .unwrap();
        }
        for _ in 0..5 {
            let m = timeout(std::time::Duration::from_secs(1), ws.next())
                .await
                .unwrap();
            assert!(matches!(m, Some(Ok(Message::Ping(_)))), "{m:?}");
            ws.flush().await.unwrap();
        }
    }
    #[tokio::test]
    async fn keepalive_ping_and_no_pong_close() {
        let timing = Timing {
            ws_ping: std::time::Duration::from_millis(30),
            ..Timing::default()
        };
        let (mut ws, _, mut stream) = raw(timing).await;
        assert!(matches!(ws.next().await, Some(Ok(Message::Ping(_)))));
        // Do not poll/flush the client again: automatic Pong stays queued.
        let mut b = [0];
        assert!(
            timeout(std::time::Duration::from_secs(1), stream.read(&mut b))
                .await
                .unwrap()
                .is_err()
        );
    }
}
