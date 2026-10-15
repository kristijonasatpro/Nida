//! The network under the stream. A destination's connection made over an
//! address that has since gone — Wi-Fi switched to another network, a cable
//! pulled — is dead whatever its socket says, and its sender only noticed
//! when a write had been stuck for ten seconds or nothing had gone out for
//! twenty: an operator's stream blinked for half a minute after a Wi-Fi
//! change the Mac had made in seconds.
//!
//! Watched here five times a second: the machine's own addresses, and the
//! ones it sends from by default. An address gone, or the way out moved, is a
//! change of network: its count ([`epoch`]) goes up, and each sender that
//! connected before it and stops moving is reconnected at once (see
//! `fanout::run_sender`) — one still moving was on a way that still works.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// How often the network is looked at: often enough that a switch to a
/// network handing out the same address — Wi-Fi drops its address and its
/// way out for a second or more as it moves — is seen in the gap. A switch
/// keeping both throughout (the same address and the same way out, as
/// from one router to another on one subnet) goes unseen; a connection
/// broken by it is reset by the server soon, or ends on the sender's own
/// timeouts.
const LOOK_EVERY: Duration = Duration::from_millis(200);

/// Changes of network seen since the watcher started.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Changes of network made by tests, each for the destinations on one port
/// (see [`change_for_test`]): tests streaming side by side in one process
/// each see only their own.
static TEST_CHANGES: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

/// The network as seen at one look.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Seen {
    /// The machine's own addresses, as interfaces that are up have them —
    /// not loopback, nor IPv6 link-local, which no stream goes out by.
    addresses: BTreeSet<IpAddr>,
    /// The addresses it sends from by default, IPv4 and IPv6: where the way
    /// out is.
    default_v4: Option<IpAddr>,
    default_v6: Option<IpAddr>,
}

/// The number of changes of network so far: a connection made at one
/// count and found at another may be on a way that is gone. Starts the
/// watcher the first time it is asked.
pub fn epoch() -> u64 {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let spawned = std::thread::Builder::new()
            .name("network-watch".into())
            .spawn(watch);
        if let Err(e) = spawned {
            log::warn!("The network is not watched: {e}");
        }
    });
    EPOCH.load(Ordering::Relaxed)
}

/// The count of network changes as a connection to `url` sees it: the
/// machine's, and those a test made for its port.
pub fn epoch_for(url: &str) -> u64 {
    let made = TEST_CHANGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|port| url.contains(&format!(":{port}/")))
        .count();
    epoch() + made as u64
}

/// A change of network for the destinations on `port`, as if one were seen
/// — for tests, which cannot pull the machine's cable.
#[doc(hidden)]
pub fn change_for_test(port: u16) {
    TEST_CHANGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(port);
}

fn watch() {
    let mut before = look();
    loop {
        std::thread::sleep(LOOK_EVERY);
        let now = look();
        if let Some(what) = changed(&before, &now) {
            EPOCH.fetch_add(1, Ordering::Relaxed);
            log::info!("Network changed: {what}; streams check their connections");
        }
        before = now;
    }
}

/// What changed from `before` to `now` that a connection could have been
/// using, or waiting for: an address gone, the way out moved, or a way out
/// found again — Wi-Fi or the cable back, so a destination waiting to try
/// again need wait no longer. An address only added — a cable plugged in
/// beside the Wi-Fi — leaves every connection as it was, unless the way out
/// moves to it. A connection still sending through any of these is left
/// alone (see `fanout::NetworkWatch`).
fn changed(before: &Seen, now: &Seen) -> Option<String> {
    if let Some(gone) = before.addresses.difference(&now.addresses).next() {
        return Some(format!("address {gone} gone"));
    }
    for (was, is) in [
        (before.default_v4, now.default_v4),
        (before.default_v6, now.default_v6),
    ] {
        match (was, is) {
            (Some(was), is) if Some(was) != is => {
                let to = is.map_or_else(|| "nowhere".to_string(), |a| a.to_string());
                return Some(format!("the way out moved from {was} to {to}"));
            }
            (None, Some(is)) => return Some(format!("a way out found again, from {is}")),
            _ => {}
        }
    }
    None
}

fn look() -> Seen {
    Seen {
        addresses: addresses(),
        default_v4: default_from(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            // TEST-NET-1 (RFC 5737): routed as anywhere out is, never
            // reached — nothing is sent to find which address goes there.
            SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9)),
        ),
        default_v6: default_from(
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            // The documentation prefix (RFC 3849), likewise.
            SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 9)),
        ),
    }
}

/// The address the system would send from towards `to`: a UDP socket's,
/// once pointed there — which sends nothing. `None` with no way there.
fn default_from(bind: SocketAddr, to: SocketAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(to).ok()?;
    let local = socket.local_addr().ok()?.ip();
    (!local.is_unspecified()).then_some(local)
}

/// The machine's own addresses on interfaces that are up, not loopback nor
/// IPv6 link-local.
#[cfg(unix)]
fn addresses() -> BTreeSet<IpAddr> {
    let mut found = BTreeSet::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list it allocates, read
    // here and given back with freeifaddrs.
    if unsafe { libc::getifaddrs(&raw mut list) } != 0 {
        return found;
    }
    let mut at = list;
    while !at.is_null() {
        // SAFETY: a node of the list getifaddrs made, not yet freed.
        let entry = unsafe { &*at };
        at = entry.ifa_next;
        let up = entry.ifa_flags & libc::IFF_UP as u32 != 0;
        let loopback = entry.ifa_flags & libc::IFF_LOOPBACK as u32 != 0;
        if !up || loopback || entry.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: `ifa_addr` points at a sockaddr of the family it says.
        let address = unsafe {
            match i32::from((*entry.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let v4 = &*entry.ifa_addr.cast::<libc::sockaddr_in>();
                    Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr))))
                }
                libc::AF_INET6 => {
                    let v6 = &*entry.ifa_addr.cast::<libc::sockaddr_in6>();
                    Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.s6_addr)))
                }
                _ => None,
            }
        };
        match address {
            Some(IpAddr::V6(v6)) if v6.is_unicast_link_local() => {}
            Some(address) => {
                found.insert(address);
            }
            None => {}
        }
    }
    // SAFETY: the list getifaddrs made, freed once.
    unsafe { libc::freeifaddrs(list) };
    found
}

/// None known here: without `getifaddrs` a change shows only as the way out
/// moving.
#[cfg(not(unix))]
fn addresses() -> BTreeSet<IpAddr> {
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(addresses: &[&str], v4: Option<&str>) -> Seen {
        Seen {
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
            default_v4: v4.map(|a| a.parse().unwrap()),
            default_v6: None,
        }
    }

    /// Wi-Fi switched to another network: its address goes, and the way out
    /// with it — a change; and the way out back is one too. A cable plugged
    /// in beside it adds an address and is no change until the way out moves
    /// to it.
    #[test]
    fn an_address_gone_or_the_way_out_moved_is_a_change() {
        let wifi = seen(&["192.168.1.20"], Some("192.168.1.20"));
        assert_eq!(changed(&wifi, &wifi), None);
        let other_wifi = seen(&["10.0.0.7"], Some("10.0.0.7"));
        assert!(changed(&wifi, &other_wifi).is_some_and(|w| w.contains("192.168.1.20 gone")));
        let cable_beside = seen(&["192.168.1.20", "10.1.1.5"], Some("192.168.1.20"));
        assert_eq!(changed(&wifi, &cable_beside), None, "an address only added");
        let cable_first = seen(&["192.168.1.20", "10.1.1.5"], Some("10.1.1.5"));
        assert!(changed(&cable_beside, &cable_first).is_some_and(|w| w.contains("moved")));
        let no_way_out = seen(&["192.168.1.20"], None);
        assert!(changed(&wifi, &no_way_out).is_some(), "the way out gone");
        assert!(
            changed(&no_way_out, &wifi).is_some_and(|w| w.contains("found again")),
            "a way out found again: waiting need go on no longer"
        );
        assert_eq!(changed(&no_way_out, &no_way_out), None);
    }

    /// A test's change of network is seen only by the destinations on its
    /// port.
    #[test]
    fn a_change_made_for_a_port_is_seen_there_only() {
        let (here, elsewhere) = ("rtmp://127.0.0.1:47123/live/k", "rtmp://127.0.0.1:47124/live/k");
        let before = (epoch_for(here), epoch_for(elsewhere));
        change_for_test(47123);
        assert_eq!(epoch_for(here), before.0 + 1);
        assert_eq!(epoch_for(elsewhere), before.1);
    }

    /// This machine's own addresses are found, and loopback is not among
    /// them.
    #[cfg(unix)]
    #[test]
    fn the_machines_own_addresses_are_found() {
        let found = addresses();
        assert!(found.iter().all(|a| !a.is_loopback()), "{found:?}");
        let now = look();
        if let Some(v4) = now.default_v4 {
            assert!(found.contains(&v4), "the address it sends from is its own: {v4} in {found:?}");
        }
    }
}
