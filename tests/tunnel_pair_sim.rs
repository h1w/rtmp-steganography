use std::sync::Arc;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use rtmp_steganography::tunnel::{
    adapter::DatagramChannel,
    kcp::Profile,
    metrics::EventEmitter,
    testchannel,
    Tunnel,
};

struct MemAdapter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    mtu: usize,
}

#[async_trait::async_trait]
impl DatagramChannel for MemAdapter {
    async fn send(&self, b: Vec<u8>) -> std::io::Result<()> {
        self.tx.send(b).await.map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.rx.lock().await.recv().await.ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize { self.mtu }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socks5_echo_through_tunnel_pair_over_lossy_mem() {
    let tmp = tempfile::tempdir().unwrap();
    let em_a = Arc::new(EventEmitter::new(tmp.path(), "A").unwrap());
    let em_b = Arc::new(EventEmitter::new(tmp.path(), "B").unwrap());

    let (a, b) = testchannel::pair(testchannel::Config { loss_pct: 0, latency_ms: 0, jitter_ms: 0, buffer: 1024 });
    let (atx, arx) = testchannel::with_simulation(a, testchannel::Config { loss_pct: 10, latency_ms: 30, jitter_ms: 5, buffer: 1024 });
    let (btx, brx) = testchannel::with_simulation(b, testchannel::Config { loss_pct: 10, latency_ms: 30, jitter_ms: 5, buffer: 1024 });

    let ch_a: Arc<dyn DatagramChannel> = Arc::new(MemAdapter { tx: atx, rx: Mutex::new(arx), mtu: 512 });
    let ch_b: Arc<dyn DatagramChannel> = Arc::new(MemAdapter { tx: btx, rx: Mutex::new(brx), mtu: 512 });

    // Echo server — egress target on peer B's side
    let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => { let _ = s.write_all(&buf[..n]).await; }
                    }
                }
            });
        }
    });

    let socks_a: SocketAddr = "127.0.0.1:11080".parse().unwrap();
    let socks_b: SocketAddr = "127.0.0.1:11081".parse().unwrap();

    std::env::set_var("PEER_ID", "A");
    let _ta = Tunnel::start(ch_a, Profile::Latency, socks_a, em_a).await.unwrap();
    std::env::set_var("PEER_ID", "B");
    let _tb = Tunnel::start(ch_b, Profile::Latency, socks_b, em_b).await.unwrap();

    // Wait for listeners to bind and tunnel warmup
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Drive a SOCKS5 client from peer A → target = echo_addr on peer B's host
    let mut c = TcpStream::connect(socks_a).await.unwrap();
    c.write_all(&[5, 1, 0]).await.unwrap();
    let mut g = [0u8; 2]; c.read_exact(&mut g).await.unwrap();
    assert_eq!(g, [5, 0]);
    let host = format!("{}", echo_addr.ip());
    let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&echo_addr.port().to_be_bytes());
    c.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10]; c.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0, "SOCKS5 reply ok");

    c.write_all(b"the quick brown fox").await.unwrap();
    let mut buf = [0u8; 32];
    let n = tokio::time::timeout(std::time::Duration::from_secs(20), c.read(&mut buf)).await.unwrap().unwrap();
    assert_eq!(&buf[..n], b"the quick brown fox");
}
