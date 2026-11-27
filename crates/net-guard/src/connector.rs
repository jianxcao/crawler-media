use std::fmt::Debug;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use crate::{NetGuardError, NormalizedTarget, Origin, TargetPolicy};

pub trait DnsResolver: Send + Sync + Debug {
    fn resolve(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError>;
}

#[derive(Debug, Default)]
pub struct SystemDnsResolver;

fn collect_unique_ips(resolved: impl Iterator<Item = SocketAddr>) -> Vec<IpAddr> {
    let mut addrs = Vec::new();
    for sock in resolved {
        if !addrs.contains(&sock.ip()) {
            addrs.push(sock.ip());
        }
    }
    addrs
}

impl DnsResolver for SystemDnsResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Vec<IpAddr>, NetGuardError> {
        if timeout.is_zero() {
            return Err(NetGuardError::Timeout(
                "DNS resolution deadline expired".into(),
            ));
        }
        let host_owned = host.to_string();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let res = (host_owned.as_str(), port)
                .to_socket_addrs()
                .map(collect_unique_ips)
                .map_err(|e| NetGuardError::Transport(format!("dns resolution failed: {e}")));
            let _ = tx.send(res);
        });
        match rx.recv_timeout(timeout) {
            Ok(res) => res,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(NetGuardError::Timeout(
                format!("dns resolution timed out after {timeout:?} for `{host}`"),
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(NetGuardError::Transport(
                format!("dns resolver worker disconnected for `{host}`"),
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedTarget {
    pub origin: Origin,
    pub addrs: Vec<SocketAddr>,
    pub uri: ureq::http::Uri,
}

pub fn resolve_and_pin(
    target: &NormalizedTarget,
    policy: &TargetPolicy,
    resolver: &dyn DnsResolver,
    timeout: Duration,
) -> Result<ResolvedTarget, NetGuardError> {
    policy.validate_target(target)?;
    let ips = if let Some(ip) = target.host_is_ip {
        vec![ip]
    } else {
        resolver.resolve(&target.host_ascii, target.port, timeout)?
    };
    if ips.is_empty() {
        return Err(NetGuardError::Transport(format!(
            "dns returned no addresses for `{}`",
            target.host_ascii
        )));
    }
    let mut pinned = Vec::with_capacity(ips.len());
    for ip in ips {
        let sock = SocketAddr::new(ip, target.port);
        policy.validate_socket_addr(target, sock)?;
        if !pinned.contains(&sock) {
            pinned.push(sock);
        }
    }
    let uri: ureq::http::Uri = target
        .url
        .as_str()
        .parse()
        .map_err(|e| NetGuardError::InvalidUrl(format!("invalid http uri: {e}")))?;
    Ok(ResolvedTarget {
        origin: target.origin.clone(),
        addrs: pinned,
        uri,
    })
}

#[derive(Debug)]
pub struct PinnedResolver {
    pub expected_uri: ureq::http::Uri,
    pub addrs: Vec<SocketAddr>,
}

impl Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        if uri.authority() != self.expected_uri.authority()
            || uri.scheme() != self.expected_uri.scheme()
        {
            return Err(ureq::Error::HostNotFound);
        }
        let mut out = self.empty();
        for addr in self.addrs.iter().take(16) {
            out.push(*addr);
        }
        if out.is_empty() {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(out)
    }
}

pub struct PinnedTcpTransport {
    stream: TcpStream,
    buffers: LazyBuffers,
    timeout_read: Option<Duration>,
    timeout_write: Option<Duration>,
    connect_deadline: Option<Instant>,
    global_deadline: Option<Instant>,
}

impl Debug for PinnedTcpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedTcpTransport")
            .field("peer", &self.stream.peer_addr().ok())
            .finish()
    }
}

fn resolve_effective_timeout(
    timeout: NextTimeout,
    connect_deadline: Option<Instant>,
    global_deadline: Option<Instant>,
) -> Result<Option<Duration>, ureq::Error> {
    if !timeout.after.is_not_happening() && timeout.after.is_zero() {
        return Err(ureq::Error::Timeout(timeout.reason));
    }
    let active_deadline = if timeout.reason == ureq::Timeout::Connect {
        match (connect_deadline, global_deadline) {
            (Some(cd), Some(gd)) => Some(cd.min(gd)),
            (Some(cd), None) => Some(cd),
            (None, Some(gd)) => Some(gd),
            (None, None) => None,
        }
    } else {
        global_deadline
    };
    let mut next = if timeout.after.is_not_happening() {
        None
    } else {
        Some(*timeout.after)
    };
    if let Some(deadline) = active_deadline {
        let now = Instant::now();
        if now >= deadline {
            return Err(ureq::Error::Timeout(timeout.reason));
        }
        let rem = deadline
            .saturating_duration_since(now)
            .max(Duration::from_nanos(1));
        next = Some(match next {
            Some(d) => d.min(rem),
            None => rem,
        });
    }
    Ok(next)
}

fn update_timeout(
    next: Option<Duration>,
    prev: &mut Option<Duration>,
    stream: &TcpStream,
    setter: fn(&TcpStream, Option<Duration>) -> io::Result<()>,
) -> io::Result<()> {
    if next != *prev {
        if let Err(e) = setter(stream, next) {
            if e.kind() == io::ErrorKind::InvalidInput {
                tracing::debug!(
                    error = %e,
                    "ignoring EINVAL updating timeout on closed/reset socket"
                );
            } else {
                return Err(e);
            }
        }
        *prev = next;
    }
    Ok(())
}

impl Transport for PinnedTcpTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let effective =
            resolve_effective_timeout(timeout, self.connect_deadline, self.global_deadline)?;
        update_timeout(
            effective,
            &mut self.timeout_write,
            &self.stream,
            TcpStream::set_write_timeout,
        )?;
        let output = &self.buffers.output()[..amount];
        match self.stream.write_all(output) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                Err(ureq::Error::Timeout(timeout.reason))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let effective =
            resolve_effective_timeout(timeout, self.connect_deadline, self.global_deadline)?;
        update_timeout(
            effective,
            &mut self.timeout_read,
            &self.stream,
            TcpStream::set_read_timeout,
        )?;
        let input = self.buffers.input_append_buf();
        let n = match self.stream.read(input) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(ureq::Error::Timeout(timeout.reason));
            }
            Err(e) => return Err(e.into()),
        };
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct PinnedTcpConnector {
    pub addrs: Vec<SocketAddr>,
    pub target: NormalizedTarget,
    pub policy: TargetPolicy,
}

#[derive(Debug)]
pub(crate) struct DeadlinePinnedTcpConnector {
    pub(crate) inner: PinnedTcpConnector,
    pub(crate) global_deadline: Option<Instant>,
}

impl PinnedTcpConnector {
    fn build_pinned_transport(
        &self,
        stream: TcpStream,
        details: &ConnectionDetails,
        connect_deadline: Option<Instant>,
        global_deadline: Option<Instant>,
    ) -> Result<PinnedTcpTransport, ureq::Error> {
        let peer = stream.peer_addr()?;
        if !self.addrs.iter().take(16).any(|addr| *addr == peer)
            || self
                .policy
                .validate_socket_addr(&self.target, peer)
                .is_err()
        {
            return Err(ureq::Error::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connected peer address mismatched pinned target",
            )));
        }
        if details.config.no_delay()
            && let Err(e) = stream.set_nodelay(true)
        {
            if e.kind() == io::ErrorKind::InvalidInput {
                tracing::debug!(
                    error = %e,
                    peer = %peer,
                    "ignoring EINVAL from TCP_NODELAY on closed/reset socket"
                );
            } else {
                return Err(e.into());
            }
        }
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(PinnedTcpTransport {
            stream,
            buffers,
            timeout_read: None,
            timeout_write: None,
            connect_deadline,
            global_deadline,
        })
    }

    pub(crate) fn connect_with_deadline(
        &self,
        details: &ConnectionDetails,
        global_deadline: Option<Instant>,
    ) -> Result<Option<PinnedTcpTransport>, ureq::Error> {
        if details.config.proxy().is_some() {
            return Err(ureq::Error::ConnectionFailed);
        }
        if !details.timeout.after.is_not_happening() && details.timeout.after.is_zero() {
            return Err(ureq::Error::Timeout(details.timeout.reason));
        }
        let now = Instant::now();
        if let Some(gd) = global_deadline
            && now >= gd
        {
            return Err(ureq::Error::Timeout(ureq::Timeout::Global));
        }
        let mut total_budget = if details.timeout.after.is_not_happening() {
            self.policy.limits.connect_timeout
        } else {
            (*details.timeout.after).min(self.policy.limits.connect_timeout)
        };
        if let Some(gd) = global_deadline {
            total_budget = total_budget.min(gd.saturating_duration_since(now));
        }
        if total_budget.is_zero() {
            return Err(ureq::Error::Timeout(details.timeout.reason));
        }
        let connect_deadline = now + total_budget;
        let candidates_len = self.addrs.len().min(16);

        let mut last_err = None;
        for (idx, addr) in self.addrs.iter().copied().take(16).enumerate() {
            if self
                .policy
                .validate_socket_addr(&self.target, addr)
                .is_err()
            {
                return Err(ureq::Error::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "pinned address failed policy",
                )));
            }
            let remaining = connect_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ureq::Error::Timeout(details.timeout.reason));
            }
            let left = candidates_len - idx;
            let connect_timeout = if left > 1 {
                (remaining / 2).max(Duration::from_nanos(1))
            } else {
                remaining
            };
            match TcpStream::connect_timeout(&addr, connect_timeout) {
                Ok(stream) => {
                    return self
                        .build_pinned_transport(
                            stream,
                            details,
                            Some(connect_deadline),
                            global_deadline,
                        )
                        .map(Some);
                }
                Err(e) => last_err = Some(e),
            }
        }
        match last_err {
            Some(e) if e.kind() == io::ErrorKind::TimedOut => {
                Err(ureq::Error::Timeout(details.timeout.reason))
            }
            Some(e) => Err(ureq::Error::Io(e)),
            None => Err(ureq::Error::ConnectionFailed),
        }
    }
}

impl Connector<()> for PinnedTcpConnector {
    type Out = PinnedTcpTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let global_deadline = details.config.timeouts().global.map(|d| Instant::now() + d);
        self.connect_with_deadline(details, global_deadline)
    }
}

impl Connector<()> for DeadlinePinnedTcpConnector {
    type Out = PinnedTcpTransport;

    fn connect(
        &self,
        details: &ConnectionDetails,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        self.inner
            .connect_with_deadline(details, self.global_deadline)
    }
}
