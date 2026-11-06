//! TCP helpers for talking to the Blender bootstrap, plus the status poller.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::AppHandle;

const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(2000);

fn connect(port: u16, timeout: Duration) -> std::io::Result<TcpStream> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let s = TcpStream::connect_timeout(&addr, timeout)?;
    // Python's create_connection(timeout=..) applies to recv/send as well.
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    Ok(s)
}

fn exchange(port: u16, msg: &str, timeout: Duration, max: usize) -> std::io::Result<String> {
    let mut s = connect(port, timeout)?;
    s.write_all(format!("{msg}
").as_bytes())?;
    let mut buf = vec![0u8; max];
    let n = s.read(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).trim().to_string())
}

pub fn tcp_send(port: u16, msg: &str, timeout: Duration) -> String {
    match exchange(port, msg, timeout, 1024) {
        Ok(r) => r,
        Err(e) => format!("error: {e}"),
    }
}

pub fn is_running(port: u16) -> bool {
    exchange(port, "ping", PROBE_TIMEOUT, 64)
        .map(|r| r.starts_with("pong"))
        .unwrap_or(false)
}

pub fn probe(ports: &[u16]) -> BTreeMap<u16, bool> {
    let mut out = BTreeMap::new();
    if ports.is_empty() {
        return out;
    }
    let workers = ports.len().clamp(1, 32);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(ports.len()));
    std::thread::scope(|sc| {
        for _ in 0..workers {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(&p) = ports.get(i) else { break };
                let r = is_running(p);
                results.lock().unwrap().push((p, r));
            });
        }
    });
    out.extend(results.into_inner().unwrap());
    out
}

/// Probe loop: once immediately, then every `interval`. A tick is skipped
/// while the previous probe is still running. Empty port list emits nothing.
fn poller_loop<P, E>(
    ports: Arc<Mutex<Vec<u16>>>,
    stop: Arc<AtomicBool>,
    interval: Duration,
    probe: P,
    emit: E,
) where
    P: Fn(&[u16]) -> BTreeMap<u16, bool> + Send + Sync + 'static,
    E: Fn(BTreeMap<u16, bool>) + Send + Sync + 'static,
{
    let probe = Arc::new(probe);
    let emit = Arc::new(emit);
    let busy = Arc::new(AtomicBool::new(false));
    while !stop.load(Ordering::SeqCst) {
        if !busy.swap(true, Ordering::SeqCst) {
            let snapshot = ports.lock().map(|p| p.clone()).unwrap_or_default();
            if snapshot.is_empty() {
                busy.store(false, Ordering::SeqCst);
            } else {
                let (probe, emit, busy, stop) =
                    (probe.clone(), emit.clone(), busy.clone(), stop.clone());
                std::thread::spawn(move || {
                    let r = probe(&snapshot);
                    if !stop.load(Ordering::SeqCst) {
                        emit(r);
                    }
                    busy.store(false, Ordering::SeqCst);
                });
            }
        }
        // sleep in small steps so stop is honoured promptly
        let mut left = interval;
        while !left.is_zero() && !stop.load(Ordering::SeqCst) {
            let step = left.min(Duration::from_millis(20));
            std::thread::sleep(step);
            left -= step;
        }
    }
}

pub fn start_poller(app: AppHandle, ports: Arc<Mutex<Vec<u16>>>) {
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::spawn(move || {
        poller_loop(ports, stop, POLL_INTERVAL, probe, move |m| {
            crate::commands::publish_status(&app, Some(m));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::thread::{self, JoinHandle};
    use std::time::Instant;

    /// Fake bootstrap: accepts one connection, reads a line, replies via `f`
    /// (None = stay silent until the client hangs up).
    fn fake_server(f: fn(&str) -> Option<&'static str>) -> (u16, JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = [0u8; 256];
            let n = s.read(&mut buf).unwrap_or(0);
            let line = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            if let Some(r) = f(&line) {
                let _ = s.write_all(r.as_bytes());
            } else {
                // silent: wait for client to close (or timeout)
                let _ = s.read(&mut buf);
            }
        });
        (port, h)
    }

    fn closed_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[test]
    fn ping_pong_is_running() {
        let (port, h) = fake_server(|m| if m == "ping" { Some("pong\n") } else { None });
        assert!(is_running(port));
        h.join().unwrap();
    }

    #[test]
    fn silent_listener_is_not_running() {
        let (port, h) = fake_server(|_| None);
        let t = Instant::now();
        assert!(!is_running(port));
        let e = t.elapsed();
        assert!(e >= Duration::from_millis(800) && e < Duration::from_millis(2500), "{e:?}");
        h.join().unwrap();
    }

    #[test]
    fn closed_port_is_not_running() {
        assert!(!is_running(closed_port()));
    }

    #[test]
    fn tcp_send_reload_returns_reply() {
        let (port, h) = fake_server(|m| if m == "reload" { Some("ok reloaded\n") } else { Some("?") });
        assert_eq!(tcp_send(port, "reload", Duration::from_secs(3)), "ok reloaded");
        h.join().unwrap();
    }

    #[test]
    fn tcp_send_failure_formats_error_prefix() {
        let r = tcp_send(closed_port(), "reload", Duration::from_secs(1));
        assert!(r.starts_with("error: "), "{r}");
    }

    #[test]
    fn probe_runs_in_parallel() {
        let ports: Vec<u16> = (0..20).map(|_| closed_port()).collect();
        let t = Instant::now();
        let r = probe(&ports);
        assert!(t.elapsed() < Duration::from_secs(3));
        assert_eq!(r.len(), ports.len());
        assert!(r.values().all(|v| !v));
    }

    #[test]
    fn probe_keys_by_port() {
        let (port, h) = fake_server(|_| Some("pong"));
        let dead = closed_port();
        let r = probe(&[port, dead]);
        assert_eq!(r.get(&port), Some(&true));
        assert_eq!(r.get(&dead), Some(&false));
        h.join().unwrap();
    }

    #[test]
    fn poller_emits_immediately_and_skips_overlap() {
        let ports = Arc::new(Mutex::new(vec![1u16, 2]));
        let stop = Arc::new(AtomicBool::new(false));
        let probes = Arc::new(AtomicUsize::new(0));
        let emits = Arc::new(AtomicUsize::new(0));
        let (p2, e2) = (probes.clone(), emits.clone());
        let h = {
            let (ports, stop) = (ports.clone(), stop.clone());
            thread::spawn(move || {
                poller_loop(
                    ports,
                    stop,
                    Duration::from_millis(50),
                    move |ps| {
                        p2.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(400)); // much slower than interval
                        ps.iter().map(|p| (*p, true)).collect()
                    },
                    move |m| {
                        assert_eq!(m.len(), 2);
                        e2.fetch_add(1, Ordering::SeqCst);
                    },
                )
            })
        };
        thread::sleep(Duration::from_millis(200));
        // first probe started immediately, others skipped while it runs
        assert_eq!(probes.load(Ordering::SeqCst), 1);
        assert_eq!(emits.load(Ordering::SeqCst), 0);
        thread::sleep(Duration::from_millis(400));
        assert!(emits.load(Ordering::SeqCst) >= 1);
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
    }

    #[test]
    fn poller_empty_ports_emits_nothing() {
        let ports = Arc::new(Mutex::new(Vec::<u16>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let emits = Arc::new(AtomicUsize::new(0));
        let e2 = emits.clone();
        let h = {
            let (ports, stop) = (ports.clone(), stop.clone());
            thread::spawn(move || {
                poller_loop(ports, stop, Duration::from_millis(20), |_| BTreeMap::new(), move |_| {
                    e2.fetch_add(1, Ordering::SeqCst);
                })
            })
        };
        thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::SeqCst);
        h.join().unwrap();
        assert_eq!(emits.load(Ordering::SeqCst), 0);
    }
}
