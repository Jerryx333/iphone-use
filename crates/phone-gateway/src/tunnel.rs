//! Independent data connections; no tunnel frames ever enter the control stream.
use crate::{gateway::Timing, protocol, Result};
use serde_json::{json, Value};
use std::{collections::VecDeque, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, Notify, Semaphore},
    time::{timeout, timeout_at, Instant},
};
pub trait ByteStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> ByteStream for T {}
pub type BoxStream = Box<dyn ByteStream>;

// Count successfully written raw bytes even if copying later fails.
struct Counted<S> {
    stream: S,
    written: u64,
}
impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Counted<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for Counted<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.stream).poll_write(cx, buf);
        if let std::task::Poll::Ready(Ok(n)) = result {
            self.written += n as u64;
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}
struct Idle {
    id: uuid::Uuid,
    stream: BoxStream,
    since: Instant,
}
#[derive(Clone)]
pub(crate) struct Tunnels {
    idle: Arc<Mutex<VecDeque<Idle>>>,
    notify: Arc<Notify>,
    slots: Arc<Semaphore>,
}
impl Default for Tunnels {
    fn default() -> Self {
        Self {
            idle: Arc::default(),
            notify: Arc::default(),
            slots: Arc::new(Semaphore::new(16)),
        }
    }
}
pub(crate) fn validate_forwards(
    runner: Option<SocketAddr>,
    mjpeg: Option<SocketAddr>,
) -> Result<()> {
    if [runner, mjpeg]
        .into_iter()
        .flatten()
        .any(|a| !a.ip().is_loopback())
    {
        return Err("forward must bind a loopback address".into());
    }
    Ok(())
}
impl Tunnels {
    pub(crate) async fn snapshot(&self) -> Value {
        json!({"idle":self.idle.lock().await.len(), "active":16-self.slots.available_permits()})
    }
    pub(crate) async fn insert(&self, stream: BoxStream, lifetime: Duration) {
        let mut pool = self.idle.lock().await;
        if pool.len() == 8 {
            tracing::info!(event = "close", code = "pool_full");
            return;
        }
        let id = uuid::Uuid::new_v4();
        let since = Instant::now();
        pool.push_back(Idle { id, stream, since });
        drop(pool);
        self.notify.notify_waiters();
        let weak = Arc::downgrade(&self.idle);
        tokio::spawn(async move {
            tokio::time::sleep_until(since + lifetime).await;
            if let Some(pool) = weak.upgrade() {
                let mut pool = pool.lock().await;
                if let Some(index) = pool.iter().position(|entry| entry.id == id) {
                    pool.remove(index);
                    tracing::info!(
                        event = "close",
                        code = "idle_timeout",
                        ms = since.elapsed().as_millis() as u64
                    );
                }
            }
        });
    }
    pub(crate) async fn listen(
        &self,
        listener: TcpListener,
        port: u16,
        timing: Timing,
    ) -> Result<()> {
        loop {
            // A transient accept error (ECONNABORTED, EMFILE) must not end forwarding for good.
            let local = match listener.accept().await {
                Ok((local, _)) => local,
                Err(error) => {
                    tracing::warn!(event = "accept_failed", port, kind = ?error.kind());
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let started = Instant::now();
            let Ok(permit) = self.slots.clone().try_acquire_owned() else {
                tracing::info!(event = "close", port, code = "active_limit");
                continue;
            };
            let tunnels = self.clone();
            let timing = timing.clone();
            tokio::spawn(async move {
                let _permit = permit;
                tunnels.forward(local, port, timing, started).await;
            });
        }
    }
    async fn forward(&self, local: TcpStream, port: u16, timing: Timing, started: Instant) {
        let deadline = started + timing.tunnel_wait;
        let acquire = async {
            loop {
                // Register before checking the pool so concurrent arrivals cannot be lost.
                let notified = self.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let idle = self.idle.lock().await.pop_front();
                let Some(mut idle) = idle else {
                    notified.await;
                    continue;
                };
                if idle.since.elapsed() >= timing.tunnel_idle {
                    continue;
                }
                let open = async {
                    protocol::write_frame(&mut idle.stream, &json!({"t":"open","port":port}))
                        .await?;
                    let frame =
                        protocol::read_frame(&mut idle.stream, protocol::PRE_AUTH_LIMIT).await?;
                    if frame["t"] != "opened" || frame["ok"] != true {
                        return Err("open_rejected".into());
                    }
                    Ok::<(), crate::Error>(())
                };
                let code = match timeout(timing.tunnel_open, open).await {
                    Ok(Ok(())) => return idle.stream,
                    Ok(Err(_)) => "open_failed",
                    Err(_) => "open_timeout",
                };
                tracing::info!(
                    event = "close",
                    port,
                    code,
                    ms = started.elapsed().as_millis() as u64
                );
            }
        };
        let Ok(remote) = timeout_at(deadline, acquire).await else {
            tracing::info!(
                event = "close",
                port,
                code = "wait_timeout",
                ms = started.elapsed().as_millis() as u64
            );
            return;
        };
        tracing::info!(event = "open", port);
        // Exact frame reads leave subsequent raw bytes in TLS, and Tokio propagates half-close.
        let mut local = Counted {
            stream: local,
            written: 0,
        };
        let mut remote = Counted {
            stream: remote,
            written: 0,
        };
        let result = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
        let sent = remote.written;
        let received = local.written;
        let code = if result.is_ok() { "eof" } else { "copy_failed" };
        tracing::info!(
            event = "close",
            port,
            sent,
            received,
            code,
            ms = started.elapsed().as_millis() as u64
        );
    }
}
