//! Control channel: the daemon's equivalent of the Windows driver's IOCTL
//! interface. Clients (the panel, scripts) connect to a Unix socket and
//! exchange one text line per message:
//!
//! ```text
//! client → daemon   dt1 AA AA AA AA DD [DD…]   write device parameters
//!                   rq1 AA AA AA AA SIZE        read device memory (SIZE in
//!                                               decimal); replies arrive as dt1
//!                   hello                       request info + full state
//!                   patch on|off|clear          software patch bay
//!                   route OUT IN                cable INPUT IN to OUTPUT OUT
//!                                               (1-based; IN 0 removes it)
//! daemon → client   info rate=R inputs=I outputs=O connected=0|1
//!                   patch on|off R,R,…          patch bay state: per output,
//!                                               the cabled input (0 = none)
//!                   dt1 AA AA AA AA DD [DD…]   parameter value (state dump,
//!                                               echo of writes, or a change
//!                                               reported by the unit)
//!                   levels in v,v,… out v,v,…   peak levels, ~20 per second
//!                   err MESSAGE
//! ```
//!
//! Like the Windows driver, the daemon keeps a mirror of every parameter
//! (the unit cannot be read back) and persists the mixer part so it is
//! restored when the unit is reconnected.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
    },
    thread,
};

/// A request from a client.
pub enum Request {
    Write([u8; 4], Vec<u8>),
    /// Roland RQ1 data request; the unit answers with DT1 messages, which
    /// are broadcast like any other parameter.
    Read([u8; 4], u32),
    Hello(u64),
    Patch(PatchCommand),
}

/// Software patch bay commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchCommand {
    Enable(bool),
    Clear,
    /// 0-based output, 0-based input (`None` removes the cable).
    Route(usize, Option<usize>),
}

pub fn parse_patch(line: &str) -> Option<PatchCommand> {
    let mut it = line.split_whitespace();
    let cmd = match (it.next()?, it.next()?) {
        ("patch", "on") => PatchCommand::Enable(true),
        ("patch", "off") => PatchCommand::Enable(false),
        ("patch", "clear") => PatchCommand::Clear,
        ("route", out) => {
            let out: usize = out.parse().ok()?;
            let input: usize = it.next()?.parse().ok()?;
            PatchCommand::Route(out.checked_sub(1)?, input.checked_sub(1))
        }
        _ => return None,
    };
    it.next().is_none().then_some(cmd)
}

/// `patch on|off R,R,…` for `outputs` outputs.
pub fn patch_line(p: &ua101_usb::Patchbay, outputs: usize) -> String {
    let routes: Vec<String> = (0..outputs)
        .map(|o| p.route(o).map_or(0, |i| i + 1).to_string())
        .collect();
    format!(
        "patch {} {}",
        if p.is_enabled() { "on" } else { "off" },
        routes.join(",")
    )
}

/// Applies a `patch on|off R,R,…` line (as saved by [`save_patch`]).
pub fn apply_patch_line(p: &ua101_usb::Patchbay, line: &str) -> bool {
    let mut it = line.split_whitespace();
    let (Some("patch"), Some(state), Some(routes)) = (it.next(), it.next(), it.next()) else {
        return false;
    };
    let on = match state {
        "on" => true,
        "off" => false,
        _ => return false,
    };
    let parsed: Option<Vec<usize>> = routes.split(',').map(|r| r.parse().ok()).collect();
    let Some(parsed) = parsed else { return false };
    p.clear();
    for (o, r) in parsed.into_iter().enumerate() {
        p.set_route(o, r.checked_sub(1));
    }
    p.set_enabled(on);
    true
}

/// Parameter mirror keyed by address.
pub type State = BTreeMap<[u8; 4], Vec<u8>>;

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `AA AA AA AA DD…` (7-bit hex bytes).
pub fn parse_dt1_args(s: &str) -> Option<([u8; 4], Vec<u8>)> {
    let bytes: Option<Vec<u8>> = s
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).ok().filter(|v| *v < 0x80))
        .collect();
    let bytes = bytes?;
    if bytes.len() < 5 {
        return None;
    }
    Some((
        [bytes[0], bytes[1], bytes[2], bytes[3]],
        bytes[4..].to_vec(),
    ))
}

/// Connected clients.
#[derive(Default, Clone)]
pub struct Clients {
    inner: Arc<Mutex<Vec<(u64, UnixStream)>>>,
}

impl Clients {
    pub fn broadcast(&self, line: &str) {
        let Ok(mut v) = self.inner.lock() else { return };
        v.retain_mut(|(_, s)| writeln!(s, "{line}").is_ok());
    }

    pub fn send_to(&self, id: u64, line: &str) {
        let Ok(mut v) = self.inner.lock() else { return };
        v.retain_mut(|(cid, s)| *cid != id || writeln!(s, "{line}").is_ok());
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().map(|v| v.is_empty()).unwrap_or(true)
    }
}

/// Starts accepting clients on `path`. Each line a client sends becomes a
/// [`Request`].
pub fn listen(path: &Path, clients: Clients, requests: Sender<Request>) -> std::io::Result<()> {
    if path.exists() {
        // A stale socket from a previous run; refuse if a daemon answers.
        if UnixStream::connect(path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("another ua101d is already listening on {}", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    thread::Builder::new()
        .name("ua101d-accept".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let Ok(reader) = stream.try_clone() else {
                    continue;
                };
                if let Ok(mut v) = clients.inner.lock() {
                    v.push((id, stream));
                }
                let requests = requests.clone();
                let clients = clients.clone();
                let _ = thread::Builder::new()
                    .name(format!("ua101d-client-{id}"))
                    .spawn(move || {
                        for line in BufReader::new(reader).lines() {
                            let Ok(line) = line else { break };
                            let line = line.trim();
                            if line == "hello" {
                                let _ = requests.send(Request::Hello(id));
                            } else if line.starts_with("patch") || line.starts_with("route") {
                                match parse_patch(line) {
                                    Some(c) => {
                                        let _ = requests.send(Request::Patch(c));
                                    }
                                    None => clients
                                        .send_to(id, "err usage: patch on|off|clear, route OUT IN"),
                                }
                            } else if let Some(rest) = line.strip_prefix("rq1 ") {
                                let mut it = rest.split_whitespace();
                                let addr: Option<Vec<u8>> = it
                                    .by_ref()
                                    .take(4)
                                    .map(|b| u8::from_str_radix(b, 16).ok().filter(|v| *v < 0x80))
                                    .collect();
                                match (addr, it.next().and_then(|s| s.parse::<u32>().ok())) {
                                    (Some(a), Some(size)) if a.len() == 4 => {
                                        let _ = requests
                                            .send(Request::Read([a[0], a[1], a[2], a[3]], size));
                                    }
                                    _ => clients.send_to(id, "err usage: rq1 AA AA AA AA SIZE"),
                                }
                            } else if let Some(rest) = line.strip_prefix("dt1 ") {
                                match parse_dt1_args(rest) {
                                    Some((a, d)) => {
                                        let _ = requests.send(Request::Write(a, d));
                                    }
                                    None => clients.send_to(id, "err bad dt1 arguments"),
                                }
                            } else if !line.is_empty() {
                                clients.send_to(id, "err unknown command");
                            }
                        }
                    });
            }
        })?;
    Ok(())
}

/// Where the mixer mirror is saved.
pub fn state_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("ua101d").join("mixer.txt")
}

/// Where the software patch bay is saved. It only routes audio inside the
/// daemon, so it is always restored.
pub fn patch_path() -> PathBuf {
    state_path().with_file_name("patchbay.txt")
}

pub fn save_patch(path: &Path, line: &str) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, format!("{line}\n"));
}

/// Loads saved parameters (`AA AA AA AA = DD …` per line).
pub fn load_state(path: &Path) -> State {
    let mut state = State::new();
    if let Ok(text) = fs::read_to_string(path) {
        for line in text.lines() {
            if let Some((a, d)) = line.split_once('=') {
                if let Some((addr, data)) = parse_dt1_args(&format!("{a} {d}")) {
                    state.insert(addr, data);
                }
            }
        }
    }
    state
}

/// Saves the mixer part of the mirror (everything outside the unit's
/// front-panel status block `00 01 xx xx`).
pub fn save_state(path: &Path, state: &State) {
    let mut out = String::new();
    for (a, d) in state {
        if is_persistent(a) {
            out.push_str(&format!("{} = {}\n", hex(a), hex(d)));
        }
    }
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, out);
}

/// Mixer blocks (`00 03`…`00 05`) are restored at startup. Device memory
/// operations, the patch bay and the front-panel status are not.
pub fn is_persistent(address: &[u8; 4]) -> bool {
    address[0] == 0x00 && (0x03..=0x05).contains(&address[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dt1_arguments() {
        assert_eq!(
            parse_dt1_args("00 04 00 03 04 00 00 00"),
            Some(([0, 4, 0, 3], vec![4, 0, 0, 0]))
        );
        assert_eq!(parse_dt1_args("00 04 00"), None);
        assert_eq!(parse_dt1_args("00 04 00 03 80"), None);
    }

    #[test]
    fn parses_patch_commands() {
        assert_eq!(parse_patch("patch on"), Some(PatchCommand::Enable(true)));
        assert_eq!(parse_patch("patch clear"), Some(PatchCommand::Clear));
        assert_eq!(
            parse_patch("route 3 1"),
            Some(PatchCommand::Route(2, Some(0)))
        );
        assert_eq!(parse_patch("route 3 0"), Some(PatchCommand::Route(2, None)));
        assert_eq!(parse_patch("route 0 1"), None);
        assert_eq!(parse_patch("route 3"), None);
        assert_eq!(parse_patch("patch maybe"), None);
    }

    #[test]
    fn patch_line_roundtrip() {
        let p = ua101_usb::Patchbay::default();
        p.set_route(1, Some(4));
        p.set_enabled(true);
        let line = patch_line(&p, 4);
        assert_eq!(line, "patch on 0,5,0,0");
        let q = ua101_usb::Patchbay::default();
        assert!(apply_patch_line(&q, &line));
        assert!(q.is_enabled());
        assert_eq!(q.route(1), Some(4));
        assert_eq!(q.route(0), None);
    }

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ua101d-test-{}", std::process::id()));
        let path = dir.join("mixer.txt");
        let mut s = State::new();
        s.insert([0, 4, 1, 2], vec![1]);
        s.insert([0, 1, 0, 4], vec![1]); // front panel: not persisted
        save_state(&path, &s);
        let back = load_state(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back.get(&[0, 4, 1, 2]), Some(&vec![1]));
        let _ = fs::remove_dir_all(dir);
    }
}
