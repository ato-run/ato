//! Runner-owned raw TCP egress broker.
//!
//! Workloads join an additional Docker `--internal` bridge and can reach only
//! this SOCKS5 listener on the bridge gateway. The broker accepts numeric IPv4
//! destinations, applies the exact CIDR/port grant, and opens the external
//! socket from the trusted host. There is no HTTP CONNECT path, DNS lookup,
//! host networking, or ordinary Docker bridge fallback.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::lease::{FixedTcpAllocation, TcpEgressGrant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTION_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONNECTIONS_PER_LISTENER: usize = 128;
const EGRESS_BRIDGE_PATTERN: &str = "atoe+";
const EGRESS_BROKER_PORT: u16 = 1080;

/// Install the host-wide admission boundary for Runner-created egress
/// bridges. Docker's `--internal` route blocks forwarding; these INPUT rules
/// additionally expose exactly the broker Port on interfaces that only an
/// authorized service joins. The rules are idempotent across worker slots and
/// intentionally remain installed while the host can create such bridges.
pub fn prepare_egress_firewall() -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "TCP egress firewall requires a native Linux Runner"
    );
    // Install default deny first. If the exact accept rule cannot be added,
    // startup fails with the egress bridges still closed.
    ensure_iptables_rule(false)?;
    ensure_iptables_rule(true)?;
    Ok(())
}

fn ensure_iptables_rule(accept: bool) -> Result<()> {
    let check = iptables_arguments("-C", accept);
    let checked = iptables_output(&check).context("inspect TCP egress firewall rule")?;
    if checked.status.success() {
        return Ok(());
    }
    let insert = iptables_arguments("-I", accept);
    let output = iptables_output(&insert).context("install TCP egress firewall rule")?;
    ensure!(
        output.status.success(),
        "install TCP egress firewall rule failed: {}",
        bounded_stderr(&output)
    );
    Ok(())
}

fn iptables_output(arguments: &[String]) -> Result<Output> {
    let direct = Command::new("iptables").args(arguments).output();
    if direct.as_ref().is_ok_and(|output| output.status.success()) {
        return direct.context("run iptables directly");
    }
    // Production workers stay unprivileged. A host may grant only these
    // idempotent rule shapes through sudoers; the workload namespaces never
    // receive sudo, the Docker socket, or CAP_NET_ADMIN.
    let privileged = Command::new("sudo")
        .args(["-n", "iptables"])
        .args(arguments)
        .output();
    match (direct, privileged) {
        (_, Ok(output)) => Ok(output),
        (Ok(output), Err(_)) => Ok(output),
        (Err(direct_error), Err(privileged_error)) => Err(anyhow::anyhow!(
            "iptables is unavailable directly ({direct_error}) and through sudo ({privileged_error})"
        )),
    }
}

fn iptables_arguments(operation: &str, accept: bool) -> Vec<String> {
    let mut arguments = vec![
        "--wait".to_owned(),
        "5".to_owned(),
        operation.to_owned(),
        "INPUT".to_owned(),
    ];
    if operation == "-I" {
        arguments.push("1".to_owned());
    }
    arguments.extend(["-i".to_owned(), EGRESS_BRIDGE_PATTERN.to_owned()]);
    if accept {
        arguments.extend([
            "-p".to_owned(),
            "tcp".to_owned(),
            "--dport".to_owned(),
            EGRESS_BROKER_PORT.to_string(),
        ]);
    }
    arguments.extend([
        "-m".to_owned(),
        "comment".to_owned(),
        "--comment".to_owned(),
        if accept {
            "ato-runtime-egress-broker"
        } else {
            "ato-runtime-egress-default-deny"
        }
        .to_owned(),
        "-j".to_owned(),
        if accept { "ACCEPT" } else { "DROP" }.to_owned(),
    ]);
    arguments
}

fn bounded_stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr)
        .chars()
        .take(2048)
        .collect::<String>()
        .trim()
        .to_owned()
}

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

/// Owns every accepted connection for one Run/grant generation. Socket clones
/// are kept only as cancellation handles; shutting them down interrupts both
/// directions of a blocking copy. Worker joins are bounded so revocation can
/// never hang the Runner indefinitely.
#[derive(Default)]
struct ConnectionGroup {
    stopping: AtomicBool,
    next_id: AtomicUsize,
    sockets: Mutex<BTreeMap<usize, Vec<TcpStream>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl ConnectionGroup {
    fn spawn(
        self: &Arc<Self>,
        client: TcpStream,
        permit: ConnectionPermit,
        work: impl FnOnce(usize, TcpStream, Arc<Self>) + Send + 'static,
    ) {
        if self.stopping.load(Ordering::Acquire) {
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let Ok(cancel) = client.try_clone() else {
            return;
        };
        if let Ok(mut sockets) = self.sockets.lock() {
            sockets.insert(id, vec![cancel]);
        } else {
            return;
        }
        let Ok(mut workers) = self.workers.lock() else {
            self.finish(id);
            return;
        };
        // A long-lived listener accepts far more connections than it ever holds
        // open at once. Without this, every connection that has already ended
        // keeps its handle here until the group is torn down, so the group's
        // bookkeeping grows with the total connection count rather than with
        // the live one.
        reap_finished(&mut workers);
        if self.stopping.load(Ordering::Acquire) {
            self.finish(id);
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
        let group = Arc::clone(self);
        let worker = thread::spawn(move || {
            let _permit = permit;
            work(id, client, Arc::clone(&group));
            group.finish(id);
        });
        workers.push(worker);
    }

    fn track(&self, id: usize, stream: &TcpStream) -> bool {
        if self.stopping.load(Ordering::Acquire) {
            let _ = stream.shutdown(Shutdown::Both);
            return false;
        }
        let Ok(cancel) = stream.try_clone() else {
            return false;
        };
        let Ok(mut sockets) = self.sockets.lock() else {
            return false;
        };
        let Some(owned) = sockets.get_mut(&id) else {
            return false;
        };
        owned.push(cancel);
        if self.stopping.load(Ordering::Acquire) {
            for socket in owned {
                let _ = socket.shutdown(Shutdown::Both);
            }
            return false;
        }
        true
    }

    fn finish(&self, id: usize) {
        if let Ok(mut sockets) = self.sockets.lock() {
            sockets.remove(&id);
        }
    }

    #[cfg(test)]
    fn tracked_workers(&self) -> usize {
        self.workers
            .lock()
            .map(|workers| workers.len())
            .unwrap_or(0)
    }

    fn cancel_and_join(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Ok(sockets) = self.sockets.lock() {
            for owned in sockets.values() {
                for socket in owned {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
        }
        let deadline = Instant::now() + CONNECTION_SHUTDOWN_TIMEOUT;
        loop {
            let all_finished = self
                .workers
                .lock()
                .map(|workers| workers.iter().all(JoinHandle::is_finished))
                .unwrap_or(true);
            if all_finished || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if let Ok(mut workers) = self.workers.lock() {
            for worker in workers.drain(..) {
                if worker.is_finished() {
                    let _ = worker.join();
                }
                // A connect still inside the bounded connect timeout is
                // detached. It observes `stopping` before publishing success
                // and can never become an authorized live connection.
            }
        }
    }
}

/// Join and drop the workers that have already ended, keeping the live ones.
///
/// `is_finished` is the only non-blocking way to ask, so a worker that ended a
/// moment ago may survive one pass and be collected by the next.
fn reap_finished(workers: &mut Vec<JoinHandle<()>>) {
    let mut live = Vec::with_capacity(workers.len());
    for worker in workers.drain(..) {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            live.push(worker);
        }
    }
    *workers = live;
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
    connections: Arc<ConnectionGroup>,
}

impl TcpEgressBroker {
    pub fn start(gateway: IpAddr, grant: &TcpEgressGrant) -> Result<Self> {
        ensure!(gateway.is_ipv4(), "TCP egress bridge gateway must be IPv4");
        let policy = EgressPolicy::from_grant(grant)?;
        let listener = TcpListener::bind(SocketAddr::new(gateway, EGRESS_BROKER_PORT))
            .context("bind TCP egress broker on isolated bridge")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let active_connections = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(ConnectionGroup::default());
        let thread_connections = Arc::clone(&connections);
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(permit) = ConnectionPermit::acquire(&active_connections) else {
                            continue;
                        };
                        let policy = policy.clone();
                        thread_connections.spawn(stream, permit, move |id, stream, group| {
                            serve_socks5(id, stream, &policy, &group)
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
            connections,
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
        self.connections.cancel_and_join();
    }
}

fn serve_socks5(
    connection_id: usize,
    mut client: TcpStream,
    policy: &EgressPolicy,
    connections: &ConnectionGroup,
) {
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
    if !connections.track(connection_id, &target) {
        return;
    }
    if socks_reply(&mut client, 0).is_err() {
        return;
    }
    let Ok(mut target_read) = target.try_clone() else {
        return;
    };
    let Ok(mut client_write) = client.try_clone() else {
        return;
    };
    let reverse = thread::spawn(move || {
        let copied = io::copy(&mut target_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
        copied
    });
    let _ = io::copy(&mut client, &mut target);
    let _ = target.shutdown(Shutdown::Write);
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
    connections: Arc<ConnectionGroup>,
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
                            let connections = Arc::clone(&target.connections);
                            connections.spawn(client, permit, move |id, client, group| {
                                proxy_fixed(id, client, target.address, &group);
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
        let target = self
            .state
            .write()
            .ok()
            .and_then(|mut state| state.target.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(target) = target {
            target.connections.cancel_and_join();
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
            let same_allocation =
                state.allocation_id.as_deref() == Some(allocation.allocation_id.as_str());
            if same_allocation {
                ensure!(
                    allocation.generation > state.generation
                        || (allocation.generation == state.generation
                            && state.pending_run_id.as_deref() == Some(run_id)),
                    "fixed TCP allocation generation is stale"
                );
            } else {
                ensure!(
                    state.pending_run_id.is_none() && state.target.is_none(),
                    "fixed TCP address belongs to another live allocation"
                );
            }
            pending.push((allocation, state));
        }
        // Nothing above mutates listener state. Either every allocation is
        // admissible, or the prior generation remains fully intact.
        let mut cancelled = Vec::new();
        for (allocation, mut state) in pending {
            if let Some(target) = state.target.take() {
                cancelled.push(target.connections);
            }
            state.allocation_id = Some(allocation.allocation_id.clone());
            state.generation = allocation.generation;
            state.pending_run_id = Some(run_id.to_owned());
        }
        for connections in cancelled {
            connections.cancel_and_join();
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
            connections: Arc::new(ConnectionGroup::default()),
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
        let target = state.target.take();
        if state.pending_run_id.as_deref() == Some(run_id) {
            state.pending_run_id = None;
        }
        drop(state);
        if let Some(target) = target {
            target.connections.cancel_and_join();
        }
    }
}

fn proxy_fixed(
    connection_id: usize,
    mut client: TcpStream,
    address: SocketAddr,
    connections: &ConnectionGroup,
) {
    let Ok(mut upstream) = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) else {
        return;
    };
    if !connections.track(connection_id, &upstream) {
        return;
    }
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
    let reverse = thread::spawn(move || {
        copy_until_closed(&mut upstream_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
    });
    copy_until_closed(&mut client, &mut upstream);
    let _ = upstream.shutdown(Shutdown::Write);
    let _ = reverse.join();
}

fn copy_until_closed(reader: &mut TcpStream, writer: &mut TcpStream) {
    let mut buffer = [0u8; 16 * 1024];
    loop {
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

    fn named_allocation(
        allocation_id: &str,
        address: SocketAddr,
        generation: u64,
    ) -> FixedTcpAllocation {
        FixedTcpAllocation {
            allocation_id: allocation_id.to_owned(),
            port_id: "smtp.tcp".to_owned(),
            service_id: "smtp".to_owned(),
            guest_port: 2525,
            bind_ip: address.ip(),
            port: address.port(),
            generation,
        }
    }

    fn allocation(address: SocketAddr, generation: u64) -> FixedTcpAllocation {
        named_allocation("tcp_test", address, generation)
    }

    /// A listener that has served thousands of short connections must not be
    /// holding thousands of dead worker handles.
    #[test]
    fn a_long_lived_group_does_not_accumulate_finished_workers() {
        let group = Arc::new(ConnectionGroup::default());
        let active = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepting = std::thread::spawn(move || {
            let mut accepted = Vec::new();
            for _ in 0..2000 {
                let (stream, _) = listener.accept().unwrap();
                accepted.push(stream);
            }
            accepted
        });

        let mut peak = 0;
        for _ in 0..2000 {
            let client = TcpStream::connect(address).unwrap();
            let permit =
                ConnectionPermit::acquire(&active).expect("a permit per serial connection");
            group.spawn(client, permit, |_, stream, _| {
                let _ = stream.shutdown(Shutdown::Both);
            });
            peak = peak.max(group.tracked_workers());
        }
        drop(accepting.join().unwrap());

        assert!(
            peak <= MAX_CONNECTIONS_PER_LISTENER * 2,
            "held worker handles grew with the cumulative connection count: peak={peak}"
        );
        group.cancel_and_join();
        assert_eq!(group.tracked_workers(), 0);
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
    fn firewall_opens_only_the_broker_port_before_default_deny() {
        let accept = iptables_arguments("-I", true);
        let deny = iptables_arguments("-I", false);
        assert!(accept.windows(2).any(|part| part == ["--dport", "1080"]));
        assert!(accept.windows(2).any(|part| part == ["-i", "atoe+"]));
        assert_eq!(accept.last().map(String::as_str), Some("ACCEPT"));
        assert!(!deny.iter().any(|part| part == "--dport"));
        assert_eq!(deny.last().map(String::as_str), Some("DROP"));
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
    fn a_revoked_allocation_hands_its_address_to_a_new_allocation_without_restart() {
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
        let allocation_a = named_allocation("tcp_a", public, 1);
        registry
            .register_pending("run-a", std::slice::from_ref(&allocation_a))
            .unwrap();
        registry.activate("run-a", &allocation_a, target).unwrap();
        registry.deactivate("run-a", &allocation_a);

        let allocation_b = named_allocation("tcp_b", public, 1);
        registry
            .register_pending("run-b", std::slice::from_ref(&allocation_b))
            .expect("a fully deactivated allocation releases the address");
        registry.activate("run-b", &allocation_b, target).unwrap();

        assert!(registry.activate("run-a", &allocation_a, target).is_err());
        registry.deactivate("run-a", &allocation_a);
        let mut client = TcpStream::connect(public).unwrap();
        client.write_all(b"b").unwrap();
        let mut echoed = [0u8; 1];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, *b"b");
        registry.deactivate("run-b", &allocation_b);
        server.join().unwrap();
    }

    #[test]
    fn fixed_tcp_propagates_client_eof_and_returns_the_connection_slot() {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let public = reserved.local_addr().unwrap();
        drop(reserved);
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = upstream.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"request");
            stream.write_all(b"response").unwrap();
        });

        let registry = FixedTcpRegistry::new(&public.to_string()).unwrap();
        let current = allocation(public, 1);
        registry
            .register_pending("run-current", std::slice::from_ref(&current))
            .unwrap();
        registry.activate("run-current", &current, target).unwrap();

        let mut client = TcpStream::connect(public).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client.write_all(b"request").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("upstream sees EOF and completes within the deadline");
        assert_eq!(response, b"response");

        registry.deactivate("run-current", &current);
        server.join().unwrap();
    }

    #[test]
    fn fixed_tcp_deactivation_cancels_an_open_connection_within_the_deadline() {
        let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
        let public = reserved.local_addr().unwrap();
        drop(reserved);
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = upstream.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            accepted_tx.send(()).unwrap();
            let mut byte = [0u8; 1];
            let outcome = stream.read(&mut byte);
            assert!(matches!(outcome, Ok(0) | Err(_)));
        });

        let registry = FixedTcpRegistry::new(&public.to_string()).unwrap();
        let current = allocation(public, 1);
        registry
            .register_pending("run-current", std::slice::from_ref(&current))
            .unwrap();
        registry.activate("run-current", &current, target).unwrap();
        let mut client = TcpStream::connect(public).unwrap();
        client
            .set_read_timeout(Some(CONNECTION_SHUTDOWN_TIMEOUT))
            .unwrap();
        accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        registry.deactivate("run-current", &current);
        assert!(started.elapsed() <= CONNECTION_SHUTDOWN_TIMEOUT);
        let mut byte = [0u8; 1];
        assert!(matches!(client.read(&mut byte), Ok(0) | Err(_)));
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
