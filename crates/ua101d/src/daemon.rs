//! The daemon: owns the UA-101 like Roland's kernel driver does, exposes its
//! audio through the `snd-aloop` loopback card, and serves the control
//! channel.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use log::{error, info, warn};
use ua101_usb::{StreamConfig, Ua101, list_devices};

use crate::{
    alsa::{self, Config, Direction, Pcm},
    bridge::{self, Levels, PumpConfig, SharedLevels},
    control::{self, Clients, Request, State},
};

#[derive(Parser, Debug)]
#[command(
    name = "ua101d",
    version,
    about = "User-space driver daemon for the Roland/EDIROL UA-101"
)]
pub struct Args {
    /// ALSA id of the snd-aloop card that carries the audio
    /// (`modprobe snd-aloop id=UA101RS pcm_substreams=1`).
    #[arg(long, default_value = "UA101RS")]
    card_id: String,
    /// Frames per ALSA period.
    #[arg(long, default_value_t = 256)]
    period: u32,
    /// Periods per ALSA buffer.
    #[arg(long, default_value_t = 4)]
    periods: u32,
    /// Control socket (default: $XDG_RUNTIME_DIR/ua101d.sock).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Do not re-send the saved mixer settings when the unit connects.
    #[arg(long)]
    no_restore: bool,
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[allow(unsafe_code)]
fn install_signal_handlers() {
    let handler = on_signal as extern "C" fn(libc::c_int);
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
}

struct Shared {
    clients: Clients,
    requests: Receiver<Request>,
    state: State,
    state_path: PathBuf,
    levels: SharedLevels,
    info: String,
}

impl Shared {
    fn hello(&self, id: u64) {
        self.clients.send_to(id, &self.info);
        for (a, d) in &self.state {
            self.clients
                .send_to(id, &format!("dt1 {} {}", control::hex(a), control::hex(d)));
        }
    }

    fn set_info(&mut self, info: String) {
        self.info = info;
        self.clients.broadcast(&self.info);
    }
}

pub fn run() -> Result<()> {
    let args = Args::parse();
    install_signal_handlers();

    let socket = args
        .socket
        .clone()
        .unwrap_or_else(ua101_usb::daemon_socket_path);
    let clients = Clients::default();
    let (tx, requests) = mpsc::channel();
    control::listen(&socket, clients.clone(), tx)
        .with_context(|| format!("cannot listen on {}", socket.display()))?;
    info!("control socket: {}", socket.display());

    let state_path = control::state_path();
    let state = if args.no_restore {
        State::new()
    } else {
        control::load_state(&state_path)
    };
    let mut shared = Shared {
        clients,
        requests,
        state,
        state_path,
        levels: Arc::new(Mutex::new(Levels::default())),
        info: "info connected=0".into(),
    };

    let mut waiting_logged = false;
    while !STOP.load(Ordering::SeqCst) {
        if list_devices().map(|d| d.is_empty()).unwrap_or(true) {
            if !waiting_logged {
                info!("waiting for a UA-101…");
                waiting_logged = true;
            }
            idle(&mut shared, Duration::from_millis(500));
            continue;
        }
        waiting_logged = false;
        match session(&args, &mut shared) {
            Ok(()) => info!("session ended"),
            Err(e) => {
                error!("{e:#}");
                shared
                    .clients
                    .broadcast(&format!("err {}", format!("{e:#}").replace('\n', " ")));
                idle(&mut shared, Duration::from_secs(2));
            }
        }
        shared.set_info("info connected=0".into());
    }
    let _ = std::fs::remove_file(&socket);
    info!("stopped");
    Ok(())
}

/// Serves clients while no unit is connected.
fn idle(shared: &mut Shared, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end && !STOP.load(Ordering::SeqCst) {
        while let Ok(req) = shared.requests.try_recv() {
            match req {
                Request::Hello(id) => shared.hello(id),
                Request::Write(..) => shared.clients.broadcast("err no UA-101 connected"),
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_pump<F>(name: &str, f: F) -> Result<JoinHandle<std::io::Result<()>>>
where
    F: FnOnce() -> std::io::Result<()> + Send + 'static,
{
    Ok(thread::Builder::new().name(name.into()).spawn(f)?)
}

fn session(args: &Args, shared: &mut Shared) -> Result<()> {
    let card = alsa::find_card(&args.card_id).ok_or_else(|| {
        anyhow!(
            "no ALSA loopback card with id {:?}; load it with \
             `sudo modprobe snd-aloop id={} pcm_substreams=1`",
            args.card_id,
            args.card_id
        )
    })?;

    let dev = Ua101::open_first().context("cannot open the UA-101")?;
    let stream = dev
        .start_stream(StreamConfig {
            ring_frames: 16_384,
            ..StreamConfig::default()
        })
        .context("cannot start USB streaming")?;
    let (ctl, cap_rx, play_tx) = stream.split();
    let play = *ctl.playback_format();
    let capt = *ctl.capture_format();
    let rate = capt.sample_rate;
    let mut midi = dev.open_midi().context("cannot open the control port")?;

    let pcm_cfg = |channels: u8| Config {
        channels: channels as u32,
        rate,
        period: args.period,
        periods: args.periods,
    };
    // Device 1 of the loopback card is ours; applications use device 0.
    let to_pcm = Pcm::open(card, 1, Direction::Capture, pcm_cfg(play.channels))
        .context("cannot open the loopback capture side")?;
    let from_pcm = Pcm::open(card, 1, Direction::Playback, pcm_cfg(capt.channels))
        .context("cannot open the loopback playback side")?;
    let period = to_pcm.config().period as usize;

    if !shared.state.is_empty() {
        let restored = shared
            .state
            .iter()
            .filter(|(a, _)| control::is_persistent(a))
            .map(|(a, d)| midi.write_parameters(*a, d))
            .filter(|r| r.is_ok())
            .count();
        info!("restored {restored} mixer parameter(s)");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let to_thread = {
        let (stop, levels) = (stop.clone(), shared.levels.clone());
        spawn_pump("ua101d-to-device", move || {
            bridge::to_device(
                to_pcm,
                play_tx,
                stop,
                levels,
                PumpConfig {
                    period,
                    target_periods: 4,
                },
            )
        })?
    };
    let from_thread = {
        let (stop, levels) = (stop.clone(), shared.levels.clone());
        spawn_pump("ua101d-from-device", move || {
            bridge::from_device(
                from_pcm,
                cap_rx,
                stop,
                levels,
                PumpConfig {
                    period,
                    target_periods: 4,
                },
            )
        })?
    };

    info!(
        "running: {rate} Hz, {} outputs, {} inputs, loopback card {card} ({})",
        play.channels, capt.channels, args.card_id
    );
    shared.set_info(format!(
        "info rate={rate} inputs={} outputs={} connected=1",
        capt.channels, play.channels
    ));

    let mut last_levels = Instant::now();
    let mut result = Ok(());
    while !STOP.load(Ordering::SeqCst) {
        if !ctl.is_running() {
            result = Err(anyhow!("USB stream stopped (unit disconnected?)"));
            break;
        }
        if to_thread.is_finished() || from_thread.is_finished() {
            result = Err(anyhow!("loopback bridge stopped"));
            break;
        }
        match midi.receive_control(Duration::from_millis(20)) {
            Ok(msgs) => {
                for (a, d) in msgs {
                    shared.clients.broadcast(&format!(
                        "dt1 {} {}",
                        control::hex(&a),
                        control::hex(&d)
                    ));
                    shared.state.insert(a, d);
                }
            }
            Err(e) => {
                result = Err(anyhow!("control port: {e}"));
                break;
            }
        }
        while let Ok(req) = shared.requests.try_recv() {
            match req {
                Request::Hello(id) => shared.hello(id),
                Request::Write(a, d) => match midi.write_parameters(a, &d) {
                    Ok(()) => {
                        shared.clients.broadcast(&format!(
                            "dt1 {} {}",
                            control::hex(&a),
                            control::hex(&d)
                        ));
                        let persist = control::is_persistent(&a);
                        shared.state.insert(a, d);
                        if persist {
                            control::save_state(&shared.state_path, &shared.state);
                        }
                    }
                    Err(e) => shared.clients.broadcast(&format!("err write failed: {e}")),
                },
            }
        }
        if last_levels.elapsed() >= Duration::from_millis(50) {
            last_levels = Instant::now();
            if !shared.clients.is_empty() {
                if let Ok(mut l) = shared.levels.lock() {
                    let fmt = |v: &mut Vec<f32>| {
                        let s = v
                            .iter()
                            .map(|x| format!("{x:.4}"))
                            .collect::<Vec<_>>()
                            .join(",");
                        v.iter_mut().for_each(|x| *x = 0.0);
                        s
                    };
                    let line = format!(
                        "levels in {} out {}",
                        fmt(&mut l.inputs),
                        fmt(&mut l.outputs)
                    );
                    shared.clients.broadcast(&line);
                }
            }
        }
    }

    stop.store(true, Ordering::SeqCst);
    for (name, t) in [("to-device", to_thread), ("from-device", from_thread)] {
        match t.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("{name} pump: {e}"),
            Err(_) => warn!("{name} pump panicked"),
        }
    }
    drop(midi);
    match ctl.stop() {
        Ok(stats) => info!(
            "USB: captured {} frames, played {} frames, {} packet errors",
            stats.capture_frames, stats.playback_frames, stats.packet_errors
        ),
        Err(e) => warn!("USB stream: {e}"),
    }
    result
}
