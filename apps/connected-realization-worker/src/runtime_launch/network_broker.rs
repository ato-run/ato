//! Runner-owned raw TCP egress broker.
//!
//! Workloads join an additional Docker `--internal` bridge and can reach only
//! this SOCKS5 listener on the bridge gateway. The broker accepts numeric IPv4
//! destinations, applies the exact CIDR/port grant, and opens the external
//! socket from the trusted host. There is no HTTP CONNECT path, DNS lookup,
//! host networking, or ordinary Docker bridge fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, ensure};

use super::lease::{FixedTcpAllocation, TcpEgressGrant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CONNECTIONS_PER_LISTENER: usize = 128;

struct ConnectionPermit(Arc<AtomicUsize>);

impl ConnectionPermit {
    fn acquire(active: &Arc<AtomicUsize>) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_CONNECTIONS_PER_LISTENER).then_some(current + 1)
            })
            .ok()
            .map(|_| Self(Arc::clone(active)))
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone)]
struct Ipv4Cidr {
    network: u32,
    mask: u32,
}

impl Ipv4Cidr {
    fn parse(value: &str) -> Result<Self> {
        let (address, prefix) = value.split_once('/').unwrap_or((value, "32"));
        let address: Ipv4Addr = address
            .parse()
            .context("TCP egress destination is not an IPv4 address")?;
        let prefix: u32 = prefix
            .parse()
            .context("TCP egress CIDR prefix is invalid")?;
        ensure!(
            (1..=32).contains(&prefix),
            "TCP egress CIDR prefix must be between 1 and 32"
        );
        let mask = u32::MAX << (32 - prefix);
        Ok(Self {
            network: u32::from(address) & mask,
            mask,
        })
    }

    fn contains(&self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask == self.network
    }
}

#[derive(Debug, Clone)]
struct EgressPolicy {
    cidr: Ipv4Cidr,
    ports: Vec<u16>,
}

impl EgressPolicy {
    fn from_grant(grant: &TcpEgressGrant) -> Result<Self> {
        ensure!(
            !grant.ports.is_empty() && grant.ports.len() <= 32,
            "TCP egress grant has no bounded port set"
        );
        let mut ports = grant.ports.clone();
        ports.sort_unstable();
        ports.dedup();
        ensure!(
            ports.len() == grant.ports.len(),
            "TCP egress grant repeats a port"
        );
        Ok(Self {
            cidr: Ipv4Cidr::parse(&grant.destination_cidr)?,
            ports,
        })
    }

    fn permits(&self, address: Ipv4Addr, port: u16) -> bool {
        self.cidr.contains(address)
            && self.ports.binary_search(&port).is_ok()
            && !is_protected(address)
    }
}

/// Deny local, special-use, multicast and metadata-reachable space even when
/// a broad operator CIDR contains it.
fn is_protected(address: Ipv4Addr) -> bool {
    let value = u32::from(address);
    let in_prefix = |network: u32, prefix: u32| {
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        value & mask == network & mask
    };
    address.is_unspecified()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_private()
        || address.is_multicast()
        || address.is_broadcast()
        || in_prefix(u32::from(Ipv4Addr::new(100, 64, 0, 0)), 10)
        || in_prefix(u32::from(Ipv4Addr::new(198, 18, 0, 0)), 15)
        || in_prefix(u32::from(Ipv4Addr::new(240, 0, 0, 0)), 4)
}

pub struct TcpEgressBroker {
    endpoint: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TcpEgressBroker {
    pub fn start(gateway: IpAddr, grant: &TcpEgressGrant) -> Result<Self> {
        ensure!(gateway.is_ipv4(), "TCP egress bridge gateway must be IPv4");
        let policy = EgressPolicy::from_grant(grant)?;
        let listener = TcpListener::bind(SocketAddr::new(gateway, 0))
            .context("bind TCP egress broker on isolated bridge")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(permit) = ConnectionPermit::acquire(&active_connections) else {
                            continue;
                        };
                        let policy = policy.clone();
                        thread::spawn(move || {
                            let _permit = permit;
                            serve_socks5(stream, &policy);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            endpoint,
            stop,
            worker: Some(worker),
        })
    }

    pub fn binding_value(&self) -> String {
        format!("socks5://{}", self.endpoint)
    }
}

impl Drop for TcpEgressBroker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_socks5(mut client: TcpStream, policy: &EgressPolicy) {
    let _ = client.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = client.set_write_timeout(Some(CONNECT_TIMEOUT));
    let mut greeting = [0u8; 2];
    if client.read_exact(&mut greeting).is_err() || greeting[0] != 5 || greeting[1] == 0 {
        return;
    }
    let mut methods = vec![0u8; usize::from(greeting[1])];
    if client.read_exact(&mut methods).is_err() || !methods.contains(&0) {
        let _ = client.write_all(&[5, 0xff]);
        return;
    }
    if client.write_all(&[5, 0]).is_err() {
        return;
    }
    let mut request = [0u8; 4];
    if client.read_exact(&mut request).is_err() || request != [5, 1, 0, 1] {
        let _ = socks_reply(&mut client, 8);
        return;
    }
    let mut address_bytes = [0u8; 4];
    let mut port_bytes = [0u8; 2];
    if client.read_exact(&mut address_bytes).is_err() || client.read_exact(&mut port_bytes).is_err()
    {
        return;
    }
    let address = Ipv4Addr::from(address_bytes);
    let port = u16::from_be_bytes(port_bytes);
    if !policy.permits(address, port) {
        let _ = socks_reply(&mut client, 2);
        return;
    }
    let Ok(mut target) =
        TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(address), port), CONNECT_TIMEOUT)
    else {
        let _ = socks_reply(&mut client, 5);
        return;
    };
    if socks_reply(&mut client, 0).is_err() {
        return;
    }
    let Ok(mut target_read) = target.try_clone() else {
        return;
    };
    let Ok(mut client_write) = client.try_clone() else {
        return;
    };
    let reverse = thread::spawn(move || io::copy(&mut target_read, &mut client_write));
    let _ = io::copy(&mut client, &mut target);
    let _ = reverse.join();
}

fn socks_reply(stream: &mut TcpStream, status: u8) -> io::Result<()> {
    stream.write_all(&[5, status, 0, 1, 0, 0, 0, 0, 0, 0])
}

#[derive(Clone)]
struct FixedTarget {
    run_id: String,
    generation: u64,
    address: SocketAddr,
    drain: Arc<AtomicBool>,
}

#[derive(Default)]
struct FixedListenerState {
    allocation_id: Option<String>,
    generation: u64,
    pending_run_id: Option<String>,
    target: Option<FixedTarget>,
}

struct FixedListener {
    state: Arc<RwLock<FixedListenerState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl FixedListener {
    fn bind(address: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(address).context("bind fixed TCP allocation")?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(RwLock::new(FixedListenerState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let Some(permit) = ConnectionPermit::acquire(&active_connections) else {
                            continue;
                        };
                        let target = thread_state
                            .read()
                            .ok()
                            .and_then(|state| state.target.clone());
                        if let Some(target) = target {
                            thread::spawn(move || {
                                let _permit = permit;
                                proxy_fixed(client, target);
                            });
                        }
                        // No target during handover: dropping the accepted
                        // stream refuses it instead of reaching the old Run.
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            state,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for FixedListener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.write()
            && let Some(target) = state.target.take()
        {
            target.drain.store(true, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Host-process lifetime owner of fixed public listeners. Each worker process
/// may bind only addresses explicitly configured for its Runner slot.
pub struct FixedTcpRegistry {
    listeners: Mutex<BTreeMap<SocketAddr, FixedListener>>,
}

impl FixedTcpRegistry {
    pub fn new(allowlist: &str) -> Result<Self> {
        let allowed = allowlist
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                entry
                    .parse::<SocketAddr>()
                    .with_context(|| format!("invalid fixed TCP allowlist entry `{entry}`"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let listeners = allowed
            .into_iter()
            .map(|address| Ok((address, FixedListener::bind(address)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(Self {
            listeners: Mutex::new(listeners),
        })
    }

    pub fn available(&self) -> bool {
        self.listeners
            .lock()
            .map(|listeners| !listeners.is_empty())
            .unwrap_or(false)
    }

    pub fn register_pending(&self, run_id: &str, allocations: &[FixedTcpAllocation]) -> Result<()> {
        let listeners = self
            .listeners
            .lock()
            .map_err(|_| anyhow::anyhow!("fixed TCP registry lock poisoned"))?;
        let mut addresses = BTreeSet::new();
        for allocation in allocations {
            let address = SocketAddr::new(allocation.bind_ip, allocation.port);
            ensure!(
                addresses.insert(address),
                "fixed TCP authorization repeats address {address}"
            );
        }
        let mut pending = Vec::with_capacity(allocations.len());
        for allocation in allocations {
            let address = SocketAddr::new(allocation.bind_ip, allocation.port);
            let listener = listeners.get(&address).with_context(|| {
                format!("fixed TCP allocation {address} is outside the Runner allowlist")
            })?;
            let state = listener
                .state
                .write()
                .map_err(|_| anyhow::anyhow!("fixed TCP listener lock poisoned"))?;
            ensure!(
                state
                    .allocation_id
                    .as_deref()
                    .is_none_or(|id| id == allocation.allocation_id),
                "fixed TCP address belongs to another allocation"
            );
            ensure!(
                allocation.generation > state.generation
                    || (allocation.generation == state.generation
                        && state.pending_run_id.as_deref() == Some(run_id)),
                "fixed TCP allocation generation is stale"
            );
            pending.push((allocation, state));
        }
        // Nothing above mutates listener state. Either every allocation is
        // admissible, or the prior generation remains fully intact.
        for (allocation, mut state) in pending {
            if let Some(target) = state.target.take() {
                target.drain.store(true, Ordering::Release);
            }
            state.allocation_id = Some(allocation.allocation_id.clone());
            state.generation = allocation.generation;
            state.pending_run_id = Some(run_id.to_owned());
        }
        Ok(())
    }

    pub fn activate(
        &self,
        run_id: &str,
        allocation: &FixedTcpAllocation,
        target: SocketAddr,
    ) -> Result<()> {
        let listeners = self
            .listeners
            .lock()
            .map_err(|_| anyhow::anyhow!("fixed TCP registry lock poisoned"))?;
        let address = SocketAddr::new(allocation.bind_ip, allocation.port);
        let listener = listeners
            .get(&address)
            .context("fixed TCP listener was not registered")?;
        let mut state = listener
            .state
            .write()
            .map_err(|_| anyhow::anyhow!("fixed TCP listener lock poisoned"))?;
        ensure!(
            state.allocation_id.as_deref() == Some(&allocation.allocation_id)
                && state.generation == allocation.generation
                && state.pending_run_id.as_deref() == Some(run_id),
            "fixed TCP activation does not match pending generation"
        );
        state.target = Some(FixedTarget {
            run_id: run_id.to_owned(),
            generation: allocation.generation,
            address: target,
            drain: Arc::new(AtomicBool::new(false)),
        });
        Ok(())
    }

    pub fn deactivate(&self, run_id: &str, allocation: &FixedTcpAllocation) {
        let Ok(listeners) = self.listeners.lock() else {
            return;
        };
        let address = SocketAddr::new(allocation.bind_ip, allocation.port);
        let Some(listener) = listeners.get(&address) else {
            return;
        };
        let Ok(mut state) = listener.state.write() else {
            return;
        };
        if state.allocation_id.as_deref() != Some(&allocation.allocation_id)
            || state.generation != allocation.generation
        {
            return;
        }
        if let Some(target) = state.target.as_ref()
            && (target.run_id != run_id || target.generation != allocation.generation)
        {
            return;
        }
        if let Some(target) = state.target.take() {
            target.drain.store(true, Ordering::Release);
        }
        if state.pending_run_id.as_deref() == Some(run_id) {
            state.pending_run_id = None;
        }
    }
}

fn proxy_fixed(mut client: TcpStream, target: FixedTarget) {
    let Ok(mut upstream) = TcpStream::connect_timeout(&target.address, CONNECT_TIMEOUT) else {
        return;
    };
    let timeout = Some(Duration::from_millis(250));
    let _ = client.set_read_timeout(timeout);
    let _ = client.set_write_timeout(timeout);
    let _ = upstream.set_read_timeout(timeout);
    let _ = upstream.set_write_timeout(timeout);
    let Ok(mut client_write) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_read) = upstream.try_clone() else {
        return;
    };
    let reverse_stop = Arc::clone(&target.drain);
    let reverse = thread::spawn(move || {
        copy_until_drained(&mut upstream_read, &mut client_write, &reverse_stop)
    });
    copy_until_drained(&mut client, &mut upstream, &target.drain);
    let _ = reverse.join();
}

fn copy_until_drained(reader: &mut TcpStream, writer: &mut TcpStream, stop: &AtomicBool) {
    let mut buffer = [0u8; 16 * 1024];
    while !stop.load(Ordering::Acquire) {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) if writer.write_all(&buffer[..count]).is_err() => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocation(address: SocketAddr, generation: u64) -> FixedTcpAllocation {
        FixedTcpAllocation {
            allocation_id: "tcp_test".to_owned(),
            port_id: "smtp.tcp".to_owned(),
            service_id: "smtp".to_owned(),
            guest_port: 2525,
            bind_ip: address.ip(),
            port: address.port(),
            generation,
        }
    }

    #[test]
    fn broad_grants_still_exclude_protected_destinations() {
        let policy = EgressPolicy {
            cidr: Ipv4Cidr::parse("0.0.0.0/1").unwrap(),
            ports: vec![25, 443],
        };
        assert!(policy.permits(Ipv4Addr::new(8, 8, 8, 8), 443));
        assert!(!policy.permits(Ipv4Addr::new(127, 0, 0, 1), 443));
        assert!(!policy.permits(Ipv4Addr::new(10, 0, 0, 1), 25));
        assert!(!policy.permits(Ipv4Addr::new(8, 8, 8, 8), 80));
        let upper_half = EgressPolicy {
            cidr: Ipv4Cidr::parse("128.0.0.0/1").unwrap(),
            ports: vec![443],
        };
        assert!(!upper_half.permits(Ipv4Addr::new(169, 254, 169, 254), 443));
        assert!(Ipv4Cidr::parse("0.0.0.0/0").is_err());
    }

    #[test]
    fn stale_deactivation_cannot_detach_the_current_fixed_target() {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let public = reserved.local_addr().unwrap();
        drop(reserved);
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = upstream.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            stream.write_all(&byte).unwrap();
        });

        let registry = FixedTcpRegistry::new(&public.to_string()).unwrap();
        let old = allocation(public, 1);
        registry
            .register_pending("run-old", std::slice::from_ref(&old))
            .unwrap();
        let current = allocation(public, 2);
        registry
            .register_pending("run-current", std::slice::from_ref(&current))
            .unwrap();
        registry.activate("run-current", &current, target).unwrap();
        registry.deactivate("run-old", &old);

        let mut client = TcpStream::connect(public).unwrap();
        client.write_all(b"x").unwrap();
        let mut echoed = [0u8; 1];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, *b"x");
        registry.deactivate("run-current", &current);
        server.join().unwrap();
    }

    #[test]
    fn a_multi_allocation_preflight_mutates_nothing_when_one_address_is_refused() {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let allowed = reserved.local_addr().unwrap();
        drop(reserved);
        let refused = TcpListener::bind("127.0.0.1:0").unwrap();
        let refused_address = refused.local_addr().unwrap();
        drop(refused);

        let registry = FixedTcpRegistry::new(&allowed.to_string()).unwrap();
        let current = allocation(allowed, 1);
        registry
            .register_pending("run-current", std::slice::from_ref(&current))
            .unwrap();
        let next = allocation(allowed, 2);
        let mut outside = allocation(refused_address, 1);
        outside.allocation_id = "tcp_outside".to_owned();
        assert!(
            registry
                .register_pending("run-next", &[next, outside])
                .is_err()
        );

        let listeners = registry.listeners.lock().unwrap();
        let state = listeners.get(&allowed).unwrap().state.read().unwrap();
        assert_eq!(state.generation, 1);
        assert_eq!(state.pending_run_id.as_deref(), Some("run-current"));
    }
}
