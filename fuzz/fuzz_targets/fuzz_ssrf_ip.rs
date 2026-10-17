#![no_main]
//! `ssrf::is_blocked_ip`: the one rule deciding whether an outbound fetch to an
//! upstream-supplied URL may dial an address — both for an IP literal in the
//! URL and for every address DNS returns for a hostname.
//!
//! The oracle is an independent table of CIDR blocks written as strings and
//! matched by prefix length, not the octet/segment tests the production code
//! uses. The property is checked **both ways**: blocked ⇔ in the table. A
//! false "allowed" is an SSRF; a false "blocked" is a public upstream refused.
//!
//! Four IPv6 forms carry an IPv4 host that some stack on the path dials, and
//! the oracle classifies them by that embedded address rather than by the v6
//! prefix: mapped `::ffff:0:0/96`, compatible `::/96`, NAT64 `64:ff9b::/96`
//! (last 32 bits) and 6to4 `2002::/16` (bits 16..48).
//!
//! The input picks an interesting prefix and fills the rest at random, so the
//! boundaries of every row are reached rather than only uniformly random
//! addresses (which are almost all global unicast).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use libfuzzer_sys::fuzz_target;

use batlehub_adapters::registry::ssrf::is_blocked_ip;

/// Not globally reachable (or multicast) IPv4 blocks.
const V4_BLOCKED: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.88.99.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

/// Native IPv6 blocks (the v4-embedding forms are handled before this table).
const V6_BLOCKED: &[&str] = &[
    "::/8",
    "100::/64",
    "64:ff9b:1::/48",
    "2001::/23",
    "2001:db8::/32",
    "3fff::/20",
    "5f00::/16",
    "fc00::/7",
    "fe80::/10",
    "fec0::/10",
    "ff00::/8",
];

fn cidr(s: &str) -> (u128, u32, bool) {
    let (addr, len) = s.split_once('/').unwrap();
    let len: u32 = len.parse().unwrap();
    match addr.parse::<IpAddr>().unwrap() {
        IpAddr::V4(a) => (u32::from(a) as u128, len, true),
        IpAddr::V6(a) => (u128::from(a), len, false),
    }
}

fn in_table(table: &[&str], bits: u128, v4: bool) -> bool {
    let width = if v4 { 32 } else { 128 };
    table.iter().any(|row| {
        let (net, len, row_v4) = cidr(row);
        assert_eq!(row_v4, v4);
        len == 0 || (bits >> (width - len)) == (net >> (width - len))
    })
}

fn v4_blocked(a: Ipv4Addr) -> bool {
    in_table(V4_BLOCKED, u32::from(a) as u128, true)
}

fn in_prefix(bits: u128, s: &str) -> bool {
    in_table(&[s], bits, false)
}

fn expected(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => v4_blocked(a),
        IpAddr::V6(a) => {
            let bits = u128::from(a);
            let low32 = Ipv4Addr::from(bits as u32);
            if in_prefix(bits, "::ffff:0:0/96")
                || in_prefix(bits, "::/96")
                || in_prefix(bits, "64:ff9b::/96")
            {
                return v4_blocked(low32);
            }
            if in_prefix(bits, "2002::/16") {
                return v4_blocked(Ipv4Addr::from((bits >> 80) as u32));
            }
            in_table(V6_BLOCKED, bits, false)
        }
    }
}

/// Prefixes worth landing on: every row above, its embedded forms, and the
/// public neighbours on either side of a few boundaries.
const SEEDS: &[&str] = &[
    "::ffff:0:0/96",
    "::/96",
    "64:ff9b::/96",
    "64:ff9b:1::/48",
    "2002::/16",
    "2001::/16",
    "2001:db8::/32",
    "2001:200::/23",
    "100::/56",
    "fe00::/8",
    "fc00::/6",
    "3ff0::/12",
    "5f00::/12",
    "2000::/3",
    "::/16",
];

fn address(u: &mut arbitrary::Unstructured<'_>) -> arbitrary::Result<IpAddr> {
    let pick: u8 = u.arbitrary()?;
    let rand: u128 = u.arbitrary()?;
    Ok(match pick % 4 {
        0 => IpAddr::V4(Ipv4Addr::from(rand as u32)),
        1 => IpAddr::V6(Ipv6Addr::from(rand)),
        2 => {
            // A v4 block's prefix with random host bits.
            let (net, len, _) = cidr(V4_BLOCKED[(pick as usize / 4) % V4_BLOCKED.len()]);
            let keep = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            let bits = (net as u32 & keep) | (rand as u32 & !keep);
            // Optionally wrap it in one of the v6 embeddings.
            let v4 = Ipv4Addr::from(bits);
            match (pick / 64) % 4 {
                0 => IpAddr::V4(v4),
                1 => IpAddr::V6(v4.to_ipv6_mapped()),
                2 => IpAddr::V6(Ipv6Addr::from((0x64_ff9b_u128 << 96) | bits as u128)),
                _ => IpAddr::V6(Ipv6Addr::from(
                    (0x2002_u128 << 112) | ((bits as u128) << 80) | (rand & ((1 << 80) - 1)),
                )),
            }
        }
        _ => {
            let (net, len, _) = cidr(SEEDS[(pick as usize / 4) % SEEDS.len()]);
            let keep = if len == 0 {
                0
            } else {
                u128::MAX << (128 - len)
            };
            IpAddr::V6(Ipv6Addr::from((net & keep) | (rand & !keep)))
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let mut u = arbitrary::Unstructured::new(data);
    let Ok(ip) = address(&mut u) else { return };
    assert_eq!(
        is_blocked_ip(ip),
        expected(ip),
        "{ip}: production says blocked={}, the table says blocked={}",
        is_blocked_ip(ip),
        expected(ip)
    );
});
