//! The seam between the gateway and its players: one interface that carries
//! the per-tick datagrams, so that UDP can later be swapped for another
//! transport (for example an unreliable channel SpacetimeDB may ship) without
//! touching the input board, the planner or the sending threads.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

/// An unreliable datagram transport. Datagrams may be lost, are never
/// duplicated or cut, and are at most `halo_wire::datagram::MAX_DATAGRAM`
/// bytes. Shared between the receiving thread and the sending threads.
pub trait Transport: Send + Sync + 'static {
    /// The next datagram from a player, copied into `buf`. Waits a while (a
    /// transport's choice, well under a second) and then returns `Ok(None)`
    /// so that the caller can check whether it should stop.
    fn recv(&self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>>;

    /// Send one datagram to a player. Callable from several threads at once.
    fn send_to(&self, datagram: &[u8], to: SocketAddr) -> io::Result<()>;

    /// Where players send to.
    fn local_addr(&self) -> SocketAddr;
}

/// [`Transport`] over one UDP socket.
pub struct UdpTransport {
    socket: UdpSocket,
    local: SocketAddr,
}

impl UdpTransport {
    pub fn bind(addr: SocketAddr) -> io::Result<UdpTransport> {
        let socket = UdpSocket::bind(addr)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let local = socket.local_addr()?;
        Ok(UdpTransport { socket, local })
    }
}

impl Transport for UdpTransport {
    fn recv(&self, buf: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
        match self.socket.recv_from(buf) {
            Ok(got) => Ok(Some(got)),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(None),
            // a datagram answered by an ICMP "unreachable" shows up as an error on the next call
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn send_to(&self, datagram: &[u8], to: SocketAddr) -> io::Result<()> {
        self.socket.send_to(datagram, to).map(|_| ())
    }

    fn local_addr(&self) -> SocketAddr {
        self.local
    }
}
