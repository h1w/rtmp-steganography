use anyhow::Result;

#[derive(Clone, Debug)]
pub struct PeerConfig {
    pub my_rtmp_url: String,
    pub my_stream_key: String,
    pub their_vk_channel: String,
    pub their_stream_name: String,
}

pub fn load_peer() -> Result<PeerConfig> {
    Ok(PeerConfig {
        my_rtmp_url: std::env::var("peer_my_rtmp_url").unwrap_or_default(),
        my_stream_key: std::env::var("peer_my_stream_key").unwrap_or_default(),
        their_vk_channel: std::env::var("peer_their_vk_channel").unwrap_or_default(),
        their_stream_name: std::env::var("peer_their_stream_name").unwrap_or_default(),
    })
}
