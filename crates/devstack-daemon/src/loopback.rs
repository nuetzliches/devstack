//! A connection to a service's port on this machine.

use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::net::TcpStream;

/// Connect to `port` on the IPv4 and the IPv6 loopback at once and keep the
/// first that answers (IPv4 when both are ready). A dev server bound to
/// `localhost` may listen on `::1` only (Node does, on Windows), and there a
/// refused connect is not immediate: Windows retries the SYN for about 2 s
/// before it reports the refusal, so trying IPv4 first cost every new
/// connection those 2 s (#418). The error is IPv4's when neither answers.
pub(crate) async fn connect(port: u16) -> std::io::Result<TcpStream> {
    let v4 = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    let v6 = TcpStream::connect((Ipv6Addr::LOCALHOST, port));
    tokio::pin!(v4, v6);
    tokio::select! {
        biased;
        r = &mut v4 => match r {
            Ok(s) => Ok(s),
            Err(e) => v6.await.map_err(|_| e),
        },
        r = &mut v6 => match r {
            Ok(s) => Ok(s),
            Err(_) => v4.await,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn reaches_a_server_on_ipv6_loopback_only() {
        let Ok(l) = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
            return; // no IPv6 on this host
        };
        let port = l.local_addr().unwrap().port();
        if std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
            return; // the IPv4 port is someone else's
        }
        let started = std::time::Instant::now();
        let s = rt().block_on(connect(port)).expect("connects over ::1");
        assert!(s.peer_addr().unwrap().is_ipv6());
        // Without the race, Windows took about 2 s to refuse IPv4 first.
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn reaches_a_server_on_ipv4_loopback() {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let s = rt()
            .block_on(connect(port))
            .expect("connects over 127.0.0.1");
        assert!(s.peer_addr().unwrap().is_ipv4());
    }

    #[test]
    fn nothing_listening_is_an_error() {
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(rt().block_on(connect(port)).is_err());
    }
}
