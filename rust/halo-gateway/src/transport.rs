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

    /// Send several datagrams, each to its own address, in as few calls to the
    /// system as the transport can manage. `sent[i]` is set to whether
    /// `batch[i]` was sent; a datagram that fails is skipped and the rest
    /// still go (`sent` is as long as `batch`). The default sends them one at
    /// a time with [`Transport::send_to`].
    fn send_batch(&self, batch: &[Outgoing], sent: &mut [bool]) {
        assert_eq!(batch.len(), sent.len());
        for (out, ok) in batch.iter().zip(sent.iter_mut()) {
            *ok = self.send_to(&out.bytes, out.to).is_ok();
        }
    }

    /// Where players send to.
    fn local_addr(&self) -> SocketAddr;
}

/// A datagram and where it goes, for [`Transport::send_batch`].
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub to: SocketAddr,
    pub bytes: Vec<u8>,
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

    #[cfg(target_os = "linux")]
    fn send_batch(&self, batch: &[Outgoing], sent: &mut [bool]) {
        assert_eq!(batch.len(), sent.len());
        sendmmsg_all(&self.socket, batch, sent);
    }

    fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

/// The most datagrams handed to one `sendmmsg` call.
#[cfg(target_os = "linux")]
const MMSG_CHUNK: usize = 64;

/// `sendmmsg` over the whole of `batch`: a call sends a run of datagrams and
/// reports how many, so a datagram that fails is the first of the next call
/// (the call reports its error only when it is the first), is marked unsent,
/// and the rest follow.
#[cfg(target_os = "linux")]
fn sendmmsg_all(socket: &UdpSocket, batch: &[Outgoing], sent: &mut [bool]) {
    use std::os::fd::AsRawFd;
    let fd = socket.as_raw_fd();
    let mut addrs: Vec<(libc::sockaddr_storage, libc::socklen_t)> = batch.iter().map(|o| sockaddr_of(o.to)).collect();
    let mut iovs: Vec<libc::iovec> = batch
        .iter()
        .map(|o| libc::iovec { iov_base: o.bytes.as_ptr() as *mut libc::c_void, iov_len: o.bytes.len() })
        .collect();
    let mut start = 0;
    while start < batch.len() {
        let end = (start + MMSG_CHUNK).min(batch.len());
        let mut hdrs: Vec<libc::mmsghdr> = (start..end)
            .map(|i| {
                // SAFETY: an all-zero mmsghdr is a valid empty one; the pointers set below
                // point into `addrs` and `iovs`, which are not touched until the call returns.
                let mut h: libc::mmsghdr = unsafe { std::mem::zeroed() };
                h.msg_hdr.msg_name = &mut addrs[i].0 as *mut _ as *mut libc::c_void;
                h.msg_hdr.msg_namelen = addrs[i].1;
                h.msg_hdr.msg_iov = &mut iovs[i];
                h.msg_hdr.msg_iovlen = 1;
                h
            })
            .collect();
        // SAFETY: `hdrs` holds `end - start` initialised headers over live buffers.
        let n = unsafe { libc::sendmmsg(fd, hdrs.as_mut_ptr(), hdrs.len() as libc::c_uint, 0) };
        if n > 0 {
            for ok in &mut sent[start..start + n as usize] {
                *ok = true;
            }
            start += n as usize;
        } else {
            let interrupted = n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
            if !interrupted {
                sent[start] = false;
                start += 1;
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn sockaddr_of(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: an all-zero sockaddr_storage is valid, and the casts below view it as the
    // sockaddr_in or sockaddr_in6 that fits in it.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = match addr {
        SocketAddr::V4(a) => {
            let sin = unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in) };
            sin.sin_family = libc::AF_INET as libc::sa_family_t;
            sin.sin_port = a.port().to_be();
            sin.sin_addr = libc::in_addr { s_addr: u32::from_ne_bytes(a.ip().octets()) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(a) => {
            let sin6 = unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in6) };
            sin6.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sin6.sin6_port = a.port().to_be();
            sin6.sin6_flowinfo = a.flowinfo();
            sin6.sin6_addr = libc::in6_addr { s6_addr: a.ip().octets() };
            sin6.sin6_scope_id = a.scope_id();
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, len as libc::socklen_t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver() -> UdpSocket {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        socket
    }

    fn payload(i: usize, len: usize) -> Vec<u8> {
        (0..len).map(|b| (i * 31 + b) as u8).collect()
    }

    /// A batch to several addresses, several datagrams to each, in odd sizes, with
    /// one that cannot be sent (over what a UDP datagram can carry) in the middle.
    fn batch_for(receivers: &[UdpSocket], bad_at: Option<usize>) -> Vec<Outgoing> {
        (0..200)
            .map(|i| {
                let to = receivers[i % receivers.len()].local_addr().unwrap();
                let len = if Some(i) == bad_at { 70_000 } else { 1 + (i * 37) % 1200 };
                Outgoing { to, bytes: payload(i, len) }
            })
            .collect()
    }

    /// What each receiver got, in order, checked against what was sent to it.
    fn assert_arrived(receivers: &[UdpSocket], batch: &[Outgoing], sent: &[bool]) {
        let mut buf = vec![0u8; 2000];
        for (r, socket) in receivers.iter().enumerate() {
            for (i, out) in batch.iter().enumerate().filter(|(i, _)| i % receivers.len() == r && sent[*i]) {
                let (len, _) = socket.recv_from(&mut buf).unwrap_or_else(|e| panic!("datagram {i} never came: {e}"));
                assert_eq!(&buf[..len], &out.bytes[..], "datagram {i} came whole to the right address");
            }
            // and nothing else
            socket.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
            assert!(socket.recv_from(&mut buf).is_err(), "receiver {r} got a datagram that was not sent to it");
        }
    }

    #[test]
    fn every_datagram_of_a_batch_arrives_whole_and_at_its_own_address() {
        let transport = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let receivers: Vec<UdpSocket> = (0..5).map(|_| receiver()).collect();
        let batch = batch_for(&receivers, None);
        let mut sent = vec![false; batch.len()];
        transport.send_batch(&batch, &mut sent);
        assert!(sent.iter().all(|ok| *ok));
        assert_arrived(&receivers, &batch, &sent);
    }

    #[test]
    fn a_datagram_that_fails_inside_a_batch_is_reported_and_the_others_still_arrive() {
        let transport = UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let receivers: Vec<UdpSocket> = (0..5).map(|_| receiver()).collect();
        // one in the middle of a chunk, one first, one last
        for bad in [100, 0, 199] {
            let batch = batch_for(&receivers, Some(bad));
            let mut sent = vec![false; batch.len()];
            transport.send_batch(&batch, &mut sent);
            assert_eq!(sent.iter().filter(|ok| !**ok).count(), 1);
            assert!(!sent[bad], "the oversized datagram {bad} is the one reported unsent");
            assert_arrived(&receivers, &batch, &sent);
        }
    }

    #[test]
    fn the_default_batch_sends_one_at_a_time_and_reports_the_failures() {
        struct OneAtATime(UdpSocket);
        impl Transport for OneAtATime {
            fn recv(&self, _: &mut [u8]) -> io::Result<Option<(usize, SocketAddr)>> {
                Ok(None)
            }
            fn send_to(&self, datagram: &[u8], to: SocketAddr) -> io::Result<()> {
                self.0.send_to(datagram, to).map(|_| ())
            }
            fn local_addr(&self) -> SocketAddr {
                self.0.local_addr().unwrap()
            }
        }
        let transport = OneAtATime(UdpSocket::bind("127.0.0.1:0").unwrap());
        let receivers: Vec<UdpSocket> = (0..3).map(|_| receiver()).collect();
        let batch = batch_for(&receivers, Some(50));
        let mut sent = vec![false; batch.len()];
        transport.send_batch(&batch, &mut sent);
        assert_eq!(sent.iter().filter(|ok| !**ok).count(), 1);
        assert!(!sent[50]);
        assert_arrived(&receivers, &batch, &sent);
    }
}
