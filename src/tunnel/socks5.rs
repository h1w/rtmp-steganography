//! Minimal SOCKS5 handshake: auth=none, CONNECT only. RFC 1928.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Socks5Request {
    pub host: String,
    pub port: u16,
}

#[repr(u8)]
pub enum Socks5Reply {
    Ok = 0x00,
    GeneralFailure = 0x01,
    ConnNotAllowed = 0x02,
    NetUnreachable = 0x03,
    HostUnreachable = 0x04,
    ConnRefused = 0x05,
    TtlExpired = 0x06,
    CmdNotSupported = 0x07,
    AddrTypeNotSupported = 0x08,
}

pub async fn negotiate<S>(s: &mut S) -> std::io::Result<Socks5Request>
where S: AsyncRead + AsyncWrite + Unpin {
    // Greeting: [ver=5][nmethods][methods...]
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(io_err("not socks5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    s.read_exact(&mut methods).await?;
    if !methods.iter().any(|&m| m == 0x00) {
        s.write_all(&[5, 0xFF]).await?;
        return Err(io_err("no no-auth method"));
    }
    s.write_all(&[5, 0x00]).await?;

    // Request: [ver=5][cmd][rsv][atyp][addr][port]
    let mut hdr = [0u8; 4];
    s.read_exact(&mut hdr).await?;
    if hdr[0] != 5 { return Err(io_err("not socks5 req")); }
    if hdr[1] != 0x01 {
        reply(s, Socks5Reply::CmdNotSupported).await.ok();
        return Err(io_err("command not CONNECT"));
    }
    let host = match hdr[3] {
        0x01 => {
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await?;
            std::net::Ipv4Addr::from(b).to_string()
        }
        0x03 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut h = vec![0u8; l[0] as usize];
            s.read_exact(&mut h).await?;
            String::from_utf8(h).map_err(|_| io_err("bad host"))?
        }
        0x04 => {
            let mut b = [0u8; 16];
            s.read_exact(&mut b).await?;
            std::net::Ipv6Addr::from(b).to_string()
        }
        _ => {
            reply(s, Socks5Reply::AddrTypeNotSupported).await.ok();
            return Err(io_err("atyp"));
        }
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    Ok(Socks5Request { host, port: u16::from_be_bytes(port) })
}

pub async fn reply<S: AsyncWrite + Unpin>(s: &mut S, code: Socks5Reply) -> std::io::Result<()> {
    // Minimal reply: bind address 0.0.0.0:0
    let resp = [5u8, code as u8, 0, 0x01, 0,0,0,0, 0,0];
    s.write_all(&resp).await
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn greets_and_parses_connect_domain() {
        let (mut client, mut server) = duplex(256);
        let server_task = tokio::spawn(async move { negotiate(&mut server).await });

        // Greeting: ver=5, nmethods=1, method=0
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut greet_resp = [0u8; 2];
        client.read_exact(&mut greet_resp).await.unwrap();
        assert_eq!(greet_resp, [5, 0]);

        // Request: CONNECT example.com:443
        let host = b"example.com";
        let mut req = vec![5u8, 1, 0, 3, host.len() as u8];
        req.extend_from_slice(host);
        req.extend_from_slice(&443u16.to_be_bytes());
        client.write_all(&req).await.unwrap();

        let parsed = server_task.await.unwrap().unwrap();
        assert_eq!(parsed, Socks5Request { host: "example.com".into(), port: 443 });
    }
}
