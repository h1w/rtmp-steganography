//! yamux session glue: wraps an AsyncRead+AsyncWrite into a multiplexed session.
//!
//! # Architecture
//!
//! `yamux 0.13` exposes only poll-based methods on `Connection` and requires the
//! caller to drive I/O by repeatedly invoking `poll_next_inbound`.  We spawn a
//! single background task that owns the `Connection` and loops on
//! `poll_next_inbound`, forwarding accepted `Stream`s through an mpsc channel.
//! Outbound stream requests are sent to the same task via a command channel so
//! that only one task ever holds `&mut Connection` at a time.

use std::io;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::TokioAsyncReadCompatExt;

/// A running yamux session backed by a driver task.
///
/// Cheaply cloneable — clones share the same underlying connection.
#[derive(Clone)]
pub struct MuxSession {
    /// Channel for requesting new outbound streams.
    outbound_tx: mpsc::Sender<oneshot::Sender<io::Result<yamux::Stream>>>,
    /// Channel that receives newly accepted inbound streams.
    inbound_rx: std::sync::Arc<tokio::sync::Mutex<mpsc::Receiver<io::Result<yamux::Stream>>>>,
}

impl MuxSession {
    /// Wrap `stream` as a yamux **client** (opens streams with odd IDs).
    pub fn client<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::new(stream, yamux::Mode::Client)
    }

    /// Wrap `stream` as a yamux **server** (opens streams with even IDs).
    pub fn server<S>(stream: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::new(stream, yamux::Mode::Server)
    }

    fn new<S>(stream: S, mode: yamux::Mode) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let compat = stream.compat();
        let mut conn = yamux::Connection::new(compat, yamux::Config::default(), mode);

        // Channel for delivering inbound streams to callers of `accept_stream`.
        let (inbound_tx, inbound_rx) = mpsc::channel::<io::Result<yamux::Stream>>(64);
        // Channel for outbound stream requests: caller sends a oneshot for the reply.
        let (outbound_tx, mut outbound_rx) =
            mpsc::channel::<oneshot::Sender<io::Result<yamux::Stream>>>(64);

        // Driver task: owns `conn`, drives I/O.
        tokio::spawn(async move {
            // Pending outbound request waiting for the connection to be ready.
            let mut pending_outbound: Option<oneshot::Sender<io::Result<yamux::Stream>>> = None;

            loop {
                // If there is a pending outbound request, try to fulfill it via
                // `poll_new_outbound` before waiting for the next inbound frame.
                if let Some(reply_tx) = pending_outbound.take() {
                    match futures::future::poll_fn(|cx| conn.poll_new_outbound(cx)).await {
                        Ok(stream) => {
                            // Ignore send error — caller may have dropped the receiver.
                            let _ = reply_tx.send(Ok(stream));
                        }
                        Err(e) => {
                            let _ = reply_tx.send(Err(io::Error::other(e.to_string())));
                            // Connection is broken; drain remaining outbound requests.
                            while let Ok(tx) = outbound_rx.try_recv() {
                                let _ = tx.send(Err(io::Error::from(io::ErrorKind::BrokenPipe)));
                            }
                            return;
                        }
                    }
                }

                // Drive inbound, also flushing outbound frames queued by streams.
                // Interleave checking for new outbound requests.
                tokio::select! {
                    // Connection produced an inbound stream (or closed).
                    result = futures::future::poll_fn(|cx| conn.poll_next_inbound(cx)) => {
                        match result {
                            Some(Ok(stream)) => {
                                if inbound_tx.send(Ok(stream)).await.is_err() {
                                    // No receiver — stop.
                                    return;
                                }
                            }
                            Some(Err(e)) => {
                                let io_err = io::Error::other(e.to_string());
                                // Best-effort notify inbound side.
                                let _ = inbound_tx.send(Err(io_err)).await;
                                return;
                            }
                            None => {
                                // Connection closed cleanly.
                                return;
                            }
                        }
                    }
                    // Caller wants a new outbound stream.
                    maybe_reply_tx = outbound_rx.recv() => {
                        match maybe_reply_tx {
                            Some(reply_tx) => {
                                pending_outbound = Some(reply_tx);
                                // Loop back so we call poll_new_outbound first.
                            }
                            None => {
                                // All MuxSession handles dropped.
                                return;
                            }
                        }
                    }
                }
            }
        });

        Self {
            outbound_tx,
            inbound_rx: std::sync::Arc::new(tokio::sync::Mutex::new(inbound_rx)),
        }
    }

    /// Open a new outbound stream (client-initiated).
    pub async fn open_stream(&self) -> io::Result<yamux::Stream> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.outbound_tx
            .send(reply_tx)
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        reply_rx
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?
    }

    /// Accept the next inbound stream (server-initiated from the remote peer).
    pub async fn accept_stream(&self) -> io::Result<yamux::Stream> {
        let mut guard = self.inbound_rx.lock().await;
        guard
            .recv()
            .await
            .unwrap_or_else(|| Err(io::Error::from(io::ErrorKind::BrokenPipe)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{AsyncReadExt as FRead, AsyncWriteExt as FWrite};
    use tokio::io::duplex;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mux_over_tokio_duplex_roundtrips() {
        let (a, b) = duplex(8192);
        let client = MuxSession::client(a);
        let server = MuxSession::server(b);

        let server_task = tokio::spawn(async move {
            let mut s = server.accept_stream().await.unwrap();
            let mut buf = [0u8; 32];
            let n = FRead::read(&mut s, &mut buf).await.unwrap();
            FWrite::write_all(&mut s, &buf[..n]).await.unwrap();
            FWrite::close(&mut s).await.unwrap();
        });

        let mut cs = client.open_stream().await.unwrap();
        FWrite::write_all(&mut cs, b"ping").await.unwrap();
        FWrite::flush(&mut cs).await.unwrap();
        let mut buf = [0u8; 32];
        let n = FRead::read(&mut cs, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ping");
        server_task.await.unwrap();
    }
}
