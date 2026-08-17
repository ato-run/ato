#![no_std]
#![no_main]

use aya_ebpf::macros::{cgroup_skb, map};
use aya_ebpf::maps::lpm_trie::{Key as LpmKey, LpmTrie};
use aya_ebpf::programs::SkBuffContext;

#[repr(C)]
pub struct Ipv4LpmKey {
    pub addr: u32,
}

#[repr(C)]
pub struct Ipv6LpmKey {
    pub addr: [u8; 16],
}

#[map]
static IPV4_ALLOW: LpmTrie<Ipv4LpmKey, u8> = LpmTrie::<Ipv4LpmKey, u8>::with_max_entries(4096, 0);

#[map]
static IPV6_ALLOW: LpmTrie<Ipv6LpmKey, u8> = LpmTrie::<Ipv6LpmKey, u8>::with_max_entries(4096, 0);

#[map]
static DNS_IPV4_ALLOW: LpmTrie<Ipv4LpmKey, u8> =
    LpmTrie::<Ipv4LpmKey, u8>::with_max_entries(256, 0);

#[map]
static DNS_IPV6_ALLOW: LpmTrie<Ipv6LpmKey, u8> =
    LpmTrie::<Ipv6LpmKey, u8>::with_max_entries(256, 0);

#[cgroup_skb(egress)]
pub fn nacelle_egress(ctx: SkBuffContext) -> i32 {
    match try_nacelle_egress(ctx) {
        Ok(allow) => {
            if allow {
                1
            } else {
                0
            }
        }
        Err(_) => 0,
    }
}

fn try_nacelle_egress(ctx: SkBuffContext) -> Result<bool, i64> {
    let pkt_len = ctx.len();
    let version: u8 = ctx.load(0)?;

    let ip_version = version >> 4;
    if ip_version == 4 {
        if pkt_len < 20 {
            return Ok(true);
        }
        // Always allow loopback traffic so sandboxed local web services remain reachable.
        let dst_first_octet: u8 = ctx.load(16)?;
        if dst_first_octet == 127 {
            return Ok(true);
        }
        let dst: u32 = u32::from_be(ctx.load(16)?);

        let key = LpmKey::new(32, Ipv4LpmKey { addr: dst });

        if IPV4_ALLOW.get(&key).is_some() {
            return Ok(true);
        }

        let dns_key = LpmKey::new(32, Ipv4LpmKey { addr: dst });
        if DNS_IPV4_ALLOW.get(&dns_key).is_some() && is_dns_packet_v4(&ctx, version)? {
            return Ok(true);
        }

        return Ok(false);
    }

    if ip_version == 6 {
        // Fail-closed for IPv6 until a verifier-safe parser is implemented.
        return Ok(false);
    }

    Ok(true)
}

fn is_dns_packet_v4(ctx: &SkBuffContext, first_byte: u8) -> Result<bool, i64> {
    let pkt_len = ctx.len();
    let ihl = (first_byte & 0x0f) as usize * 4;
    if pkt_len < (ihl as u32 + 4) {
        return Ok(false);
    }
    let proto: u8 = ctx.load(9)?;
    if proto != 6 && proto != 17 {
        return Ok(false);
    }
    let port: u16 = ctx.load(ihl + 2)?;
    Ok(u16::from_be(port) == 53)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    unsafe { core::hint::unreachable_unchecked() }
}

#[used]
#[unsafe(link_section = "license")]
static LICENSE: [u8; 4] = *b"GPL\0";
