//! Datagram channel abstraction: KCP speaks this; flicker and mem impls fulfill it.

use async_trait::async_trait;

#[async_trait]
pub trait DatagramChannel: Send + Sync + 'static {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()>;
    async fn recv(&self) -> std::io::Result<Vec<u8>>;
    fn max_payload(&self) -> usize;
}

use std::sync::mpsc as stdmpsc;
use tokio::sync::Mutex as TokioMutex;
use crate::flicker::{FLICKER_MAX_PAYLOAD_BYTES, InboundMessage, OutboundMessage};

pub const MSG_TYPE_TUNNEL: u8 = 0x02;

/// Bridges tokio async tunnel code to the existing sync flicker mpsc channels.
///
/// A background OS thread reads `InboundMessage`s off the sync receiver, filters
/// those with `msg_type == MSG_TYPE_TUNNEL`, and forwards their payloads to an
/// async tokio channel that `DatagramChannel::recv` consumes.
pub struct FlickerChannel {
    out_tx: stdmpsc::Sender<OutboundMessage>,
    in_rx: TokioMutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
}

impl FlickerChannel {
    pub fn new(
        out_tx: stdmpsc::Sender<OutboundMessage>,
        in_rx_sync: stdmpsc::Receiver<InboundMessage>,
    ) -> Self {
        let (async_tx, async_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
        std::thread::spawn(move || {
            while let Ok(msg) = in_rx_sync.recv() {
                if msg.msg_type == MSG_TYPE_TUNNEL {
                    if async_tx.blocking_send(msg.payload).is_err() { break; }
                }
            }
        });
        Self { out_tx, in_rx: TokioMutex::new(async_rx) }
    }
}

#[async_trait]
impl DatagramChannel for FlickerChannel {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
        let msg = OutboundMessage { msg_type: MSG_TYPE_TUNNEL, payload: buf };
        self.out_tx.send(msg).map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.in_rx.lock().await.recv().await
            .ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize { FLICKER_MAX_PAYLOAD_BYTES }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::testchannel;
    use tokio::sync::Mutex;

    struct MemAdapter {
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
        mtu: usize,
    }

    #[async_trait]
    impl DatagramChannel for MemAdapter {
        async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
            self.tx.send(buf).await.map_err(|_| std::io::ErrorKind::BrokenPipe.into())
        }
        async fn recv(&self) -> std::io::Result<Vec<u8>> {
            self.rx.lock().await.recv().await.ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
        }
        fn max_payload(&self) -> usize { self.mtu }
    }

    #[tokio::test]
    async fn mem_adapter_roundtrips() {
        let (a, b) = testchannel::pair(testchannel::Config {
            loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 16,
        });
        let testchannel::Endpoint { tx: atx, rx: arx } = a;
        let testchannel::Endpoint { tx: btx, rx: brx } = b;
        let ad = MemAdapter { tx: atx, rx: Mutex::new(arx), mtu: 256 };
        let bd = MemAdapter { tx: btx, rx: Mutex::new(brx), mtu: 256 };
        ad.send(b"hello".to_vec()).await.unwrap();
        let got = bd.recv().await.unwrap();
        assert_eq!(got, b"hello");
    }
}
