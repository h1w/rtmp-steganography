pub mod adapter;
pub mod egress;
pub mod framing;
pub mod kcp;
pub mod listener;
pub mod metrics;
pub mod mux;
pub mod socks5;
pub mod testchannel;

use std::sync::Arc;
use std::net::SocketAddr;

use crate::tunnel::adapter::DatagramChannel;
use crate::tunnel::kcp::{KcpSession, Profile};
use crate::tunnel::metrics::EventEmitter;
use crate::tunnel::mux::MuxSession;

pub struct Tunnel {
    pub emitter: Arc<EventEmitter>,
}

impl Tunnel {
    /// Start the full tunnel stack: KCP + yamux + SOCKS5 listener + egress worker.
    ///
    /// The PEER_ID env var decides yamux client/server role:
    /// peers with PEER_ID < "B" are yamux Client, others are Server.
    pub async fn start(
        channel: Arc<dyn DatagramChannel>,
        profile: Profile,
        socks_bind: SocketAddr,
        emitter: Arc<EventEmitter>,
    ) -> std::io::Result<Self> {
        // 1. KCP over the datagram channel
        let kcp = Arc::new(KcpSession::start(channel, profile, Arc::clone(&emitter)));
        let stream = Arc::clone(&kcp).stream();

        // 2. yamux mux over the kcp stream. Mode chosen by PEER_ID.
        let peer_id = std::env::var("PEER_ID").unwrap_or_else(|_| "A".into());
        let mode_client = peer_id.as_str() < "B";
        let mux: MuxSession = if mode_client {
            MuxSession::client(stream)
        } else {
            MuxSession::server(stream)
        };

        // 3. Spawn SOCKS5 listener (inbound TCP) and egress worker (outbound streams)
        let em_l = Arc::clone(&emitter);
        let em_e = Arc::clone(&emitter);
        let mux_l = mux.clone();
        let mux_e = mux.clone();
        tokio::spawn(async move {
            let lst = crate::tunnel::listener::Listener::new(socks_bind, mux_l, em_l);
            let _ = lst.run().await;
        });
        tokio::spawn(async move {
            let eg = crate::tunnel::egress::Egress::new(mux_e, em_e);
            let _ = eg.run().await;
        });

        Ok(Self { emitter })
    }
}
