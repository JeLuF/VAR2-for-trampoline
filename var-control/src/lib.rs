//! Remote control interface for Bitfocus Companion (INP-003).
//!
//! A plain text protocol over TCP, so Companion's "Generic TCP/UDP" module (and later
//! a Stream Deck) can send commands without a dedicated module: one command per line,
//! case-insensitive, answered with one line. The server listens on all network
//! interfaces, so Companion can run on another computer; it is disabled by default.
//!
//! Only review, marker and view functions are available, the same as in locked mode:
//! nothing can start or stop the recording, change settings or quit the application.
//!
//! | Command | Effect |
//! |---|---|
//! | `PING` | Answers `PONG` |
//! | `PLAY` | Play / pause (forwards) |
//! | `REVERSE` | Play backwards / pause |
//! | `PAUSE` | Pause |
//! | `STEP <n>` | `n` frames forward or back, e.g. `STEP -1` |
//! | `JUMP <s>` | `s` seconds forward or back, e.g. `JUMP -10` |
//! | `FASTER`, `SLOWER` | Next speed |
//! | `SPEED <x>` | Speed 2, 1, 0.5, 0.25 or 0.1 |
//! | `REVIEW`, `LIVE` | Review mode / back to live |
//! | `MARKER` | Set a marker |
//! | `MARKER PREV`, `MARKER NEXT` | Previous / next marker |
//! | `CAMERA <n>` | Camera `n` (1–4) full screen / back |
//! | `GRID` | Back to the grid |
//!
//! Answers: `OK` (accepted), `PONG`, or `ERR <reason>`.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{debug, info, warn};

/// Default TCP port.
pub const DEFAULT_PORT: u16 = 7450;
/// Connections at the same time.
const MAX_CONNECTIONS: usize = 8;
/// Longest accepted command line.
const MAX_LINE: usize = 256;
/// How often blocked calls check whether the server is stopping.
const POLL: Duration = Duration::from_millis(100);

/// Playback speeds that `SPEED` accepts, in thousandths.
const SPEEDS: [u32; 5] = [2000, 1000, 500, 250, 100];

/// A command received from a remote control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    PlayPause,
    PlayReverse,
    Pause,
    Step(i32),
    Jump(i64),
    Faster,
    Slower,
    /// Speed in thousandths: 2000 = 2×, 100 = 0.1×.
    Speed(u32),
    Review,
    Live,
    SetMarker,
    PreviousMarker,
    NextMarker,
    /// Camera slot (0-based) full screen / back.
    Camera(usize),
    Grid,
}

/// Parses one command line. `Ok(None)` for `PING` and empty lines.
pub fn parse(line: &str) -> Result<Option<Command>, String> {
    let line = line.trim().to_ascii_uppercase();
    let words: Vec<&str> = line.split_whitespace().collect();
    let number = |name: &str| -> Result<i64, String> {
        match words.as_slice() {
            [_, n] => n.parse().map_err(|_| format!("{name} needs a number")),
            _ => Err(format!("{name} needs one number")),
        }
    };
    let command = match words.first().copied() {
        None | Some("PING") => return Ok(None),
        Some("PLAY") => Command::PlayPause,
        Some("REVERSE") => Command::PlayReverse,
        Some("PAUSE") => Command::Pause,
        Some("STEP") => match number("STEP")? {
            n @ (-100..=-1 | 1..=100) => Command::Step(n as i32),
            _ => return Err("STEP needs -100 to 100 frames, not 0".into()),
        },
        Some("JUMP") => match number("JUMP")? {
            s @ (-36_000..=-1 | 1..=36_000) => Command::Jump(s),
            _ => return Err("JUMP needs -36000 to 36000 seconds, not 0".into()),
        },
        Some("FASTER") => Command::Faster,
        Some("SLOWER") => Command::Slower,
        Some("SPEED") => {
            let speed: f64 = match words.as_slice() {
                [_, x] => x
                    .trim_end_matches('X')
                    .parse()
                    .map_err(|_| "SPEED needs a number".to_owned())?,
                _ => return Err("SPEED needs one number".into()),
            };
            let thousandths = (speed * 1000.0).round() as u32;
            if !SPEEDS.contains(&thousandths) {
                return Err("SPEED must be 2, 1, 0.5, 0.25 or 0.1".into());
            }
            Command::Speed(thousandths)
        }
        Some("REVIEW") => Command::Review,
        Some("LIVE") => Command::Live,
        Some("MARKER") => match words.get(1).copied() {
            None | Some("SET") => Command::SetMarker,
            Some("PREV" | "PREVIOUS") => Command::PreviousMarker,
            Some("NEXT") => Command::NextMarker,
            Some(_) => return Err("MARKER takes PREV or NEXT".into()),
        },
        Some("CAMERA") => match number("CAMERA")? {
            n @ 1..=4 => Command::Camera(n as usize - 1),
            _ => return Err("CAMERA needs 1 to 4".into()),
        },
        Some("GRID") => Command::Grid,
        Some(other) => return Err(format!("unknown command {other}")),
    };
    // Extra words after a command without arguments are an error, so typos show.
    let arguments = match command {
        Command::Step(_)
        | Command::Jump(_)
        | Command::Speed(_)
        | Command::Camera(_)
        | Command::PreviousMarker
        | Command::NextMarker => 1,
        Command::SetMarker => words.len() - 1,
        _ => 0,
    };
    if words.len() > 1 + arguments {
        return Err("too many words".into());
    }
    Ok(Some(command))
}

/// The TCP server. Stops when dropped.
pub struct ControlServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    /// Listens on `port` on all network interfaces (0 picks a free port). Every
    /// valid command is passed to `handler` with the sender's address, on a
    /// connection thread.
    pub fn start(
        port: u16,
        handler: impl Fn(Command, SocketAddr) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<dyn Fn(Command, SocketAddr) + Send + Sync> = Arc::new(handler);
        let thread = std::thread::Builder::new()
            .name("var-remote-control".into())
            .spawn({
                let stop = Arc::clone(&stop);
                move || accept_loop(listener, &stop, &handler)
            })?;
        info!(%address, "remote control listening");
        Ok(ControlServer {
            address,
            stop,
            thread: Some(thread),
        })
    }

    /// The address the server listens on (`0.0.0.0:<port>`).
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        info!(address = %self.address, "remote control stopped");
    }
}

fn accept_loop(
    listener: TcpListener,
    stop: &Arc<AtomicBool>,
    handler: &Arc<dyn Fn(Command, SocketAddr) + Send + Sync>,
) {
    let connections = Arc::new(AtomicUsize::new(0));
    let mut threads: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    warn!(%peer, "remote control: too many connections");
                    let _ = stream.write_all(b"ERR too many connections\r\n");
                    continue;
                }
                info!(%peer, "remote control connected");
                connections.fetch_add(1, Ordering::Relaxed);
                let spawned = std::thread::Builder::new()
                    .name("var-remote-connection".into())
                    .spawn({
                        let stop = Arc::clone(stop);
                        let handler = Arc::clone(handler);
                        let connections = Arc::clone(&connections);
                        move || {
                            if let Err(e) = serve(stream, peer, &stop, &*handler) {
                                debug!(%peer, %e, "remote control connection error");
                            }
                            info!(%peer, "remote control disconnected");
                            connections.fetch_sub(1, Ordering::Relaxed);
                        }
                    });
                match spawned {
                    Ok(thread) => threads.push(thread),
                    Err(e) => {
                        warn!(%e, "cannot start remote control connection");
                        connections.fetch_sub(1, Ordering::Relaxed);
                    }
                }
                threads.retain(|t| !t.is_finished());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) => {
                warn!(%e, "remote control: accept failed");
                std::thread::sleep(POLL);
            }
        }
    }
    for thread in threads {
        let _ = thread.join();
    }
}

/// Reads command lines from one connection and answers each.
fn serve(
    mut stream: TcpStream,
    peer: SocketAddr,
    stop: &AtomicBool,
    handler: &(dyn Fn(Command, SocketAddr) + Send + Sync),
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_nodelay(true)?;
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 512];
    // Set when a line got too long: its rest is skipped up to the next line end.
    let mut skipping = false;
    while !stop.load(Ordering::Relaxed) {
        let read = match stream.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        pending.extend_from_slice(&buffer[..read]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if std::mem::take(&mut skipping) {
                continue;
            }
            let line = String::from_utf8_lossy(&line);
            let answer = match parse(&line) {
                Ok(Some(command)) => {
                    debug!(%peer, ?command, "remote control command");
                    handler(command, peer);
                    "OK".to_owned()
                }
                Ok(None) if line.trim().is_empty() => continue,
                Ok(None) => "PONG".to_owned(),
                Err(e) => {
                    debug!(%peer, line = %line.trim(), %e, "remote control: invalid command");
                    format!("ERR {e}")
                }
            };
            stream.write_all(answer.as_bytes())?;
            stream.write_all(b"\r\n")?;
        }
        if pending.len() > MAX_LINE {
            pending.clear();
            skipping = true;
            stream.write_all(b"ERR line too long\r\n")?;
        }
    }
    Ok(())
}

/// The computer's address in the local network, for showing where to connect
/// Companion. Asks the OS which interface it would use; no data is sent. `None`
/// without a network.
pub fn local_address() -> Option<std::net::IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // A documentation address (TEST-NET-1): only selects the route.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::Mutex;

    #[test]
    fn parses_commands() {
        assert_eq!(parse("PING"), Ok(None));
        assert_eq!(parse(""), Ok(None));
        assert_eq!(parse("play\r\n"), Ok(Some(Command::PlayPause)));
        assert_eq!(parse("Reverse"), Ok(Some(Command::PlayReverse)));
        assert_eq!(parse("STEP -1"), Ok(Some(Command::Step(-1))));
        assert_eq!(parse("step 5"), Ok(Some(Command::Step(5))));
        assert_eq!(parse("JUMP -10"), Ok(Some(Command::Jump(-10))));
        assert_eq!(parse("JUMP 60"), Ok(Some(Command::Jump(60))));
        assert_eq!(parse("SPEED 0.25"), Ok(Some(Command::Speed(250))));
        assert_eq!(parse("speed 2x"), Ok(Some(Command::Speed(2000))));
        assert_eq!(parse("MARKER"), Ok(Some(Command::SetMarker)));
        assert_eq!(parse("marker next"), Ok(Some(Command::NextMarker)));
        assert_eq!(parse("MARKER PREV"), Ok(Some(Command::PreviousMarker)));
        assert_eq!(parse("CAMERA 2"), Ok(Some(Command::Camera(1))));
        assert_eq!(parse("GRID"), Ok(Some(Command::Grid)));
        assert_eq!(parse("REVIEW"), Ok(Some(Command::Review)));
        assert_eq!(parse("LIVE"), Ok(Some(Command::Live)));
    }

    #[test]
    fn rejects_invalid_commands() {
        for line in [
            "STOP",
            "QUIT",
            "STEP",
            "STEP 0",
            "STEP x",
            "JUMP",
            "SPEED 3",
            "CAMERA 5",
            "CAMERA 0",
            "PLAY NOW",
            "MARKER DELETE",
        ] {
            assert!(parse(line).is_err(), "{line} accepted");
        }
    }

    #[test]
    fn serves_commands_over_tcp() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let server = ControlServer::start(0, {
            let received = Arc::clone(&received);
            move |command, _| received.lock().unwrap().push(command)
        })
        .unwrap();
        let port = server.address().port();
        assert!(
            server.address().ip().is_unspecified(),
            "listens on all interfaces"
        );

        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut ask = |line: &str| {
            writer.write_all(line.as_bytes()).unwrap();
            let mut answer = String::new();
            reader.read_line(&mut answer).unwrap();
            answer.trim_end().to_owned()
        };
        assert_eq!(ask("PING\n"), "PONG");
        assert_eq!(ask("PLAY\r\n"), "OK");
        assert_eq!(ask("STEP -1\n"), "OK");
        assert!(ask("STOP\n").starts_with("ERR"));
        assert!(ask(&format!("{}\n", "X".repeat(600))).starts_with("ERR"));
        assert_eq!(ask("MARKER\n"), "OK");
        assert_eq!(
            *received.lock().unwrap(),
            vec![Command::PlayPause, Command::Step(-1), Command::SetMarker]
        );
        drop(server);
    }
}
