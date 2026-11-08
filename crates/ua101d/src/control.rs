//! Control channel: the daemon's equivalent of the Windows driver's IOCTL
//! interface. Clients (the panel, scripts) connect to a Unix socket and
//! exchange one text line per message:
//!
//! ```text
//! client → daemon   dt1 AA AA AA AA DD [DD…]   write device parameters
//!                   hello                       request info + full state
//! daemon → client   info rate=R inputs=I outputs=O connected=0|1
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
    Hello(u64),
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
