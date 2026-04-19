//! SOCKS5 inbound listener. Per accepted TCP connection:
//! 1. Negotiate SOCKS5 handshake.
//! 2. Open a yamux stream on the tunnel.
//! 3. Write the internal CONNECT frame.
//! 4. Await 1-byte egress status.
//! 5. Reply to SOCKS5 client and bidirectional-pump bytes.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::compat::FuturesAsyncReadCompatExt;

use crate::tunnel::framing::ConnectFrame;
use crate::tunnel::metrics::{Event, EventEmitter};
use crate::tunnel::mux::MuxSession;
use crate::tunnel::socks5::{self, Socks5Reply};

pub struct Listener {
    addr: SocketAddr,
    mux: MuxSession,
    emitter: Arc<EventEmitter>,
}

impl Listener {
    pub fn new(addr: SocketAddr, mux: MuxSession, emitter: Arc<EventEmitter>) -> Self {
        Self { addr, mux, emitter }
    }

    pub async fn run(self) -> std::io::Result<()> {
        let lst = TcpListener::bind(self.addr).await?;
        loop {
            let (client, _peer) = lst.accept().await?;
            let mux = self.mux.clone();
            let em = Arc::clone(&self.emitter);
            tokio::spawn(async move {
                handle_one(client, mux, em).await;
            });
        }
    }
}

async fn handle_one(mut client: tokio::net::TcpStream, mux: MuxSession, em: Arc<EventEmitter>) {
    // 1. SOCKS5 handshake
    let req = match socks5::negotiate(&mut client).await {
        Ok(r) => r,
        Err(_) => {
            em.emit(Event::new("socks5", "error").field("stage", "negotiate"));
            return;
        }
    };
    em.emit(Event::new("socks5", "connect")
        .field("host", req.host.clone())
        .field("port", req.port as i64));

    // 2. Open yamux stream
    let stream = match mux.open_stream().await {
        Ok(s) => s,
        Err(_) => {
            socks5::reply(&mut client, Socks5Reply::NetUnreachable).await.ok();
            em.emit(Event::new("socks5", "reply").field("code", Socks5Reply::NetUnreachable as u8 as i64));
            return;
        }
    };

    // yamux::Stream is futures-io; adapt to tokio via compat
    let mut stream = stream.compat();

    // 3. Send CONNECT frame on the tunnel stream
    let cf = ConnectFrame { host: req.host.clone(), port: req.port };
    if cf.write_to(&mut stream).await.is_err() {
        socks5::reply(&mut client, Socks5Reply::NetUnreachable).await.ok();
        return;
    }

    // 4. Read 1-byte status from egress
    use tokio::io::AsyncReadExt;
    let mut b = [0u8; 1];
    let status = match stream.read_exact(&mut b).await {
        Ok(_) => b[0],
        Err(_) => 0x03, // treat as net unreachable
    };
    let reply_code = match status {
        0 => Socks5Reply::Ok,
        1 => Socks5Reply::HostUnreachable,
        2 => Socks5Reply::ConnRefused,
        3 => Socks5Reply::TtlExpired,
        _ => Socks5Reply::NetUnreachable,
    };
    socks5::reply(&mut client, reply_code).await.ok();
    em.emit(Event::new("socks5", "reply").field("code", status as i64));
    if status != 0 { return; }

    // 5. Bidirectional pump
    let (mut cr, mut cw) = client.into_split();
    let (mut sr, mut sw) = tokio::io::split(stream);
    let a = tokio::io::copy(&mut cr, &mut sw);
    let b_pump = tokio::io::copy(&mut sr, &mut cw);
    let _ = tokio::try_join!(a, b_pump);
}
