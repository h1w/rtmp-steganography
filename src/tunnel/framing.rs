//! First-frame CONNECT message sent on every opened yamux stream.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFrame {
    pub host: String,
    pub port: u16,
}

impl ConnectFrame {
    pub fn encode(&self) -> Vec<u8> {
        let host = self.host.as_bytes();
        assert!(host.len() <= u8::MAX as usize, "host too long");
        let mut v = Vec::with_capacity(4 + host.len());
        v.push(1); // version
        v.push(host.len() as u8);
        v.extend_from_slice(host);
        v.extend_from_slice(&self.port.to_be_bytes());
        v
    }

    pub async fn write_to<W: AsyncWrite + Unpin>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.encode()).await
    }

    pub async fn read_from<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Self> {
        let mut ver = [0u8; 1];
        r.read_exact(&mut ver).await?;
        if ver[0] != 1 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "connect version"));
        }
        let mut hl = [0u8; 1];
        r.read_exact(&mut hl).await?;
        let mut host = vec![0u8; hl[0] as usize];
        r.read_exact(&mut host).await?;
        let mut port = [0u8; 2];
        r.read_exact(&mut port).await?;
        Ok(Self {
            host: String::from_utf8(host).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "host utf8"))?,
            port: u16::from_be_bytes(port),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = duplex(256);
        let f = ConnectFrame { host: "example.com".into(), port: 443 };
        f.write_to(&mut a).await.unwrap();
        let g = ConnectFrame::read_from(&mut b).await.unwrap();
        assert_eq!(f, g);
    }

    #[test]
    fn encode_exact_bytes() {
        let f = ConnectFrame { host: "a".into(), port: 80 };
        assert_eq!(f.encode(), vec![1, 1, b'a', 0, 80]);
    }
}
