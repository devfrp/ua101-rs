# ua101-rs

A **cross-platform Rust driver** for the **Roland / EDIROL UA-101** USB
audio interface. Roland no longer ships up-to-date drivers for it.

It runs entirely in user space and contains no C code. The protocol was
reconstructed by static analysis of Roland's original Windows driver, then
checked against the descriptors of a real unit. See [`docs/protocol.md`](docs/protocol.md).

## Status

| Feature                          | Linux | macOS | Windows |
|----------------------------------|:-----:|:-----:|:-------:|
| Detection, descriptors           | ✅    | ✅    | ✅ (WinUSB) |
| Direct monitoring (experimental) | ✅    | ✅    | ✅      |
| MIDI in/out                      | ✅    | ✅    | ✅      |
| Audio, 10 outputs / 12 inputs    | ✅ (usbfs) | 🚧 IOKit isoch | 🚧 WinUSB isoch |
| System audio device (`ua101d` driver daemon) | 🧪 written, hardware test pending | 🚧 | 🚧 |
| Mixer / device panel (Roland SysEx)           | ✅ (addresses partly inferred) | ✅ | ✅ |

✅ implemented · 🧪 implemented, not yet verified on hardware · 🚧 to do. On Linux, 12-channel recording has been verified
on a real unit at 44.1 kHz. Audible playback is still to be verified (see
*Open questions* in `docs/protocol.md`).

## Installation

### Linux (any distribution)

```sh
curl -fsSL https://raw.githubusercontent.com/devfrp/ua101-rs/main/packaging/linux/install.sh | sh
```

The script:

- detects the architecture (x86_64, aarch64) and the libc (glibc or musl);
- installs the panel's runtime libraries with the distribution's package
  manager (apt, dnf, pacman, zypper, xbps);
- checks the archive's SHA-256;
- installs `ua101`, `ua101-panel` and a udev rule that gives access to the
  card without root.

Options: `sh -s -- --cli-only`, `--no-deps`, `--version v0.1.0`,
`--prefix ~/.local`, `--blacklist-kernel` (stops the in-kernel `snd-ua101`
driver from claiming the card), `--uninstall`.

> Linux already includes a kernel driver (`snd-ua101`). `ua101` detaches it
> from the interfaces it uses and reattaches it on exit.

### Windows

Download `ua101-panel-windows-x86_64.exe` (panel) or
`ua101-windows-x86_64.exe` (command line) from the
[releases](https://github.com/devfrp/ua101-rs/releases). You must first bind
the card to the generic WinUSB driver with Zadig; see
[`packaging/windows/README.md`](packaging/windows/README.md).

### macOS

Download `ua101-macos-universal.dmg` (Intel + Apple Silicon) and drag
**UA-101 Panel** into *Applications*. The app is not notarized, so the first
time, right-click it and choose *Open*, or run
`xattr -dr com.apple.quarantine "/Applications/UA-101 Panel.app"`.

## The driver daemon (Linux)

`ua101d` plays the role of Roland's Windows driver. It owns the unit, runs
the audio streams, keeps the mixer state (and restores it when the unit is
plugged in), and gives the panel a control channel. Applications reach the
card through the `snd-aloop` loopback card `UA101RS`:

- **device 0** of `UA101RS` is the UA-101 for applications and
  PipeWire/JACK: 10 outputs, 12 inputs;
- device 1 is reserved for the daemon.

The installer sets everything up. By hand:

```sh
sudo modprobe snd-aloop id=UA101RS pcm_substreams=1
ua101d                        # or: systemctl --user enable --now ua101d
```

While `ua101d` runs, `ua101-panel` connects to it automatically (status
`UA101D` in the header) and the card stays available to the system. The
control socket is `$XDG_RUNTIME_DIR/ua101d.sock`; its line protocol is
documented in `crates/ua101d/src/control.rs`.

## Usage

```sh
ua101 list                          # connected units
ua101 info --raw                    # formats, endpoints, raw descriptor
ua101 monitor on --input 1          # direct monitoring of input 1
ua101 midi-in --seconds 30          # print incoming MIDI
ua101 midi-out 90 3c 64             # send a Note On
ua101 tone --output 1 --freq 1000   # test tone (Linux)
ua101 play file.wav --output 3      # play on outputs 3 and up (Linux)
ua101 record take.wav --seconds 60  # record all 12 inputs (Linux)
```

The sample rate is set with the switch on the unit. The driver reads it
from the descriptors.

## Building from source

```sh
cargo build --release              # whole workspace
cargo test --workspace             # tests (no hardware needed)
cargo run -p ua101-cli -- info     # CLI
cargo run -p ua101-panel           # graphical panel
```

Requires Rust 1.85 or later (1.95 for the panel). On Linux, building the
panel needs the `libxkbcommon`, `wayland`, `libGL` and `libX11` development
headers.

## Architecture

```
crates/
  ua101-protocol   no_std, no dependencies: descriptors, sample format,
                   packet sizing, vendor request, USB-MIDI
  ua101-usb        driver: nusb (control, bulk) + one isochronous backend per OS
                   (Linux: usbfs), full-duplex engine with ring buffers
  ua101-cli        `ua101` binary
  ua101-panel      `ua101-panel` binary (egui)
  ua101d           driver daemon: audio bridge to snd-aloop (pure-Rust ALSA
                   ioctls), MIDI/mixer control, state mirror, control socket
packaging/         Linux installer, udev rule, macOS bundle, Windows notes
docs/protocol.md   USB protocol and evidence
```

## Releasing

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The `release.yml` workflow builds Linux (x86_64, aarch64, musl), Windows
(`.exe`) and macOS (universal `.app` in a `.dmg`), then publishes the
files and `SHA256SUMS` in the GitHub release.

## Legal

This project is an independent reimplementation made for interoperability.
It neither contains nor redistributes any Roland files: the original driver
packages are excluded from the repository. "Roland", "EDIROL" and "UA-101"
are trademarks of Roland Corporation, which is not affiliated with this
project.

## License

Your choice of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
© 2026 devfrp.
