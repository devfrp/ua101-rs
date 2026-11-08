//! Client of the `ua101d` control socket. When the daemon runs, the panel
//! controls the mixer through it and never takes the device itself, so the
//! sound card stays available to the system.

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    sync::mpsc::{self, Receiver},
    thread,
};

use eframe::egui;

pub enum DaemonEvent {
    Info {
        rate: Option<u32>,
        connected: bool,
    },
    Data([u8; 4], Vec<u8>),
    Levels {
        inputs: Vec<f32>,
        outputs: Vec<f32>,
    },
    /// Software patch bay: enabled, and per output the cabled input
    /// (1-based, 0 = none).
    Patch {
        on: bool,
        routes: Vec<usize>,
    },
    Error(String),
    Closed,
}

pub struct DaemonLink {
    writer: UnixStream,
    pub events: Receiver<DaemonEvent>,
}

fn parse_bytes(s: &str) -> Option<Vec<u8>> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).ok())
        .collect()
}

fn parse_levels(s: &str) -> Vec<f32> {
    s.split(',').filter_map(|v| v.parse().ok()).collect()
}

fn parse_line(line: &str) -> Option<DaemonEvent> {
    let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
    match kind {
        "info" => {
            let mut rate = None;
            let mut connected = false;
            for kv in rest.split_whitespace() {
                match kv.split_once('=') {
                    Some(("rate", v)) => rate = v.parse().ok(),
                    Some(("connected", v)) => connected = v == "1",
                    _ => {}
                }
            }
            Some(DaemonEvent::Info { rate, connected })
        }
        "dt1" => {
            let b = parse_bytes(rest)?;
            (b.len() >= 5).then(|| DaemonEvent::Data([b[0], b[1], b[2], b[3]], b[4..].to_vec()))
        }
        "levels" => {
            let rest = rest.strip_prefix("in ")?;
            let (i, o) = rest.split_once(" out ")?;
            Some(DaemonEvent::Levels {
                inputs: parse_levels(i),
                outputs: parse_levels(o),
            })
        }
        "patch" => {
            let (state, routes) = rest.split_once(' ')?;
            let routes: Option<Vec<usize>> = routes.split(',').map(|r| r.parse().ok()).collect();
            Some(DaemonEvent::Patch {
                on: state == "on",
                routes: routes?,
            })
        }
        "err" => Some(DaemonEvent::Error(rest.to_string())),
        _ => None,
    }
}

impl DaemonLink {
    /// Connects to a running daemon, or returns `None`.
    pub fn connect(ctx: egui::Context) -> Option<Self> {
        let path = ua101_usb::daemon_socket_path();
        let mut writer = UnixStream::connect(&path).ok()?;
        let reader = writer.try_clone().ok()?;
        let (tx, events) = mpsc::channel();
        thread::Builder::new()
            .name("ua101d-client".into())
            .spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    if let Some(ev) = parse_line(line.trim()) {
                        let is_levels = matches!(ev, DaemonEvent::Levels { .. });
                        if tx.send(ev).is_err() {
                            return;
                        }
                        if !is_levels {
                            ctx.request_repaint();
                        } else {
                            ctx.request_repaint_after(std::time::Duration::from_millis(30));
                        }
                    }
                }
                let _ = tx.send(DaemonEvent::Closed);
                ctx.request_repaint();
            })
            .ok()?;
        writeln!(writer, "hello").ok()?;
        Some(Self { writer, events })
    }

    /// Sends a raw command line (`patch on`, `route 3 1`…).
    pub fn command(&mut self, line: &str) -> bool {
        writeln!(self.writer, "{line}").is_ok()
    }

    pub fn write(&mut self, address: [u8; 4], data: &[u8]) -> bool {
        let hex: Vec<String> = address
            .iter()
            .chain(data)
            .map(|b| format!("{b:02x}"))
            .collect();
        writeln!(self.writer, "dt1 {}", hex.join(" ")).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_lines() {
        assert!(matches!(
            parse_line("info rate=44100 inputs=12 outputs=10 connected=1"),
            Some(DaemonEvent::Info {
                rate: Some(44100),
                connected: true
            })
        ));
        match parse_line("dt1 00 01 00 05 01") {
            Some(DaemonEvent::Data(a, d)) => {
                assert_eq!(a, [0, 1, 0, 5]);
                assert_eq!(d, vec![1]);
            }
            _ => panic!(),
        }
        match parse_line("patch on 0,3,0") {
            Some(DaemonEvent::Patch { on, routes }) => {
                assert!(on);
                assert_eq!(routes, vec![0, 3, 0]);
            }
            _ => panic!(),
        }
        match parse_line("levels in 0.5,0.25 out 0.1") {
            Some(DaemonEvent::Levels { inputs, outputs }) => {
                assert_eq!(inputs, vec![0.5, 0.25]);
                assert_eq!(outputs, vec![0.1]);
            }
            _ => panic!(),
        }
    }
}
