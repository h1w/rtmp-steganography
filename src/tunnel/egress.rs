//! Egress: accept yamux streams, read CONNECT frame, dial TCP, pump bytes.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpStream;
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::tunnel::framing::ConnectFrame;
use crate::tunnel::metrics::{Event, EventEmitter};
use crate::tunnel::mux::MuxSession;

pub struct Egress {
    mux: MuxSession,
    emitter: Arc<EventEmitter>,
}

impl Egress {
    pub fn new(mux: MuxSession, emitter: Arc<EventEmitter>) -> Self {
        Self { mux, emitter }
    }

    pub async fn run(self) -> std::io::Result<()> {
        loop {
            let stream = match self.mux.accept_stream().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let em = Arc::clone(&self.emitter);
            tokio::spawn(async move { handle_one(stream, em).await; });
        }
        Ok(())
    }
}

async fn handle_one(stream: yamux::Stream, em: Arc<EventEmitter>) {
    // Bridge futures-io -> tokio-io
    let mut stream = stream.compat();

    // 1. Read CONNECT frame
    let cf = match ConnectFrame::read_from(&mut stream).await {
        Ok(f) => f,
        Err(_) => {
            em.emit(Event::new("egress", "bad_frame"));
            return;
        }
    };

    let target = format!("{}:{}", cf.host, cf.port);
    em.emit(Event::new("egress", "dial").field("target", target.clone()));

    // 2. Dial target with timeout
    let dial = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect(&target)).await;
    let (tcp, status): (Option<TcpStream>, u8) = match dial {
        Ok(Ok(t)) => (Some(t), 0),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => (None, 2),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::TimedOut => (None, 3),
        Ok(Err(_)) => (None, 1),
        Err(_) => (None, 3),
    };

    // 3. Write status byte back
    use tokio::io::AsyncWriteExt;
    if stream.write_all(&[status]).await.is_err() {
        em.emit(Event::new("egress", "status_write_failed"));
        return;
    }
    let _ = stream.flush().await;
    em.emit(Event::new("egress", "status").field("code", status as i64));

    let Some(tcp) = tcp else { return; };

    // 4. Bidirectional pump: tunnel stream <-> target TCP
    let (mut tr, mut tw) = tcp.into_split();
    let (mut sr, mut sw) = tokio::io::split(stream);
    let a = tokio::io::copy(&mut sr, &mut tw);
    let b = tokio::io::copy(&mut tr, &mut sw);
    let _ = tokio::try_join!(a, b);
}
