//! Who may knock (PHONE-2), and where this Mac can be found.
//!
//! The listener is on every IPv4 interface, so it is the caller's address
//! that decides. Bound to the Tailscale address alone, it went deaf whenever
//! Tailscale started after the app or the address moved, and the home
//! network's address changes with every network the Mac joins.

use std::net::{IpAddr, Ipv4Addr};

use serde::Serialize;

/// The ways in that are open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reach {
    pub tailscale: bool,
    pub home: bool,
}

/// Which way a caller, or one of this Mac's addresses, is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Way {
    /// 100.64.0.0/10, the range Tailscale gives its devices.
    Tailscale,
    /// The private ranges a home router hands out: 10/8, 172.16/12,
    /// 192.168/16. A phone on the router's own VPN comes from one of these
    /// too.
    Home,
}

pub fn way_of(ip: IpAddr) -> Option<Way> {
    // An IPv4 caller seen through an IPv6 socket.
    let ip = match ip {
        IpAddr::V6(v6) => IpAddr::V4(v6.to_ipv4_mapped()?),
        v4 => v4,
    };
    let IpAddr::V4(v4) = ip else { return None };
    let [a, b, ..] = v4.octets();
    if a == 100 && (64..128).contains(&b) {
        Some(Way::Tailscale)
    } else if v4.is_private() {
        Some(Way::Home)
    } else {
        None
    }
}

impl Reach {
    pub fn any(self) -> bool {
        self.tailscale || self.home
    }

    /// Whether a caller from `ip` may come in. Loopback is not one of the
    /// ways: nothing on this Mac needs the phone's door, and an agent on it
    /// has the MCP server.
    pub fn admits(self, ip: IpAddr) -> bool {
        match way_of(ip) {
            Some(Way::Tailscale) => self.tailscale,
            Some(Way::Home) => self.home,
            None => false,
        }
    }
}

/// One of this Mac's addresses a phone could use.
#[derive(Debug, Clone, Serialize)]
pub struct Address {
    pub way: Way,
    pub ip: String,
    /// The interface it is on: `en0`, `utun4`.
    pub interface: String,
}

/// This Mac's addresses on either way in, Tailscale's first.
pub fn addresses() -> Vec<Address> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list it owns, freed
    // below with freeifaddrs; nothing is kept past that.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return found;
    }
    let mut at = list;
    while !at.is_null() {
        // SAFETY: `at` is a node of the list getifaddrs returned.
        let ifa = unsafe { &*at };
        at = ifa.ifa_next;
        let up = ifa.ifa_flags & (libc::IFF_UP as u32) != 0;
        if !up || ifa.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: checked non-null; the family says how to read it.
        if unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        // SAFETY: an AF_INET address is a sockaddr_in.
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        let Some(way) = way_of(IpAddr::V4(ip)) else { continue };
        // SAFETY: ifa_name is a NUL-terminated string owned by the list.
        let interface = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .to_string();
        found.push(Address { way, ip: ip.to_string(), interface });
    }
    // SAFETY: the list came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(list) };
    found.sort_by_key(|a| (a.way != Way::Tailscale, a.interface.clone()));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn each_switch_lets_in_its_own_range_and_nothing_else() {
        let tailscale = Reach { tailscale: true, home: false };
        let home = Reach { tailscale: false, home: true };
        let both = Reach { tailscale: true, home: true };

        for caller in ["100.64.0.1", "100.101.102.103", "100.127.255.254"] {
            assert!(tailscale.admits(ip(caller)), "{caller}");
            assert!(!home.admits(ip(caller)), "{caller}");
        }
        // The router's LAN, its WireGuard clients, and other private ranges.
        for caller in ["192.168.8.23", "10.0.0.2", "172.16.4.1", "172.31.255.1"] {
            assert!(home.admits(ip(caller)), "{caller}");
            assert!(!tailscale.admits(ip(caller)), "{caller}");
        }
        // The internet, this Mac itself, and the edges of the ranges.
        for caller in ["8.8.8.8", "127.0.0.1", "100.63.255.255", "100.128.0.1", "172.32.0.1", "169.254.1.1", "::1"] {
            assert!(!both.admits(ip(caller)), "{caller}");
        }
        assert!(!Reach::default().admits(ip("192.168.1.2")), "closed is closed");
    }

    #[test]
    fn an_ipv4_caller_seen_as_ipv6_is_judged_by_its_ipv4_address() {
        let home = Reach { tailscale: false, home: true };
        assert!(home.admits(ip("::ffff:192.168.8.23")));
        assert!(!home.admits(ip("::ffff:8.8.8.8")));
        assert!(!home.admits(ip("fd00::1")), "IPv6 is not a way in");
    }
}
