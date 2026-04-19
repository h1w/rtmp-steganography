use std::sync::Arc;
use rtmp_steganography::tunnel::{
    adapter,
    kcp::{KcpSession, Profile},
    metrics::EventEmitter,
    testchannel,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

struct MemAdapter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    rx: Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>,
    mtu: usize,
}

#[async_trait::async_trait]
impl adapter::DatagramChannel for MemAdapter {
    async fn send(&self, buf: Vec<u8>) -> std::io::Result<()> {
        self.tx
            .send(buf)
            .await
            .map_err(|_| std::io::ErrorKind::BrokenPipe.into())
    }
    async fn recv(&self) -> std::io::Result<Vec<u8>> {
        self.rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| std::io::ErrorKind::BrokenPipe.into())
    }
    fn max_payload(&self) -> usize {
        self.mtu
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kcp_roundtrip_over_lossy_mem_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let em_a = Arc::new(EventEmitter::new(tmp.path(), "A").unwrap());
    let em_b = Arc::new(EventEmitter::new(tmp.path(), "B").unwrap());

    let (a, b) = testchannel::pair(testchannel::Config {
        loss_pct: 0,
        latency_ms: 0,
        jitter_ms: 0,
        buffer: 512,
    });

    let (a_tx, a_rx) = testchannel::with_simulation(
        a,
        testchannel::Config {
            loss_pct: 10,
            latency_ms: 50,
            jitter_ms: 5,
            buffer: 512,
        },
    );
    let (b_tx, b_rx) = testchannel::with_simulation(
        b,
        testchannel::Config {
            loss_pct: 10,
            latency_ms: 50,
            jitter_ms: 5,
            buffer: 512,
        },
    );

    let ad_a: Arc<dyn adapter::DatagramChannel> = Arc::new(MemAdapter {
        tx: a_tx,
        rx: Mutex::new(a_rx),
        mtu: 512,
    });
    let ad_b: Arc<dyn adapter::DatagramChannel> = Arc::new(MemAdapter {
        tx: b_tx,
        rx: Mutex::new(b_rx),
        mtu: 512,
    });

    let sess_a = Arc::new(KcpSession::start(ad_a, Profile::Latency, em_a));
    let sess_b = Arc::new(KcpSession::start(ad_b, Profile::Latency, em_b));

    let mut sa = sess_a.stream();
    let mut sb = sess_b.stream();

    sa.write_all(b"hello from a").await.unwrap();
    sa.flush().await.unwrap();

    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        sb.read(&mut buf),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(&buf[..n], b"hello from a");
}
