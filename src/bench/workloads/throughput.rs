//! Native Rust throughput workload — streams raw bytes through the SOCKS5
//! tunnel to a raw echo server, measures bidirectional goodput with no
//! external dependencies (no iperf3, no proxychains).
//!
//! Counters are `AtomicU64` so that even if the whole stream times out we
//! can still report *partial* bytes_sent / bytes_received — that's enough
//! to derive real goodput from an aborted run.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::tunnel::metrics::{Event, EventEmitter};

/// Hard upper bound on the whole stream. Stops the workload even if the tunnel
/// stalls so the bench never hangs forever.
const STREAM_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(
    socks: SocketAddr,
    target_host: &str,
    target_port: u16,
    total_bytes: u64,
    em: Arc<EventEmitter>,
) {
    let sent_counter = Arc::new(AtomicU64::new(0));
    let recv_counter = Arc::new(AtomicU64::new(0));

    let start = Instant::now();
    let fut = stream_roundtrip(
        socks, target_host.to_string(), target_port, total_bytes,
        Arc::clone(&sent_counter), Arc::clone(&recv_counter),
    );
    let result = tokio::time::timeout(STREAM_TIMEOUT, fut).await;
    let elapsed = start.elapsed();

    let sent = sent_counter.load(Ordering::SeqCst);
    let received = recv_counter.load(Ordering::SeqCst);
    let (ok, fail_stage): (bool, Option<String>) = match result {
        Ok(Ok(())) => (true, None),
        Ok(Err(e)) => (false, Some(e)),
        Err(_) => (false, Some(format!("stream_timeout_at_{}ms", elapsed.as_millis()))),
    };

    let sec = elapsed.as_secs_f64().max(1e-6);
    // one-way user-visible goodput: received bytes per elapsed second.
    let oneway_bps = ((received as f64) * 8.0 / sec) as u64;
    // tunnel-internal throughput: each user byte goes twice (send + echo).
    let tunnel_internal_bps = ((received as f64) * 8.0 * 2.0 / sec) as u64;

    let mut ev = Event::new("bench", "throughput_done")
        .field("workload", "throughput_raw")
        .field("ok", ok)
        .field("bytes_target", total_bytes as i64)
        .field("bytes_sent", sent as i64)
        .field("bytes_received", received as i64)
        .field("elapsed_ms", elapsed.as_millis() as i64)
        .field("oneway_bps", oneway_bps as i64)
        .field("oneway_kbits_per_s", (oneway_bps as f64 / 1000.0 * 100.0).round() / 100.0)
        .field("tunnel_internal_bps", tunnel_internal_bps as i64)
        .field("tunnel_internal_kbits_per_s", (tunnel_internal_bps as f64 / 1000.0 * 100.0).round() / 100.0);
    if let Some(stage) = fail_stage {
        ev = ev.field("fail_stage", stage);
    }
    em.emit(ev);
}

async fn stream_roundtrip(
    socks: SocketAddr,
    host: String,
    port: u16,
    total_bytes: u64,
    sent_counter: Arc<AtomicU64>,
    recv_counter: Arc<AtomicU64>,
) -> Result<(), String> {
    let mut c = TcpStream::connect(socks).await.map_err(|e| format!("socks_connect: {e}"))?;
    c.write_all(&[5, 1, 0]).await.map_err(|e| format!("greet_write: {e}"))?;
    let mut g = [0u8; 2];
    c.read_exact(&mut g).await.map_err(|e| format!("greet_read: {e}"))?;
    if g != [5, 0] { return Err("bad_greet".into()); }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await.map_err(|e| format!("req_write: {e}"))?;
    let mut rep = [0u8; 10];
    c.read_exact(&mut rep).await.map_err(|e| format!("req_read: {e}"))?;
    if rep[1] != 0 { return Err(format!("socks_reply_{}", rep[1])); }

    let (mut rx, mut tx) = c.into_split();

    let tx_task = {
        let counter = Arc::clone(&sent_counter);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            for (i, b) in buf.iter_mut().enumerate() { *b = (i & 0xff) as u8; }
            let mut sent: u64 = 0;
            while sent < total_bytes {
                let chunk = ((total_bytes - sent) as usize).min(buf.len());
                if tx.write_all(&buf[..chunk]).await.is_err() { break; }
                sent += chunk as u64;
                counter.store(sent, Ordering::SeqCst);
            }
            let _ = tx.shutdown().await;
        })
    };

    let rx_task = {
        let counter = Arc::clone(&recv_counter);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let mut received: u64 = 0;
            while received < total_bytes {
                let n = match rx.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                received += n as u64;
                counter.store(received, Ordering::SeqCst);
            }
        })
    };

    let _ = tx_task.await;
    let _ = rx_task.await;
    Ok(())
}
