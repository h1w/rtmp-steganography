//! In-memory lossy channel for tests. Bidirectional, two endpoints.
//! Spawns tokio tasks to model per-direction loss, base latency, and jitter.

use std::time::Duration;
use tokio::sync::mpsc;

pub struct Endpoint {
    pub(crate) tx: mpsc::Sender<Vec<u8>>,
    pub(crate) rx: mpsc::Receiver<Vec<u8>>,
}

pub struct Config {
    pub loss_pct: u8,     // 0..100
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub buffer: usize,    // mpsc bound
}

/// Create two linked endpoints. Bytes sent on A arrive on B and vice versa.
/// No loss/latency modeling here — wrap with `with_simulation` to add that.
pub fn pair(cfg: Config) -> (Endpoint, Endpoint) {
    let (a_out, b_in) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let (b_out, a_in) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    (
        Endpoint { tx: a_out, rx: a_in },
        Endpoint { tx: b_out, rx: b_in },
    )
}

impl Endpoint {
    pub async fn send(&self, buf: Vec<u8>) -> Result<(), ()> {
        self.tx.send(buf).await.map_err(|_| ())
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

/// Wraps an Endpoint with per-direction loss, base latency, and uniform jitter.
/// Returns a (user-facing send, user-facing recv) pair.
///
/// Loss is applied only on the RECEIVE side (dropping arriving frames) — this is
/// sufficient to model an unreliable channel and keeps the simulation simple.
pub fn with_simulation(
    ep: Endpoint,
    cfg: Config,
) -> (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>) {
    let (user_tx, mut out_queue) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let (sim_tx, user_rx) = mpsc::channel::<Vec<u8>>(cfg.buffer);
    let Endpoint { tx: wire_tx, rx: mut wire_rx } = ep;

    // Outbound: pass through, no modeling of sender-side loss (receiver-side is enough).
    tokio::spawn(async move {
        while let Some(buf) = out_queue.recv().await {
            if wire_tx.send(buf).await.is_err() { break; }
        }
    });

    // Inbound: drop loss_pct%, add latency + uniform jitter.
    let loss = cfg.loss_pct;
    let base = cfg.latency_ms;
    let jitter = cfg.jitter_ms;
    tokio::spawn(async move {
        use rand::Rng;
        while let Some(buf) = wire_rx.recv().await {
            if rand::thread_rng().gen_range(0u8..100) < loss { continue; }
            let delay = base + rand::thread_rng().gen_range(0..=jitter.max(1));
            let sim_tx = sim_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                let _ = sim_tx.send(buf).await;
            });
        }
    });

    (user_tx, user_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn zero_loss_delivers_all_frames() {
        let (a, mut b) = pair(Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 16 });
        for i in 0..10u8 {
            a.send(vec![i]).await.unwrap();
        }
        for i in 0..10u8 {
            let f = b.recv().await.unwrap();
            assert_eq!(f, vec![i]);
        }
    }

    #[tokio::test]
    async fn simulation_drops_frames_with_loss() {
        let (a, b) = pair(Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 256 });
        // Loopback task: echo every frame arriving on B back to A so the
        // simulation's inbound path (a_in ← b_out) actually receives data.
        let Endpoint { tx: b_tx, rx: mut b_rx } = b;
        tokio::spawn(async move {
            while let Some(buf) = b_rx.recv().await {
                if b_tx.send(buf).await.is_err() { break; }
            }
        });
        let (tx, mut rx) = with_simulation(a, Config { loss_pct: 50, latency_ms: 1, jitter_ms: 0, buffer: 256 });
        for _ in 0..200 { tx.send(vec![0u8]).await.unwrap(); }
        drop(tx);
        let mut delivered = 0;
        // Read with a timeout so the test can't hang if lots of frames are in flight delays
        loop {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Some(_)) => delivered += 1,
                _ => break,
            }
        }
        // With 50% loss over 200 frames, expect 70..130 (wide margin for randomness)
        assert!(delivered > 70 && delivered < 130, "delivered={delivered}");
    }
}
