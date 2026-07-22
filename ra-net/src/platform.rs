//! §4.7 quarantine: every socket-option / broadcast quirk in `ra-net` lives
//! here, so any future `#[cfg(target_os)]` is confined to this one module
//! (the crate rule: "a `#[cfg(target_os)]` anywhere else fails review").
//!
//! As of M8-B **no platform `cfg` is needed at all**: everything the LAN
//! stage requires — `bind`, `set_nonblocking`, `set_broadcast`, `send_to`,
//! `recv_from` — is portable `std::net` on Windows/macOS/Linux. The module
//! still exists as the designated landing pad, and the socket constructors
//! below are the only places sockets are configured, so a future quirk (e.g.
//! `SO_REUSEADDR` for multiple joiners on one machine, which `std` does not
//! expose) has exactly one home.
//!
//! The original's LAN flow for comparison: IPX broadcast discovery
//! (IPXCONN.CPP) later bridged to UDP broadcast on port 1234 (UDPADDR /
//! WSPUDP.CPP `SetSocketOption(SO_BROADCAST)`); our discovery below is the
//! modern equivalent of the same fixed-port broadcast scheme.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

/// The fixed UDP port session announcements are broadcast to (M8-B P2).
/// `0x5241` = ASCII `"RA"` = 21057 — outside the well-known range and
/// unclaimed by anything common. **Tests never bind this**: CI safety demands
/// OS-assigned ports, so tests inject explicit targets/ports instead.
pub const DISCOVERY_PORT: u16 = 0x5241;

/// Where a host's announcements are sent when no explicit targets are given:
/// the limited-broadcast address (reaches every host on the LAN segment)
/// **plus** loopback (the limited broadcast is not reliably looped back to
/// listeners on the sending machine itself on all platforms, and same-machine
/// host+join is both the acceptance recipe and the common "try it out" case).
pub fn default_announce_targets(port: u16) -> Vec<SocketAddr> {
    vec![
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::BROADCAST, port)),
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
    ]
}

/// The host's game-and-lobby socket: any interface, OS-assigned port (the
/// announcement carries the real port), non-blocking, broadcast-capable
/// (announcements are sent from this same socket, so replies arrive here).
pub fn bind_host_socket() -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_nonblocking(true)?;
    sock.set_broadcast(true)?;
    Ok(sock)
}

/// A joiner's discovery listener on `port` (the fixed [`DISCOVERY_PORT`] in
/// real use; an OS-assigned port — `0` — in tests).
pub fn bind_discovery_listener(port: u16) -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port))?;
    sock.set_nonblocking(true)?;
    Ok(sock)
}

/// A joiner's game socket: any interface, OS-assigned port, non-blocking.
pub fn bind_join_socket() -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_nonblocking(true)?;
    Ok(sock)
}
