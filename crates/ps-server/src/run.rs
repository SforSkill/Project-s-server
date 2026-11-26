//! Socket loop: one receive thread, one fixed-rate simulation thread.

use crate::config::ServerConfig;
use crate::server::Server;
use crate::stats::Status;
use game_sim::GameData;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Requests from outside the simulation thread (admin API, console).
pub enum Control {
    Command(String, Sender<String>),
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    pub stop: Arc<AtomicBool>,
    pub status: Arc<Mutex<Option<Status>>>,
    pub control: Sender<Control>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn command(&self, line: &str) -> Option<String> {
        let (tx, rx) = mpsc::channel();
        self.control.send(Control::Command(line.into(), tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(2)).ok()
    }

    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Binds the socket and starts the server threads.
pub fn start(cfg: ServerConfig, data: Arc<GameData>) -> Result<ServerHandle, String> {
    let socket = UdpSocket::bind(&cfg.bind).map_err(|e| format!("UDP {} : {e}", cfg.bind))?;
    let addr = socket.local_addr().map_err(|e| e.to_string())?;
    let server = Server::new(cfg, data)?;
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(Some(server.status())));
    let (ctl_tx, ctl_rx) = mpsc::channel();
    let thread = {
        let stop = stop.clone();
        let status = status.clone();
        std::thread::Builder::new()
            .name("projet-s-sim".into())
            .spawn(move || sim_loop(server, socket, stop, status, ctl_rx))
            .map_err(|e| e.to_string())?
    };
    Ok(ServerHandle {
        addr,
        stop,
        status,
        control: ctl_tx,
        thread: Some(thread),
    })
}

/// Local listen server for the client (loopback only, random port).
pub fn spawn_local(mut cfg: ServerConfig, data: Arc<GameData>) -> Result<ServerHandle, String> {
    cfg.bind = "127.0.0.1:0".into();
    cfg.admin_bind = None;
    cfg.listen_host = true;
    start(cfg, data)
}

fn sim_loop(
    mut server: Server,
    socket: UdpSocket,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<Option<Status>>>,
    control: Receiver<Control>,
) {
    let epoch = Instant::now();
    let (tx, rx) = mpsc::channel::<(SocketAddr, Vec<u8>, f64)>();
    let rx_socket = socket.try_clone().expect("socket clone");
    let _ = rx_socket.set_read_timeout(Some(Duration::from_millis(100)));
    let rx_stop = stop.clone();
    let receiver = std::thread::Builder::new()
        .name("projet-s-rx".into())
        .spawn(move || {
            let mut buf = [0u8; 2048];
            while !rx_stop.load(Ordering::Relaxed) {
                match rx_socket.recv_from(&mut buf) {
                    Ok((n, from)) => {
                        if tx
                            .send((from, buf[..n].to_vec(), epoch.elapsed().as_secs_f64()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    // Windows reports ICMP port unreachable as ConnectionReset: ignore.
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Err(e) => {
                        tracing::warn!("réception UDP : {e}");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        })
        .expect("rx thread");
    let dt = server.sim.dt();
    let mut next = 0.0f64;
    let mut out = Vec::new();
    let mut last_status = 0.0;
    tracing::info!(
        "serveur prêt sur {} — {} Hz, carte {}, mode {}",
        socket
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default(),
        server.sim.rate.hz(),
        server.cfg.map,
        server.cfg.profile
    );
    while !stop.load(Ordering::Relaxed) {
        // Wait for the next tick deadline: sleep coarse, then spin briefly.
        loop {
            let now = epoch.elapsed().as_secs_f64();
            let wait = next - now;
            if wait <= 0.0 {
                break;
            }
            if wait > 0.0015 {
                std::thread::sleep(Duration::from_secs_f64(wait - 0.001));
            } else {
                std::hint::spin_loop();
            }
        }
        let now = epoch.elapsed().as_secs_f64();
        if now - next > dt {
            server.stats.late_ticks += 1;
        }
        // Far behind (debugger, suspended VM): drop the backlog instead of fast-forwarding.
        if now - next > 0.25 {
            next = now;
        }
        while let Ok((from, pkt, t)) = rx.try_recv() {
            server.receive(from, &pkt, t, &mut out);
        }
        while let Ok(c) = control.try_recv() {
            match c {
                Control::Command(line, reply) => {
                    let r = server.command(&line);
                    let _ = reply.send(r);
                }
            }
        }
        server.tick(now, &mut out);
        for (to, p) in out.drain(..) {
            let _ = socket.send_to(&p, to);
        }
        if now - last_status > 0.5 {
            last_status = now;
            if let Ok(mut s) = status.lock() {
                *s = Some(server.status());
            }
        }
        next += dt;
    }
    let now = epoch.elapsed().as_secs_f64();
    server.shutdown(now, &mut out);
    for (to, p) in out.drain(..) {
        let _ = socket.send_to(&p, to);
    }
    let _ = receiver.join();
    tracing::info!("serveur arrêté");
}
