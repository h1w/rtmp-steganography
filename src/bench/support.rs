//! Embedded support services used by the bench harness.
//! Run inside the peer process so egress targets are deterministic.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Start an embedded HTTP echo server. Reads one request, replies with the body.
/// Loops until `running` goes false. Returns after bind.
pub async fn spawn_http_echo(bind: SocketAddr, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((mut s, _)) = lst.accept().await else { continue };
            tokio::spawn(async move {
                let mut buf = Vec::with_capacity(4096);
                let mut tmp = [0u8; 4096];
                // Read headers until \r\n\r\n
                loop {
                    let n = match s.read(&mut tmp).await { Ok(0) | Err(_) => return, Ok(n) => n };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
                }
                let body_start = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    Some(p) => p + 4,
                    None => return,
                };
                let headers = &buf[..body_start];
                let cl = parse_content_length(headers).unwrap_or(0);
                while buf.len() - body_start < cl {
                    let n = match s.read(&mut tmp).await { Ok(0) | Err(_) => break, Ok(n) => n };
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body_end = (body_start + cl).min(buf.len());
                let body = &buf[body_start..body_end];
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.write_all(body).await;
            });
        }
    });
    Ok(())
}

fn parse_content_length(hdr: &[u8]) -> Option<usize> {
    let s = std::str::from_utf8(hdr).ok()?;
    for line in s.split("\r\n") {
        let mut it = line.splitn(2, ':');
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            if k.eq_ignore_ascii_case("content-length") {
                return v.trim().parse().ok();
            }
        }
    }
    None
}

/// Raw TCP sink — accepts data, counts bytes, discards. One-way throughput
/// endpoint for high-latency links where round-trip echo is too slow. When
/// the client closes the stream (FIN), the server emits a per-connection
/// eprintln with total bytes and duration. Pair with `--one-way` on bench.
pub async fn spawn_raw_sink(bind: SocketAddr, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((mut s, addr)) = lst.accept().await else { continue };
            tokio::spawn(async move {
                let start = std::time::Instant::now();
                let mut buf = vec![0u8; 16384];
                let mut total: u64 = 0;
                loop {
                    let n = match s.read(&mut buf).await {
                        Ok(0) => break,
                        Err(_) => break,
                        Ok(n) => n,
                    };
                    total += n as u64;
                }
                let dur = start.elapsed();
                eprintln!("[bench/sink] {} closed: bytes_received={} duration_ms={} goodput_kbps={:.2}",
                    addr, total, dur.as_millis(),
                    (total as f64 * 8.0 / 1000.0) / dur.as_secs_f64().max(1e-6));
            });
        }
    });
    Ok(())
}

/// Raw TCP echo server — streams bytes back as-is, no HTTP framing. Used by
/// the throughput workload to measure sustained bidirectional goodput with
/// zero request/response overhead and no external tools.
pub async fn spawn_raw_echo(bind: SocketAddr, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((s, _)) = lst.accept().await else { continue };
            tokio::spawn(async move {
                let (mut r, mut w) = s.into_split();
                let mut buf = vec![0u8; 16384];
                loop {
                    let n = match r.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if w.write_all(&buf[..n]).await.is_err() { return; }
                }
            });
        }
    });
    Ok(())
}

/// Minimal TCP-DNS responder. Accepts RFC 1035 length-prefixed DNS queries,
/// echoes the request back with the QR/flags bits flipped to mark it a response.
/// NOT a real resolver — only produces well-formed reply envelopes. Sufficient
/// for measuring tunnel behavior (which is what the bench cares about).
///
/// Fixture file is read once at startup; its content is never sent over the wire,
/// it just exists to keep the naming list co-located with the responder.
pub async fn spawn_tcp_dns(
    bind: SocketAddr,
    fixture_path: &Path,
    running: Arc<AtomicBool>,
) -> std::io::Result<()> {
    // Eagerly read fixture so a missing/unreadable file is reported at bind time.
    let _names = std::fs::read_to_string(fixture_path)?;
    let lst = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        while running.load(Ordering::SeqCst) {
            let Ok((mut s, _)) = lst.accept().await else { continue };
            tokio::spawn(async move {
                loop {
                    let mut lenb = [0u8; 2];
                    if s.read_exact(&mut lenb).await.is_err() { return; }
                    let n = u16::from_be_bytes(lenb) as usize;
                    if n == 0 || n > 4096 { return; }
                    let mut req = vec![0u8; n];
                    if s.read_exact(&mut req).await.is_err() { return; }
                    let mut resp = req.clone();
                    if resp.len() >= 4 {
                        resp[2] = 0x81; // QR=1, Opcode=0, AA=0, TC=0, RD=1
                        resp[3] = 0x80; // RA=1, no error
                    }
                    if s.write_all(&(resp.len() as u16).to_be_bytes()).await.is_err() { return; }
                    if s.write_all(&resp).await.is_err() { return; }
                }
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[tokio::test(flavor = "multi_thread")]
    async fn http_echo_returns_body() {
        let running = Arc::new(AtomicBool::new(true));
        // Bind to ephemeral port by probing, then reusing the address.
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = probe.local_addr().unwrap();
        drop(probe);
        spawn_http_echo(bound, Arc::clone(&running)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut c = TcpStream::connect(bound).await.unwrap();
        let body = b"hello-echo";
        let req = format!("POST /echo HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n", body.len());
        c.write_all(req.as_bytes()).await.unwrap();
        c.write_all(body).await.unwrap();

        let mut buf = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.read_to_end(&mut buf)).await;
        let resp = String::from_utf8_lossy(&buf);
        assert!(resp.contains("hello-echo"), "resp={}", resp);
        running.store(false, Ordering::SeqCst);
    }
}
