//! The HTTP client every download goes through. There's one for the whole
//! session, so a connection to a server is kept open and used again for the
//! next download from it, as browsers do.
//!
//! Hosts often have both an IPv6 and an IPv4 address, and on some networks
//! one of them doesn't work. Trying them one after another waits out the
//! broken one each time, so they're tried together instead, each a moment
//! after the last, and the first to connect is used: "Happy Eyeballs", as
//! RFC 8305 calls it. The winner is remembered for a while, so later
//! downloads from the host go straight to it.

use std::{
    collections::HashMap,
    fmt,
    net::{SocketAddr, TcpStream},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use ureq::{
    Agent,
    config::Config,
    http::Uri,
    unversioned::{
        resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
        transport::{DefaultConnector, NextTimeout},
    },
};

/// How long to wait for a connection to a server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a download can take, from start to finish.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long after one address the next is tried, if the first hasn't connected.
const STAGGER: Duration = Duration::from_millis(250);
/// How long the address that won a race is used without racing again.
const REMEMBER: Duration = Duration::from_secs(10 * 60);

pub fn agent() -> &'static Agent {
    static AGENT: LazyLock<Agent> = LazyLock::new(|| {
        let config = Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(TOTAL_TIMEOUT))
            .user_agent("toy-browser")
            .build();
        Agent::with_parts(config, DefaultConnector::new(), RacingResolver)
    });
    &AGENT
}

/// Looks up a host's addresses, then puts first the one that connects soonest.
struct RacingResolver;

/// The address that last won a race for each host, and when.
static WINNERS: LazyLock<Mutex<HashMap<String, (SocketAddr, Instant)>>> = LazyLock::new(Mutex::default);

impl fmt::Debug for RacingResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RacingResolver")
    }
}

impl Resolver for RacingResolver {
    fn resolve(&self, uri: &Uri, config: &Config, timeout: NextTimeout) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = DefaultResolver::default().resolve(uri, config, timeout)?;
        let host = uri.authority().map_or_else(String::new, |authority| authority.to_string());
        let remembered = WINNERS.lock().unwrap().get(&host).filter(|(_, when)| when.elapsed() < REMEMBER).map(|(addr, _)| *addr);
        let winner = match remembered.and_then(|addr| addrs.iter().position(|a| *a == addr)) {
            Some(i) => Some(i),
            None => race(&addrs, CONNECT_TIMEOUT).inspect(|&i| _ = WINNERS.lock().unwrap().insert(host, (addrs[i], Instant::now()))),
        };
        if let Some(i) = winner {
            // The client connects to the addresses in order, so it gets this one at once.
            addrs[..=i].rotate_right(1);
        }
        Ok(addrs)
    }
}

/// Tries to connect to each of `addrs`, starting one every `STAGGER`
/// until one connects, and returns which connected first. Returns `None`
/// if there's only one, so nothing to choose, or none connect.
fn race(addrs: &[SocketAddr], timeout: Duration) -> Option<usize> {
    if addrs.len() < 2 {
        return None;
    }
    let (sender, receiver) = mpsc::channel();
    let won = Arc::new(AtomicBool::new(false));
    for (n, i) in interleaved(addrs).into_iter().enumerate() {
        let (addr, sender, won) = (addrs[i], sender.clone(), won.clone());
        thread::spawn(move || {
            thread::sleep(STAGGER * n as u32);
            // The connection is only a test, and is closed again at once.
            if !won.load(Ordering::Relaxed) && TcpStream::connect_timeout(&addr, timeout).is_ok() {
                won.store(true, Ordering::Relaxed);
                _ = sender.send(i);
            }
        });
    }
    // Once every attempt has failed, nothing can send, and this stops waiting.
    drop(sender);
    let winner = receiver.recv_timeout(timeout + STAGGER * addrs.len() as u32).ok();
    won.store(true, Ordering::Relaxed);
    winner
}

/// The positions of `addrs`, taking IPv6 and IPv4 addresses in turn, so
/// that the second address tried is the other kind of the first.
fn interleaved(addrs: &[SocketAddr]) -> Vec<usize> {
    let (mut v6, mut v4): (Vec<usize>, Vec<usize>) = (0..addrs.len()).partition(|&i| addrs[i].is_ipv6());
    if addrs[0].is_ipv4() {
        (v6, v4) = (v4, v6);
    }
    let mut order = Vec::with_capacity(addrs.len());
    let (mut first, mut second) = (v6.into_iter(), v4.into_iter());
    loop {
        match (first.next(), second.next()) {
            (None, None) => return order,
            (a, b) => order.extend(a.into_iter().chain(b)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn families_take_turns() {
        let addrs: Vec<SocketAddr> = ["[::1]:1", "[::2]:1", "[::3]:1", "1.0.0.1:1", "1.0.0.2:1"].map(|a| a.parse().unwrap()).into();
        assert_eq!(interleaved(&addrs), [0, 3, 1, 4, 2]);
    }

    #[test]
    fn the_address_that_answers_wins() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // A port that was just free, so nothing answers there.
        let refused = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let addrs = [refused, listener.local_addr().unwrap()];
        assert_eq!(race(&addrs, Duration::from_secs(2)), Some(1));
        assert_eq!(race(&addrs[..1], Duration::from_secs(2)), None, "one address needs no race");
        assert_eq!(race(&[refused, refused], Duration::from_secs(2)), None);
    }
}
