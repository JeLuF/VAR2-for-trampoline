//! Remote control for Bitfocus Companion (INP-003): runs the `var-control` TCP server
//! while enabled and turns its commands into the same actions as the keyboard
//! shortcuts. Only review, marker and view functions are reachable, as in locked mode.

use std::net::IpAddr;
use std::sync::mpsc;

use eframe::egui;
use tracing::error;
use var_control::{Command, ControlServer};

use crate::config::RemoteControlConfig;
use crate::shortcuts::Action;

pub struct RemoteControl {
    /// What the operator set: on or off, and the port.
    pub settings: RemoteControlConfig,
    /// The running server and the port it was asked for (0 in tests: any free port).
    server: Option<(ControlServer, u16)>,
    /// All network interfaces; the loopback address in tests (no firewall prompt).
    bind_ip: IpAddr,
    /// The port the server could not be started on, with the reason; not retried
    /// until the settings change.
    error: Option<(u16, String)>,
    local_address: Option<IpAddr>,
    sender: mpsc::Sender<Command>,
    commands: mpsc::Receiver<Command>,
    ctx: egui::Context,
}

impl RemoteControl {
    pub fn new(ctx: &egui::Context, settings: RemoteControlConfig) -> Self {
        let (sender, commands) = mpsc::channel();
        RemoteControl {
            settings,
            server: None,
            bind_ip: var_control::listen_address(0).ip(),
            error: None,
            local_address: None,
            sender,
            commands,
            ctx: ctx.clone(),
        }
    }

    /// Starts or stops the server to match the settings, and returns the commands
    /// received since the last call. Call once per UI frame.
    pub fn update(&mut self) -> Vec<Action> {
        let wanted = self.settings.enabled.then_some(self.settings.port);
        let running = self.server.as_ref().map(|(_, port)| *port);
        if running.is_some() && running != wanted {
            self.server = None;
        }
        if wanted.is_none() {
            self.error = None;
        }
        if let Some(port) = wanted
            && self.server.is_none()
            && self.error.as_ref().is_none_or(|(p, _)| *p != port)
        {
            let sender = self.sender.clone();
            let ctx = self.ctx.clone();
            match ControlServer::start_on((self.bind_ip, port).into(), move |command, _peer| {
                let _ = sender.send(command);
                ctx.request_repaint();
            }) {
                Ok(server) => {
                    self.server = Some((server, port));
                    self.error = None;
                    self.local_address = var_control::local_address();
                }
                Err(e) => {
                    error!(port, %e, "cannot start the remote control");
                    self.error = Some((port, e.to_string()));
                }
            }
        }
        self.commands.try_iter().map(to_action).collect()
    }

    /// Where Companion connects, or why it can't; `None` when disabled.
    pub fn status(&self) -> Option<Result<String, String>> {
        if let Some((port, e)) = &self.error {
            return Some(Err(format!("Port {port}: {e}")));
        }
        let (server, _) = self.server.as_ref()?;
        let port = server.address().port();
        Some(Ok(match self.local_address {
            Some(ip) => format!("Listening on {ip}:{port} (all network interfaces)"),
            None => format!("Listening on port {port} (all network interfaces)"),
        }))
    }
}

/// The action of a remote control command.
pub fn to_action(command: Command) -> Action {
    match command {
        Command::PlayPause => Action::PlayPause,
        Command::PlayReverse => Action::PlayReverse,
        Command::Pause => Action::Pause,
        Command::Step(frames) => Action::Step(frames),
        Command::Jump(seconds) => Action::Jump(seconds),
        Command::Faster => Action::Faster,
        Command::Slower => Action::Slower,
        Command::Speed(thousandths) => Action::Speed(thousandths),
        Command::Review => Action::Review,
        Command::Live => Action::Live,
        Command::SetMarker => Action::SetMarker,
        Command::PreviousMarker => Action::PreviousMarker,
        Command::NextMarker => Action::NextMarker,
        Command::Camera(slot) => Action::FullScreen(slot),
        Command::Grid => Action::ExitFullScreen,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn enabling_starts_and_disabling_stops_the_server() {
        let ctx = egui::Context::default();
        let mut remote = RemoteControl::new(
            &ctx,
            RemoteControlConfig {
                enabled: false,
                port: 0,
            },
        );
        // Loopback only in the test: no Windows Firewall prompt.
        remote.bind_ip = std::net::Ipv4Addr::LOCALHOST.into();
        assert!(remote.update().is_empty());
        assert!(remote.status().is_none(), "disabled by default");

        remote.settings.enabled = true;
        remote.update();
        let port = remote.server.as_ref().unwrap().0.address().port();
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        writer.write_all(b"JUMP -10\nCAMERA 1\n").unwrap();
        let mut answer = String::new();
        reader.read_line(&mut answer).unwrap();
        reader.read_line(&mut answer).unwrap();
        assert_eq!(answer, "OK\r\nOK\r\n");
        assert_eq!(
            remote.update(),
            vec![Action::Jump(-10), Action::FullScreen(0)]
        );

        remote.settings.enabled = false;
        remote.update();
        assert!(remote.status().is_none());
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }
}
