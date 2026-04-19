use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{emit_done, WorkloadResult};
use crate::tunnel::metrics::EventEmitter;

pub async fn run(
    socks: SocketAddr,
    dns_host: &str,
    dns_port: u16,
    names: &[String],
    em: Arc<EventEmitter>,
) {
    for (i, name) in names.iter().enumerate() {
        let start = Instant::now();
        let r = lookup(socks, dns_host, dns_port, name).await;
        let res = match r {
            Ok(b) => WorkloadResult { ok: true, latency_ms: start.elapsed().as_millis() as u64, bytes: b },
            Err(_) => WorkloadResult { ok: false, latency_ms: start.elapsed().as_millis() as u64, bytes: 0 },
        };
        emit_done(&em, "dns", i as u64, &res);
    }
}

async fn lookup(socks: SocketAddr, host: &str, port: u16, name: &str) -> std::io::Result<u64> {
    let mut c = TcpStream::connect(socks).await?;
    c.write_all(&[5, 1, 0]).await?;
    let mut g = [0u8; 2];
    c.read_exact(&mut g).await?;
    if g != [5, 0] {
        return Err(std::io::ErrorKind::Other.into());
    }
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    c.write_all(&req).await?;
    let mut rep = [0u8; 10];
    c.read_exact(&mut rep).await?;
    if rep[1] != 0 {
        return Err(std::io::ErrorKind::Other.into());
    }

    // Build a minimal DNS A query for `name`
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&0x1234u16.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD=1
    q.extend_from_slice(&1u16.to_be_bytes());
    q.extend_from_slice(&0u16.to_be_bytes());
    q.extend_from_slice(&0u16.to_be_bytes());
    q.extend_from_slice(&0u16.to_be_bytes());
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&1u16.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());

    c.write_all(&(q.len() as u16).to_be_bytes()).await?;
    c.write_all(&q).await?;

    let mut lenb = [0u8; 2];
    c.read_exact(&mut lenb).await?;
    let n = u16::from_be_bytes(lenb) as usize;
    let mut resp = vec![0u8; n];
    c.read_exact(&mut resp).await?;
    Ok(n as u64)
}
