//! `ua101d`: user-space driver daemon for the Roland/EDIROL UA-101.
//!
//! It plays the role of Roland's Windows kernel driver (RDWM1049.SYS):
//! it owns the unit, runs the isochronous audio streams, mirrors the
//! internal mixer state and offers a control channel to the panel. Audio
//! reaches applications through the `snd-aloop` loopback card. Linux only
//! for now.

#[cfg(target_os = "linux")]
mod alsa;
#[cfg(target_os = "linux")]
mod bridge;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod daemon;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    #[cfg(target_os = "linux")]
    {
        daemon::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("ua101d currently supports Linux only")
    }
}
