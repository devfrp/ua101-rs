//! `ua101`: command-line front end of the Rust UA-101 driver.

use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use ua101_usb::{
    StreamConfig, Ua101, list_devices,
    protocol::{
        descriptors::StreamFormat,
        monitor::MonitorTarget,
        pcm::{S24_MAX, f32_to_s24, s16_to_s24},
    },
};

#[derive(Parser)]
#[command(
    name = "ua101",
    version,
    about = "Rust user-space driver for the Roland/EDIROL UA-101"
)]
struct Cli {
    /// Pick the N-th connected UA-101 (0-based).
    #[arg(long, global = true, default_value_t = 0)]
    device: usize,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List connected UA-101 units.
    List,
    /// Show descriptors and stream formats.
    Info {
        /// Also dump the raw configuration descriptor.
        #[arg(long)]
        raw: bool,
    },
    /// Switch direct (hardware) input monitoring. Experimental: the request
    /// encoding is inferred from Roland's ASIO driver.
    Monitor {
        state: OnOff,
        /// Input number (1-based). All inputs when omitted.
        #[arg(long)]
        input: Option<u16>,
    },
    /// Print incoming MIDI messages.
    MidiIn {
        /// Stop after this many seconds (runs until Ctrl-C when omitted).
        #[arg(long)]
        seconds: Option<f64>,
    },
    /// Send MIDI bytes given in hex, e.g. `90 3c 64`.
    MidiOut {
        #[arg(required = true)]
        bytes: Vec<String>,
    },
    /// Record every input to a 24-bit WAV file (Linux).
    Record {
        output: PathBuf,
        #[arg(long, default_value_t = 10.0)]
        seconds: f64,
    },
    /// Play a WAV file (16/24/32-bit integer or 32-bit float) (Linux).
    Play {
        input: PathBuf,
        /// First output the file's channel 1 goes to (1-based).
        #[arg(long, default_value_t = 1)]
        output: u8,
    },
    /// Play a sine test tone (Linux).
    Tone {
        #[arg(long, default_value_t = 440.0)]
        freq: f64,
        #[arg(long, default_value_t = 3.0)]
        seconds: f64,
        /// Level in dBFS.
        #[arg(long, default_value_t = -18.0, allow_negative_numbers = true)]
        level: f64,
        /// Output channel (1-based). All outputs when omitted.
        #[arg(long)]
        output: Option<u8>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    match cli.command {
        Command::List => list(),
        Command::Info { raw } => info(&open(cli.device)?, raw),
        Command::Monitor { state, input } => monitor(&open(cli.device)?, state, input),
        Command::MidiIn { seconds } => midi_in(&open(cli.device)?, seconds),
        Command::MidiOut { bytes } => midi_out(&open(cli.device)?, &bytes),
        Command::Record { output, seconds } => record(&open(cli.device)?, &output, seconds),
        Command::Play { input, output } => play(&open(cli.device)?, &input, output),
        Command::Tone {
            freq,
            seconds,
            level,
            output,
        } => tone(&open(cli.device)?, freq, seconds, level, output),
    }
}

fn open(index: usize) -> Result<Ua101> {
    let devices = list_devices()?;
    let Some(info) = devices.into_iter().nth(index) else {
        bail!("no UA-101 found (USB 0582:007d). Is it connected and powered?");
    };
    Ua101::open(info).context(
        "cannot open the UA-101. On Linux, install the udev rule (see packaging/linux/60-ua101.rules)",
    )
}

fn list() -> Result<()> {
    let devices = list_devices()?;
    if devices.is_empty() {
        println!("no UA-101 connected");
    }
    for (i, d) in devices.iter().enumerate() {
        println!(
            "[{i}] {} {} serial {} speed {:?}",
            d.manufacturer_string().unwrap_or("Roland"),
            d.product_string().unwrap_or("UA-101"),
            d.serial_number().unwrap_or("?"),
            d.speed()
        );
    }
    Ok(())
}

fn print_format(name: &str, f: &Result<StreamFormat, impl std::fmt::Display>) {
    match f {
        Ok(f) => println!(
            "{name:9} interface {} alt {}: {} ch, {}-bit in {} bytes, {} Hz, endpoint 0x{:02x} \
             (max {} B/packet = {} frames, bInterval {})",
            f.interface,
            f.alt_setting,
            f.channels,
            f.bit_resolution,
            f.subframe_size,
            f.sample_rate,
            f.endpoint.address,
            f.endpoint.bytes_per_interval(),
            f.max_frames_per_packet(),
            f.endpoint.interval
        ),
        Err(e) => println!("{name:9} unavailable: {e}"),
    }
}

fn info(dev: &Ua101, raw: bool) -> Result<()> {
    let i = dev.info();
    println!(
        "{} {} (serial {}), bcdDevice {:04x}, speed {:?}",
        i.manufacturer_string().unwrap_or("Roland"),
        i.product_string().unwrap_or("UA-101"),
        i.serial_number().unwrap_or("?"),
        i.device_version(),
        dev.speed()
    );
    println!(
        "sample rate: {} Hz (set with the switch on the unit)",
        dev.sample_rate()
    );
    print_format("playback", &dev.playback_format());
    print_format("capture", &dev.capture_format());
    if raw {
        for (n, chunk) in dev.config_descriptor().chunks(16).enumerate() {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            println!("{:04x}: {}", n * 16, hex.join(" "));
        }
    }
    Ok(())
}

fn monitor(dev: &Ua101, state: OnOff, input: Option<u16>) -> Result<()> {
    let target = match input {
        None => MonitorTarget::All,
        Some(0) => bail!("inputs are numbered from 1"),
        Some(n) => MonitorTarget::Input(n - 1),
    };
    let on = matches!(state, OnOff::On);
    dev.set_direct_monitor(target, on)?;
    println!(
        "direct monitor {target:?} -> {}",
        if on { "on" } else { "off" }
    );
    Ok(())
}

fn midi_in(dev: &Ua101, seconds: Option<f64>) -> Result<()> {
    let mut port = dev.open_midi()?;
    let deadline = seconds.map(|s| Instant::now() + Duration::from_secs_f64(s));
    println!("listening for MIDI (Ctrl-C to quit)...");
    while deadline.is_none_or(|d| Instant::now() < d) {
        for p in port.receive_packets(Duration::from_millis(200))? {
            let (cable, data) = ua101_usb::protocol::midi::decode_packet(&p).unwrap_or((0, &[]));
            let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
            println!("cable {cable}: {}", hex.join(" "));
        }
    }
    Ok(())
}

fn midi_out(dev: &Ua101, bytes: &[String]) -> Result<()> {
    let data = bytes
        .iter()
        .flat_map(|s| s.split_whitespace())
        .map(|b| u8::from_str_radix(b.trim_start_matches("0x"), 16))
        .collect::<Result<Vec<u8>, _>>()
        .context("bytes must be hexadecimal, e.g. `90 3c 64`")?;
    let mut port = dev.open_midi()?;
    port.send(&data, Duration::from_secs(1))?;
    println!("sent {} byte(s)", data.len());
    Ok(())
}

fn report(stats: ua101_usb::StreamStats) {
    println!(
        "captured {} frames, played {} frames, {} overrun, {} underrun, {} packet errors",
        stats.capture_frames,
        stats.playback_frames,
        stats.capture_overruns,
        stats.playback_underruns,
        stats.packet_errors
    );
}

fn record(dev: &Ua101, path: &PathBuf, seconds: f64) -> Result<()> {
    let mut stream = dev.start_stream(StreamConfig::default())?;
    let channels = stream.capture_format().channels as u16;
    let rate = stream.sample_rate();
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    let wanted = (seconds * rate as f64) as u64 * channels as u64;
    let mut written = 0u64;
    println!(
        "recording {channels} channels at {rate} Hz to {}",
        path.display()
    );
    while written < wanted {
        if !stream.is_running() {
            break;
        }
        let rx = stream.capture();
        let n = rx.slots();
        if n == 0 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let chunk = rx.read_chunk(n.min((wanted - written) as usize))?;
        let (a, b) = chunk.as_slices();
        for &s in a.iter().chain(b) {
            writer.write_sample(s)?;
        }
        written += (a.len() + b.len()) as u64;
        chunk.commit_all();
    }
    writer.finalize()?;
    report(stream.stop()?);
    Ok(())
}

/// Pushes interleaved device frames produced by `fill` until it returns
/// `false`, then waits for the ring to drain.
fn play_frames(dev: &Ua101, mut fill: impl FnMut(&mut [i32]) -> bool) -> Result<()> {
    let mut stream = dev.start_stream(StreamConfig::default())?;
    let channels = stream.playback_format().channels as usize;
    let mut frame = vec![0i32; channels];
    let mut more = true;
    while more {
        if !stream.is_running() {
            break;
        }
        let tx = stream.playback();
        let room = tx.slots() / channels;
        if room == 0 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        for _ in 0..room {
            frame.fill(0);
            if !fill(&mut frame) {
                more = false;
                break;
            }
            for &s in &frame {
                let _ = tx.push(s);
            }
        }
    }
    // Wait for the ring to drain, then for the in-flight URBs (~8 ms).
    let capacity = stream.playback().buffer().capacity();
    while stream.is_running() && stream.playback().slots() < capacity {
        thread::sleep(Duration::from_millis(10));
    }
    thread::sleep(Duration::from_millis(50));
    report(stream.stop()?);
    Ok(())
}

fn play(dev: &Ua101, path: &PathBuf, first_output: u8) -> Result<()> {
    let format = dev.playback_format()?;
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != format.sample_rate {
        bail!(
            "{} is {} Hz but the UA-101 runs at {} Hz (set the rate switch on the unit)",
            path.display(),
            spec.sample_rate,
            format.sample_rate
        );
    }
    let offset = first_output.max(1) as usize - 1;
    let file_ch = spec.channels as usize;
    if offset + file_ch > format.channels as usize {
        bail!(
            "{file_ch} channel(s) starting at output {} exceed the {} outputs",
            first_output,
            format.channels
        );
    }
    let samples: Box<dyn Iterator<Item = i32>> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, _) => Box::new(
            reader
                .samples::<f32>()
                .map(|s| f32_to_s24(s.unwrap_or(0.0))),
        ),
        (hound::SampleFormat::Int, 16) => {
            Box::new(reader.samples::<i16>().map(|s| s16_to_s24(s.unwrap_or(0))))
        }
        (hound::SampleFormat::Int, 24) => Box::new(reader.samples::<i32>().map(|s| s.unwrap_or(0))),
        (hound::SampleFormat::Int, 32) => {
            Box::new(reader.samples::<i32>().map(|s| s.unwrap_or(0) >> 8))
        }
        (_, bits) => bail!("unsupported WAV sample size: {bits} bits"),
    };
    let mut samples = samples.peekable();
    println!(
        "playing {} ({file_ch} ch) on outputs {}-{}",
        path.display(),
        offset + 1,
        offset + file_ch
    );
    play_frames(dev, |frame| {
        if samples.peek().is_none() {
            return false;
        }
        for c in 0..file_ch {
            frame[offset + c] = samples.next().unwrap_or(0);
        }
        true
    })
}

fn tone(dev: &Ua101, freq: f64, seconds: f64, level_db: f64, output: Option<u8>) -> Result<()> {
    let format = dev.playback_format()?;
    let rate = format.sample_rate as f64;
    let total = (seconds * rate) as u64;
    let amp = 10f64.powf(level_db / 20.0).min(1.0) * S24_MAX as f64;
    let target = match output {
        Some(0) => bail!("outputs are numbered from 1"),
        Some(n) if n > format.channels => bail!("the UA-101 has {} outputs", format.channels),
        Some(n) => Some(n as usize - 1),
        None => None,
    };
    println!("{freq} Hz at {level_db} dBFS for {seconds} s");
    let mut n = 0u64;
    play_frames(dev, |frame| {
        if n >= total {
            return false;
        }
        let s = (amp * (std::f64::consts::TAU * freq * n as f64 / rate).sin()) as i32;
        match target {
            Some(c) => frame[c] = s,
            None => frame.fill(s),
        }
        n += 1;
        true
    })
}
