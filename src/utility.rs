use std::net::{TcpListener, UdpSocket};

/// Returns a valid, open TCP port by binding to port 0 and reading the assigned port.
pub fn get_open_tcp_port() -> u16 {
    let listener = TcpListener::bind("0.0.0.0:0").expect("Failed to bind TCP socket");
    listener.local_addr().unwrap().port()
}

/// Returns a valid, open UDP port by binding to port 0 and reading the assigned port.
pub fn get_open_udp_port() -> u16 {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("Failed to bind UDP socket");
    socket.local_addr().unwrap().port()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_open_tcp_port() {
        let port = get_open_tcp_port();
        assert!(port > 0);
    }

    #[test]
    fn test_get_open_udp_port() {
        let port = get_open_udp_port();
        assert!(port > 0);
    }
}
