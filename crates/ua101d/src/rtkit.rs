//! Real-time scheduling for the audio threads through RealtimeKit, the
//! D-Bus service PipeWire uses for its own data loop.
//!
//! A desktop user has no `RLIMIT_RTPRIO`, so `sched_setscheduler` fails
//! and the threads stay at normal priority. Under CPU load (and always on
//! a PREEMPT_RT kernel, where interrupt threads are real-time) they then
//! run late and in bursts: the USB ring fill leaves its band and the
//! bridge drops or repeats a frame every period, which is heard as
//! crackling. With SCHED_RR the same load causes no glitch (measured).

use zbus::blocking::{Connection, Proxy};

const DEST: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const IFACE: &str = "org.freedesktop.RealtimeKit1";
/// Priority asked for: PipeWire's own (rtkit's default maximum).
const PRIORITY: i32 = 20;

/// Kernel thread id of the calling thread.
pub fn current_tid() -> u64 {
    // SAFETY: gettid has no preconditions.
    unsafe { libc::gettid() as u64 }
}

/// Asks RealtimeKit to make thread `tid` of this process SCHED_RR.
/// Returns the priority obtained.
pub fn make_realtime(tid: u64) -> Result<u32, String> {
    let conn = Connection::system().map_err(|e| format!("system bus: {e}"))?;
    let proxy = Proxy::new(&conn, DEST, PATH, IFACE).map_err(|e| e.to_string())?;
    let max_priority: i32 = proxy
        .get_property("MaxRealtimePriority")
        .map_err(|e| format!("RealtimeKit unavailable: {e}"))?;
    let rttime_max: i64 = proxy
        .get_property("RTTimeUSecMax")
        .map_err(|e| format!("RealtimeKit unavailable: {e}"))?;
    // RealtimeKit only serves processes whose RLIMIT_RTTIME is at most its
    // maximum, so that a runaway real-time thread gets SIGXCPU.
    limit_rttime(rttime_max.max(1) as u64)?;
    let priority = PRIORITY.min(max_priority).max(1) as u32;
    proxy
        .call_method(
            "MakeThreadRealtimeWithPID",
            &(std::process::id() as u64, tid, priority),
        )
        .map_err(|e| e.to_string())?;
    Ok(priority)
}

/// Lowers RLIMIT_RTTIME to `usec` (never raises it).
fn limit_rttime(usec: u64) -> Result<(), String> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid pointer to an rlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &mut lim) } != 0 {
        return Err(format!("getrlimit: {}", std::io::Error::last_os_error()));
    }
    let max = lim.rlim_max.min(usec as libc::rlim_t);
    let cur = lim.rlim_cur.min(max);
    if (cur, max) == (lim.rlim_cur, lim.rlim_max) {
        return Ok(());
    }
    let new = libc::rlimit {
        rlim_cur: cur,
        rlim_max: max,
    };
    // SAFETY: valid pointer to an rlimit.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &new) } != 0 {
        return Err(format!("setrlimit: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// Makes `tid` real-time and logs the outcome under `name`.
pub fn promote(name: &str, tid: u64) {
    match make_realtime(tid) {
        Ok(p) => log::info!("{name}: real-time priority {p} (SCHED_RR)"),
        Err(e) => log::warn!(
            "{name}: no real-time priority ({e}); audio may crackle under load. \
             Is rtkit-daemon installed?"
        ),
    }
}
