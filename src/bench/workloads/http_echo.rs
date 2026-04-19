use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(
    socks: SocketAddr,
    target_host: &str,
    target_port: u16,
    payload_bytes: usize,
    iterations: usize,
    em: Arc<EventEmitter>,
) {
    for i in 0..iterations {
        let start = Instant::now();
        let r = one_roundtrip(socks, target_host, target_port, payload_bytes).await;
        let result = match r {
            Ok(b) => WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes: b },
            Err(_) => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
        };
        emit_done(&em, "http_echo", i as u64, &result);
    }
}

async fn one_roundtrip(
    socks: SocketAddr,
    host: &str,
    port: u16,
    bytes: usize,
) -> std::io::Result<u64> {
    let mut c = TcpStream::connect(socks).await?;
    c.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2];
    c.read_exact(&mut g).await?;
    if g != [5, 0] {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "bad socks5 greet"));
    }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await?;
    let mut rep = [0u8; 10];
    c.read_exact(&mut rep).await?;
    if rep[1] != 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("socks5 reply {}", rep[1])));
    }

    let body: Vec<u8> = (0..bytes).map(|i| (i as u8).wrapping_mul(31)).collect();
    let head = format!("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", bytes);
    c.write_all(head.as_bytes()).await?;
    c.write_all(&body).await?;

    // Read full response body (after headers)
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = c.read(&mut tmp).await?;
        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
        // Once the response body is complete (headers + CL bytes), exit.
        if let Some(h_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let body_got = buf.len() - (h_end + 4);
            if body_got >= bytes { break; }
        }
    }
    Ok(buf.len() as u64)
}
