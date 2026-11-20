//! Every IPv4 address form that the egress gate's CIDR stage is supposed to keep
//! away from loopback / link-local / private space must be covered by
//! DENIED_RANGES, which is what check_addr applies to a resolved address.
//!
//! DENIED_RANGES is extracted verbatim from services/netd/src/egress/gate.rs by
//! run.sh, so this reproduces the shipped list rather than a copy of it.
use ipnet::IpNet;
use std::net::IpAddr;

include!(concat!(env!("OUT"), "/egress_denied_ranges.rs"));

fn denied(addr: &str) -> bool {
    let ip: IpAddr = addr.parse().unwrap_or_else(|_| panic!("bad address {addr}"));
    DENIED_RANGES.iter().any(|range| {
        let net: IpNet = range.parse().expect("static range");
        net.contains(&ip)
    })
}

fn show(addr: &str, reaches: &str) {
    println!("{addr:<22} denied={:<5} (encodes {reaches})", denied(addr));
}

fn main() {
    // Positive controls: the shipped list does catch these.
    assert!(denied("127.0.0.1"));
    assert!(denied("169.254.169.254"));
    assert!(denied("::ffff:127.0.0.1"), "IPv4-mapped form is denied");
    assert!(denied("::1"));
    assert!(denied("64:ff9b::7f00:1"), "well-known NAT64 /96 is denied");
    println!("positive controls: IPv4, link-local, ::ffff:0:0/96, ::1, 64:ff9b::/96 are denied");

    // Remaining holes: addresses that encode 127.0.0.1 (or another private IPv4)
    // in a standard IPv6 transition form that the deny list does not cover.
    show("::7f00:1", "IPv4-compatible ::127.0.0.1");
    show("2002:7f00:1::", "6to4 2002:V4ADDR::/48 -> 127.0.0.1");
    show("2002:a9fe:a9fe::", "6to4 -> 169.254.169.254");
    show("64:ff9b:1::7f00:1", "RFC 8215 local-use NAT64 -> 127.0.0.1");
    show("192.88.99.1", "6to4 relay anycast");

    assert!(!denied("::7f00:1"), "IPv4-compatible loopback is allowed");
    assert!(!denied("2002:7f00:1::"), "6to4 loopback is allowed");
    assert!(!denied("2002:a9fe:a9fe::"), "6to4 link-local is allowed");
    assert!(!denied("64:ff9b:1::7f00:1"), "local-use NAT64 loopback is allowed");
    println!(
        "egress CIDR deny list leaves {} of the documented embedding forms uncovered",
        4
    );
}
