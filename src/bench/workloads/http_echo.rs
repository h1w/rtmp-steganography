use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::tunnel::metrics::{Event, EventEmitter};

/// Upper bound for any single http_echo roundtrip. VK HLS RTT is ~10s;
/// 1 KB through 4 round trips (SOCKS + CONNECT + HTTP req/resp) is ~60s
/// realistic, so 90s is safe headroom without hiding real stalls.
const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn run(
    socks: SocketAddr,
    target_host: &str,
    target_port: u16,
    payload_bytes: usize,
    iterations: usize,
    em: Arc<EventEmitter>,
) {
    for i in 0..iterations {
        let fut = one_roundtrip(socks, target_host, target_port, payload_bytes, i as u64, &em);
        match tokio::time::timeout(ROUNDTRIP_TIMEOUT, fut).await {
            Ok(_) => {}
            Err(_) => {
                em.emit(Event::new("bench", "request_done")
                    .field("workload", "http_echo")
                    .field("id", i as i64)
                    .field("ok", false)
                    .field("fail_stage", "timeout")
                    .field("total_ms", ROUNDTRIP_TIMEOUT.as_millis() as i64)
                    .field("latency_ms", ROUNDTRIP_TIMEOUT.as_millis() as i64)
                    .field("bytes", 0));
            }
        }
    }
}

async fn one_roundtrip(
    socks: SocketAddr,
    host: &str,
    port: u16,
    bytes: usize,
    id: u64,
    em: &Arc<EventEmitter>,
) -> std::io::Result<()> {
    let start = Instant::now();
    let mut c = TcpStream::connect(socks).await?;
    let socks_connect_ms = start.elapsed().as_millis() as u64;

    c.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2];
    c.read_exact(&mut g).await?;
    if g != [5, 0] {
        emit_fail(em, "http_echo", id, start.elapsed().as_millis() as u64, "bad_greet");
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "bad socks5 greet"));
    }
    let socks_greet_ms = start.elapsed().as_millis() as u64;

    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await?;
    let mut rep = [0u8; 10];
    c.read_exact(&mut rep).await?;
    if rep[1] != 0 {
        emit_fail(em, "http_echo", id, start.elapsed().as_millis() as u64, "socks_reply_err");
        return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("socks5 reply {}", rep[1])));
    }
    // Tunnel "CONNECT" round trip complete: this is the closest analogue to a "ping" for the channel.
    let tunnel_connect_ms = start.elapsed().as_millis() as u64;

    let body: Vec<u8> = (0..bytes).map(|i| (i as u8).wrapping_mul(31)).collect();
    let head = format!("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", bytes);
    c.write_all(head.as_bytes()).await?;
    c.write_all(&body).await?;
    let send_done_ms = start.elapsed().as_millis() as u64;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut ttfb_ms: Option<u64> = None;
    loop {
        let n = c.read(&mut tmp).await?;
        if n == 0 { break; }
        if ttfb_ms.is_none() {
            ttfb_ms = Some(start.elapsed().as_millis() as u64);
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(h_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body_got = buf.len() - (h_end + 4);
            if body_got >= bytes { break; }
        }
    }
    let total_ms = start.elapsed().as_millis() as u64;

    em.emit(Event::new("bench", "request_done")
        .field("workload", "http_echo")
        .field("id", id as i64)
        .field("ok", true)
        .field("socks_connect_ms", socks_connect_ms as i64)
        .field("socks_greet_ms", socks_greet_ms as i64)
        .field("tunnel_connect_ms", tunnel_connect_ms as i64)
        .field("send_done_ms", send_done_ms as i64)
        .field("ttfb_ms", ttfb_ms.unwrap_or(total_ms) as i64)
        .field("total_ms", total_ms as i64)
        .field("latency_ms", total_ms as i64)
        .field("bytes", buf.len() as i64)
        .field("payload_bytes", bytes as i64));
    Ok(())
}

fn emit_fail(em: &Arc<EventEmitter>, workload: &'static str, id: u64, elapsed_ms: u64, stage: &'static str) {
    em.emit(Event::new("bench", "request_done")
        .field("workload", workload)
        .field("id", id as i64)
        .field("ok", false)
        .field("fail_stage", stage)
        .field("total_ms", elapsed_ms as i64)
        .field("latency_ms", elapsed_ms as i64)
        .field("bytes", 0));
}
