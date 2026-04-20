//! KCP session wrapper. Owns the KCP state, a tick driver task, and
//! talks to a DatagramChannel below. Exposes async reliable byte I/O.
//!
//! Architecture (duplex-pipe variant):
//!
//! ```text
//! [KcpStream user API] <== tokio::io::duplex ==> [driver task] <-> kcp::Kcp <-> DatagramChannel
//! ```
//!
//! The driver task owns the KCP state exclusively — no Mutex in the hot path.
//! User writes go through the duplex pipe; the driver reads them and calls
//! `kcp.send()`. In the inbound direction the driver calls `kcp.input()` on
//! arriving datagrams and pumps `kcp.recv()` back through the duplex pipe.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};

use crate::tunnel::adapter::DatagramChannel;
use crate::tunnel::metrics::{Event, EventEmitter};

// ──────────────────────────────────────────────────────────────────────────────
// Profiles
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Throughput,
    Latency,
}

impl Profile {
    pub fn from_env() -> Self {
        match std::env::var("TUNNEL_PROFILE").ok().as_deref() {
            Some("throughput") => Self::Throughput,
            _ => Self::Latency,
        }
    }

    pub fn params(self) -> KcpParams {
        match self {
            // Throughput: large windows, conservative RTO-based retransmit.
            // VK CMAF RTT is 30-60s so fast retransmit (resend=N) mis-fires
            // from duplicate ACKs under reordering. Rely on RTO doubling
            // instead. nc=1 disables congestion control so snd_wnd is the
            // actual in-flight ceiling.
            Profile::Throughput => KcpParams {
                snd_wnd: 512,
                rcv_wnd: 512,
                nodelay: 0,
                interval: 40,
                resend: 0,
                nc: 1,
                min_rto: 200,
            },
            Profile::Latency => KcpParams {
                snd_wnd: 32,
                rcv_wnd: 32,
                nodelay: 1,
                interval: 10,
                resend: 2,
                nc: 1,
                min_rto: 100,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KcpParams {
    pub snd_wnd: u16,
    pub rcv_wnd: u16,
    pub nodelay: i32,
    pub interval: i32,
    pub resend: i32,
    pub nc: i32,
    /// Note: `u32` — `set_rx_minrto` takes `u32` in kcp 0.5.x.
    pub min_rto: u32,
}

// ──────────────────────────────────────────────────────────────────────────────
// DgramOutput: bridges the sync `std::io::Write` that kcp expects to an
// unbounded mpsc channel so the driver task can collect KCP output datagrams
// and forward them to the DatagramChannel asynchronously.
// ──────────────────────────────────────────────────────────────────────────────

struct DgramOutput {
    sink: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl Write for DgramOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.sink.send(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// KcpSession
// ──────────────────────────────────────────────────────────────────────────────

/// A running KCP session.
///
/// Call [`KcpSession::stream`] to obtain an `AsyncRead + AsyncWrite` handle.
pub struct KcpSession {
    channel: Arc<dyn DatagramChannel>,
    profile: Profile,
    emitter: Arc<EventEmitter>,
}

impl KcpSession {
    /// Construct and record configuration. Background tasks are launched per-stream.
    pub fn start(
        channel: Arc<dyn DatagramChannel>,
        profile: Profile,
        emitter: Arc<EventEmitter>,
    ) -> Self {
        Self { channel, profile, emitter }
    }

    /// Obtain an `AsyncRead + AsyncWrite` handle backed by a fresh KCP connection.
    ///
    /// The returned `KcpStream` is the user-facing byte-stream. Background tasks
    /// are started when this is called.
    pub fn stream(self: Arc<Self>) -> KcpStream {
        let p = self.profile.params();

        // Outbound pump channel: KCP writer → network send task.
        let (kcp_out_tx, mut kcp_out_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        // Build KCP.
        let mut kcp = kcp::Kcp::new(0x1234_5678, DgramOutput { sink: kcp_out_tx });
        kcp.set_wndsize(p.snd_wnd, p.rcv_wnd);
        kcp.set_nodelay(p.nodelay != 0, p.interval, p.resend, p.nc != 0);
        kcp.set_rx_minrto(p.min_rto);
        let mtu = self.channel.max_payload().saturating_sub(36);
        let _ = kcp.set_mtu(mtu);

        // Duplex pipe: one half stays here (returned as KcpStream), the other
        // half goes to the driver task.
        let pipe_cap = 256 * 1024; // 256 KiB buffer
        let (user_half, driver_half) = tokio::io::duplex(pipe_cap);

        // Inbound datagrams from the network layer.
        let (net_in_tx, mut net_in_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(512);

        // ── Network receive pump ──────────────────────────────────────────────
        {
            let channel = Arc::clone(&self.channel);
            tokio::spawn(async move {
                while let Ok(buf) = channel.recv().await {
                    if net_in_tx.send(buf).await.is_err() {
                        break;
                    }
                }
            });
        }

        // ── Network send pump ─────────────────────────────────────────────────
        {
            let channel = Arc::clone(&self.channel);
            let emitter = Arc::clone(&self.emitter);
            tokio::spawn(async move {
                while let Some(buf) = kcp_out_rx.recv().await {
                    emitter.emit(
                        Event::new("kcp", "tx").field("len", buf.len() as i64),
                    );
                    if channel.send(buf).await.is_err() {
                        break;
                    }
                }
            });
        }

        // ── Main driver task ──────────────────────────────────────────────────
        // Owns `kcp` and `driver_half` of the duplex pipe.
        {
            let tick_interval =
                Duration::from_millis(p.interval.max(1) as u64);

            tokio::spawn(async move {
                let (mut pipe_rx, mut pipe_tx) =
                    tokio::io::split(driver_half);

                let mut app_buf = vec![0u8; 65536];
                let mut recv_buf = vec![0u8; 65536];
                let mut ticker =
                    tokio::time::interval(tick_interval);
                ticker.set_missed_tick_behavior(
                    tokio::time::MissedTickBehavior::Skip,
                );

                loop {
                    tokio::select! {
                        // Tick: update KCP state machine.
                        _ = ticker.tick() => {
                            let now = now_ms_u32();
                            let _ = kcp.update(now);
                            drain_to_pipe(&mut kcp, &mut pipe_tx, &mut recv_buf).await;
                        }

                        // Inbound datagram from the network.
                        Some(buf) = net_in_rx.recv() => {
                            let _ = kcp.input(&buf);
                            drain_to_pipe(&mut kcp, &mut pipe_tx, &mut recv_buf).await;
                        }

                        // Outbound bytes from the application via the pipe.
                        result = pipe_rx.read(&mut app_buf) => {
                            match result {
                                Ok(0) | Err(_) => break, // pipe closed
                                Ok(n) => {
                                    let _ = kcp.send(&app_buf[..n]);
                                    let _ = kcp.flush();
                                }
                            }
                        }
                    }
                }
            });
        }

        KcpStream { inner: user_half }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helper: drain KCP recv queue → write to user's pipe half.
// ──────────────────────────────────────────────────────────────────────────────

async fn drain_to_pipe<W: AsyncWriteExt + Unpin>(
    kcp: &mut kcp::Kcp<DgramOutput>,
    pipe_tx: &mut W,
    buf: &mut Vec<u8>,
) {
    while let Ok(sz) = kcp.peeksize() {
        if buf.len() < sz {
            buf.resize(sz, 0);
        }
        match kcp.recv(&mut buf[..sz]) {
            Ok(n) => {
                if pipe_tx.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn now_ms_u32() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32
}

// ──────────────────────────────────────────────────────────────────────────────
// KcpStream: thin wrapper making the duplex half `Send + 'static`
// ──────────────────────────────────────────────────────────────────────────────

/// User-facing byte-stream handle.
/// Implements `AsyncRead + AsyncWrite + Unpin + Send + 'static`.
pub struct KcpStream {
    inner: tokio::io::DuplexStream,
}

impl Unpin for KcpStream {}

impl AsyncRead for KcpStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for KcpStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Unit tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_from_env_defaults_to_latency() {
        std::env::remove_var("TUNNEL_PROFILE");
        assert_eq!(Profile::from_env(), Profile::Latency);
    }

    #[test]
    fn profile_parameters_match_spec() {
        assert_eq!(Profile::Throughput.params().snd_wnd, 512);
        assert_eq!(Profile::Throughput.params().nodelay, 0);
        assert_eq!(Profile::Latency.params().snd_wnd, 32);
        assert_eq!(Profile::Latency.params().nodelay, 1);
    }
}
