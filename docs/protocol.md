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

## MIDI (observed, static)

- Bulk transfers: URB function `0x09` (`0x19520`), IN with
  `TransferFlags = 3` (IN | SHORT_TRANSFER_OK) and a length of
  `wMaxPacketSize` (`0x1d100`).
- OUT transfers are sized `events * 4` from a ring of 32-bit entries
  (`0x1d780`): standard **USB-MIDI 1.0 event packets** (cable number + CIN,
  then 3 data bytes).

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
