# UA-101 USB protocol

This document records what is known about how the Roland/EDIROL UA-101 talks
over USB, and where each fact comes from. The protocol crate
(`crates/ua101-protocol`) implements exactly what is written here.

Evidence levels:

- **observed (device)**: read from a real unit (descriptors via `lsusb -v` /
  sysfs).
- **observed (static)**: read from Roland's Windows driver code with
  a disassembler.
  These describe what the code does, not what was seen at run time.
- **inferred**: an interpretation of static code that has not been confirmed
  on hardware yet.

Analysed artifact: `RDWM1049.SYS` from the Windows 10 x64 package
(`driverpackage_8c29…cab`, INF `DriverVer=08/17/2015,1.00.00.0000`), SHA-256
`85897e4db82f1b582025698cf75bc36dda01d72a831403a233f1931015cdf229`, image base
`0x10000`. Addresses below are virtual addresses in that image. The ASIO DLL is
`RDAS1049.DLL` from the same package (image base `0x180000000`). None of these
binaries are included in this repository.

## Device identity (observed, device)

| Field          | Value                                  |
|----------------|----------------------------------------|
| VID:PID        | `0582:007d`                            |
| bcdUSB         | 2.00, runs at **high speed** (480 Mb/s) |
| Class          | `0xff` vendor specific (device and interfaces) |
| bcdDevice      | 1.05 on the analysed unit              |
| Configurations | 1 (`wTotalLength = 0x9b`)              |

## Interfaces (observed, device)

| Intf | Alt | Role     | Endpoint | Type | wMaxPacketSize | bInterval |
|------|-----|----------|----------|------|----------------|-----------|
| 0    | 0   | playback (idle)  | none | | | |
| 0    | 1   | playback | `0x02` OUT | isochronous, async | 280 | 1 |
| 1    | 0   | capture (idle)   | none | | | |
| 1    | 1   | capture  | `0x81` IN  | isochronous, async, *implicit feedback data* | 336 | 1 |
| 2    | 0   | MIDI     | `0x04` OUT bulk, `0x83` IN bulk | | 512 | |
| 2    | 1   | MIDI     | `0x04` OUT bulk, `0x83` IN **interrupt** | | 512 | 1 |

Each streaming alternate setting carries two USB Audio Class 1.0 style
class-specific descriptors (type `0x24`) between the interface and endpoint
descriptors:

```
07 24 01 <bTerminalLink> 00 01 00        AS_GENERAL
0b 24 02 01 <ch> 04 18 01 <tSamFreq x3>  FORMAT_TYPE I: ch channels, 4-byte subframe, 24 bits, 1 rate
```

At the 44.1 kHz switch position: playback has 10 channels, capture has
12 channels, `tSamFreq = 44 ac 00` (44 100 Hz). Packet capacity is 7 frames
per microframe in both directions (280 / 40 and 336 / 48).

## Enumeration (observed, static)

The driver submits URBs with `IOCTL_INTERNAL_USB_SUBMIT_URB` (`0x220003`,
helper at `0x2c420`). Its start-up sequence:

1. `GET_DESCRIPTOR(DEVICE)`: URB function `0x0b`, 18 bytes (`0x2b7e0`).
2. `GET_DESCRIPTOR(CONFIGURATION)`, first the header, then `wTotalLength`
   bytes (`0x2ba00`).
3. `SELECT_CONFIGURATION`: URB function `0x00` (`0x2bfa0`).
4. For interfaces 0 and 1, alternate setting 1: find the `FORMAT_TYPE`
   descriptor and the isochronous endpoint (`0x2d930`). It keeps
   `bFormatType`, `bNrChannels`, `bBitResolution`, `bSamFreqType`, the 24-bit
   `tSamFreq`, the endpoint direction and `wMaxPacketSize`.
5. The sample rate is read 24 bytes after the interface descriptor of
   interface 0 / alt 1 (9 + 7-byte `AS_GENERAL` + offset 8 of `FORMAT_TYPE`).
   **96 000 Hz is assumed** when no descriptor is available (`0x2b3e0`).
6. `SELECT_INTERFACE`: URB function `0x01` (`0x2c130`).

No vendor request is needed to start streaming. The sample rate is chosen
with the hardware switch: the device re-enumerates with different
descriptors.

Other URBs: `SYNC_RESET_PIPE_AND_CLEAR_STALL` `0x1e` (`0x364d0`),
`GET_CURRENT_FRAME_NUMBER` `0x07` (`0x3e600`).

## Audio streaming (observed, static, unless noted)

- Isochronous URBs: function `0x0a` (`0x378f0`), `TransferFlags`
  `USBD_START_ISO_TRANSFER_ASAP`.
- Capture (`0x3d330`): `TransferFlags = 5` (IN | ASAP); packet `i` starts at
  `i * max_packet_size`.
- Playback (`0x3e0f0`, `0x3e270`, `0x3e3c0`): for a transfer of `N` packets
  holding `F` frames, packet `i` ends at `floor(F * (i + 1) / N)` frames
  (`0x37980`). See `packets::distribute_frames`.
- Sample layout (`0x37fc0`, `0x37ed0`): the mixer sums client streams as
  24-bit integers, clips to `[-0x800000, 0x7fffff]`, and stores each sample
  as a little-endian 32-bit word `sample << 8`, interleaved by channel.
  That matches `bSubframeSize = 4`, `bBitResolution = 24`.
- Clock: the capture endpoint is declared *implicit feedback data*
  (observed, device). The device is the clock master, so playback packets
  must carry as many frames as the matching capture packets. The Rust
  engine does this, falling back to the nominal rate during warm-up. The
  exact pairing used by Roland's driver is **inferred**.
- At high speed with `bInterval = 1` there are 8000 packets per second
  (5 or 6 frames per packet at 44.1 kHz, 6 at 48 kHz, 11 or 12 at 88.2/96 kHz).

### Hardware check (observed, device, 2026-11-08)

At 44.1 kHz, with the Rust engine on Linux and the URB size the CLI uses
(8 URBs × 8 packets per direction), a 2 s capture returned 88 252 frames.
That matches the nominal rate, with 0 packet errors and 0 overruns. The
silent playback stream ran alongside it. Inputs 1–8 carry an analog noise
floor around −102 dBFS RMS with nothing connected, inputs 9–10 are digital
silence, and channels 11–12 sit near −134 dBFS. After the stream stopped,
the kernel driver re-attached and ALSA listed the card again.

Not yet verified on hardware: audible playback, the playback/capture
pairing and the 48/88.2/96 kHz positions.

## MIDI (observed, static)

- Bulk transfers: URB function `0x09` (`0x19520`), IN with
  `TransferFlags = 3` (IN | SHORT_TRANSFER_OK) and a length of
  `wMaxPacketSize` (`0x1d100`).
- OUT transfers are sized `events * 4` from a ring of 32-bit entries
  (`0x1d780`): standard **USB-MIDI 1.0 event packets** (cable number + CIN,
  then 3 data bytes).

## Mixer and device memory: Roland SysEx on cable 1 (observed, static)

Roland's control panel (`RDDP1049.EXE`) drives the mixer through driver
IOCTL `0x22225c`. Its 40-byte buffer holds a sub-command at offset 4 and
arguments at offsets 8, 12 and 16. The kernel dispatcher (`0x24ca0`, 28
cases, jump table at `0x25578`) turns each sub-command into a **Roland
exclusive message** sent on **USB-MIDI cable 1** of the bulk MIDI endpoint.
The send path is `0x1e290` with cable argument `1`.

```
DT1 (write): F0 41 10 00 69 12 a0 a1 a2 a3 <data> sum F7   (finaliser 0x209c0)
RQ1 (read):  F0 41 10 00 69 11 a0 a1 a2 a3 s0 s1 s2 s3 sum F7   (0x20d40)
sum = (128 - Σ(address + data) mod 128) mod 128
```

Addresses written by each sub-command (`n` and `m` are IOCTL arguments, `v`
is the value):

| Sub-cmd | Address                     | Data   | Handler   | Notes |
|---------|-----------------------------|--------|-----------|-------|
| 4       | RQ1 `00 02 n 00`, size 4    |        | `0x237f0` | reads one channel block |
| 9       | `00 00 00 00`               | 1 byte | `0x23bf0` | |
| 10      | `00 00 00 01`               | 1 byte | `0x23cc0` | |
| 11      | `00 00 00 02`               | 1 byte | `0x23d90` | |
| 12      | `00 00 n 03`                | 1 byte | `0x23e40` | |
| 14–19   | `00 01 00 00` … `00 01 00 05` | 1 byte | `0x24030`… | global parameters |
| 20      | `00 02 n m`                 | 1 byte | `0x24510` | per-channel parameter; driver keeps 5 per channel |
| 21      | `00 (n+2) 00 00`            | 1 byte | `0x24660` | `n` is 1-based |
| 22      | `00 (n+3) (2m) 01`          | 1 byte | `0x247c0` | |
| 23      | `00 (n+3) m 02`             | 1 byte | `0x248e0` | |
| 24      | `00 (n+3) m 03`             | 4 bytes| `0x24a00` | 16-bit value split into nibbles (`0x20b80`) |
| 25      | `00 (n+3) m 07`             | 6 bytes| `0x24b50` | `0x20e70` |

Multi-byte values are split into 4-bit nibbles, most significant first
(`0x20b80`: 4 nibbles for a 16-bit volume; `0x20e70`: 6 nibbles).

Channel parameters, `00 (3+bus) ch slot` with bus 0 = MONITOR OUT,
1 = INPUT, 2 = OUTPUT (WAVE):

| Slot | Size            | Meaning | Evidence |
|------|-----------------|---------|----------|
| `01` | 1 byte (on `2·pair`) | stereo LINK of a channel pair | inferred |
| `02` | 1 byte          | **MUTE** (`01` muted, `00` audible); for OUTPUT strips, whether the output reaches MONITOR OUT | **observed**: `00` brought the sound back. The Windows 10 panel has MUTE buttons (`CBtnMute`, `IDM_MUTE_ON_ALL`) |
| `03` | 16-bit, 4 nibbles | **PAN**, `0x4000` centre | **observed**: sweeping it was heard as a balance change. Panel presets `IDM_PAN_L100/C0/R100` |
| `07` | 24-bit, 6 nibbles | **VOLUME**, linear gain | **observed**: `0x400000` on a channel and on MONITOR OUT saturates, `0x100000` on both gives a clean, normal level. The panel's "set all" helper (`0x140004530`) is called with `0x7fffff, 0x400000, 0x200000, …, 0x8000` (6 dB steps) |

**SOFT CTRL**: with it on, the unit takes the whole monitor mix from the
computer and starts silent. The host must send the full mixer state;
`ua101d` does so when a session starts and whenever the unit reports SOFT
CTRL on (`00 01 00 05 = 01`).

The driver keeps exactly these three values per channel (`0x22610`,
`0x22700`, `0x227d0`: offsets +0, +4, +8 of a 12-byte record) plus LINK
per pair, so SOLO is not a per-channel slot; sub-command 21
(`00 (n+2) 00 00`) is a candidate.

### Front-panel status (observed, device)

The unit sends a DT1 on cable 1 whenever a front-panel switch moves (it did
not answer RQ1 requests in our tests, so the state is unknown until a
switch moves):

| Address        | Switch                 | Values seen |
|----------------|------------------------|-------------|
| `00 01 00 02`  | CLOCK                  | `00`, `09`  |
| `00 01 00 04`  | DIRECT MONITOR MONO    | `00`, `01`  |
| `00 01 00 05`  | DIRECT MONITOR SOFT CTRL | `00`, `01` |

The Windows driver parses incoming DT1s by their first address byte
(`0x1c8c0`: `00`, `01` and `02` blocks).

The panel uses sub-commands 6 and 20–26 for its mixer controls. Which UI
control maps to which address, and the value ranges, are **not yet known**:
the plan is to read the device's memory with RQ1 and correlate.

### Mixer memory block `02 00 00 00` (observed, static + device)

The unit answers `RQ1 02 00 00 00`, size 256, with one DT1 holding the
whole mixer (**observed**). The Windows driver sends the same block to
restore the mixer (sub-commands 8 and 13; serialiser `0x228a0` with
`0x219f0`, `0x21e20`, `0x22090`, `0x22460`, `0x212a0`, `0x21320`,
`0x21210`/`0x218b0`/`0x21950`, `0x213a0`). Writing known values with
single DT1s and reading the block back gave exactly those values, which
confirms the layout below (offsets in the 256 data bytes):

| Offset | Content |
|--------|---------|
| `00`–`03` | 4 bytes (`0x219f0`); read back as `01 01 01 01` |
| `04`–`06` | bit fields (`0x21e20`): `04` bit 3 flag + bits 0–2 value, `05` bits 0–2 value, `06` bits 0/1/2 flags |
| `07`–`15` | 3 groups of 5 bytes, the sub-command 20 values (`00 02 a b`); read back as `02 03 04 05 06`, `07 08 09 0a 01`, `00 00 00 00 00` |
| `16`, `17` | INPUT / OUTPUT bus value (sub-command 21) |
| `18`, `19`–`1a`, `1b`–`1c` | LINK bits (bit `pair % 4` of byte `base + pair / 4`): MONITOR OUT, INPUT, OUTPUT |
| `1d`, `1e`–`20`, `21`–`23` | MUTE bits (bit `ch % 4` of byte `base + ch / 4`): MONITOR OUT, INPUT, OUTPUT |
| `24`–`4b` | INPUT pan, 4 nibbles × 10 |
| `4c`–`73` | OUTPUT pan, 4 nibbles × 10 (MONITOR OUT has no pan) |
| `74`–`7f` | MONITOR OUT volume, 6 nibbles × 2 |
| `80`–`bb` | INPUT volume, 6 nibbles × 10 |
| `bc`–`f7` | OUTPUT volume, 6 nibbles × 10 |
| `f8`–`ff` | unused (zero) |

`ua101d` reads this block when a session starts and adopts it; only a
silent unit (everything muted or at zero, as after power-on with SOFT CTRL)
gets the daemon's state.

- Panel calls (`RDDP1049.EXE`, IOCTL `0x22225c`, arguments at buffer
  offsets 8/12/16/20/24):
  - sub-command 6 (`00 00 n 08` + 4 bytes): six fixed senders
    (`0x1400055d0`…) with argument patterns `(0,0,3,0,0)`, `(0,0,4,0,0)`,
    `(0,0,0,3,0)`, `(0,0,0,5,0)`, `(0,0,0,4,0)`, `(0,0,0,0,3)`; another
    caller (`0x14000990a`) passes computed values. Patch bay (the panel
    draws virtual cables) is the leading candidate.
  - sub-command 7 (`00 00 n 04` + 4 bytes): `0x140009bf7`, computed values.
  - sub-command 9 (`00 00 00 00`): value 0 or 1 (`0x140003d4f`, `0x140003e0f`).
  - sub-command 11 (`00 00 00 02`): all-zero arguments.
  - sub-command 12 (`00 00 n 03`): only `n` set (`0x140002c90`).

## Direct monitoring (inferred)

The ASIO driver (`RDAS1049.DLL`, `0x1800014af` and `0x180001531`) calls
`DeviceIoControl(0x222144)` with a 28-byte buffer. The kernel driver's IOCTL
dispatcher (jump table at `0x40c32`, case at `0x410c4`) takes the 16-bit
word at offset 4 of that buffer and sends (`0x42220`, `0x3e560`, `0x377a0`):

```
URB_FUNCTION_VENDOR_DEVICE (0x17), host-to-device
bRequest = 0x02, wValue = <word>, wIndex = 0, wLength = 0
```

The ASIO DLL builds the word as `(state == 1 ? 0x8000 : 0) | ((channel + 1) & 0x7fff)`
for one channel, or `0x8000` / `0x0000` with no channel bits for all
channels at once. The registry setting `AsioMonitor` read by the kernel
driver suggests this is ASIO direct monitoring. The effect on the hardware
has not been verified.

## Driver IOCTLs (observed, static)

User-mode components talk to the kernel driver through these codes
(`FILE_DEVICE_UNKNOWN`, `METHOD_BUFFERED`). Only `0x222144` reaches the
device directly; the others manage driver-side buffers and state.

| IOCTL      | Used by       | Handler   |
|------------|---------------|-----------|
| `0x222050` | ASIO, `RDDP1049.EXE` | `0x40cc1` |
| `0x22211c` | ASIO          | `0x40e2c` |
| `0x222120` | ASIO          | `0x40e71` |
| `0x222124` | ASIO          | `0x40da8` |
| `0x222128` | ASIO          | `0x40eab` |
| `0x222130` | ASIO          | `0x40f5b` |
| `0x222138` | ASIO          | `0x40fe5` |
| `0x22213c` | ASIO          | `0x4106b` |
| `0x222144` | ASIO          | `0x410c4` → vendor request `0x02` |
| `0x222150`–`0x22215c` | ASIO | separate comparisons before the table |
| `0x222160` | `RDDP1049.EXE` | separate comparison |

## Open questions

- [ ] Confirm the direct-monitor request on hardware and find out whether
      monitoring has a level or pan component (the 28-byte IOCTL buffer
      holds more than the word that is sent).
- [ ] Find out how the control panel's patch bay and "Load/Store Memory of
      Device" reach the unit (no vendor request other than `0x02` was found
      in the kernel driver; SysEx over the MIDI port is a candidate).
- [ ] Capture descriptors at the 48, 88.2 and 96 kHz switch positions and
      in "advanced"/"2ch" modes (`Use2ch`, `AsioSmaller` registry values).
- [ ] Check the playback/capture start order and warm-up the device needs.
