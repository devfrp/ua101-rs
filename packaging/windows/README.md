# Windows

The release contains two stand-alone programs:

- `ua101-panel-windows-x86_64.exe`: the graphical control panel
- `ua101-windows-x86_64.exe`: the command-line tool (`ua101 list`, `ua101 info`, …)

## Driver binding (WinUSB)

A user-space driver talks to the device through Microsoft's generic
**WinUSB** driver. Windows only uses WinUSB once you bind it to the device:

1. Uninstall Roland's driver for the UA-101 if it is installed.
2. Run [Zadig](https://zadig.akeo.ie/), select *Options → List All Devices*.
3. For each of the UA-101 interfaces (`Interface 0`, `1` and `2`), select
   **WinUSB** and click *Replace Driver*.
4. Run `ua101-windows-x86_64.exe info`.

To go back to Roland's driver, uninstall the WinUSB device in Device Manager
(tick *Delete the driver software*) and reinstall Roland's package.

## What works on Windows

| Feature                     | Status                                         |
|-----------------------------|------------------------------------------------|
| Detection, descriptors      | yes                                            |
| Direct monitor (vendor req) | yes (request encoding inferred)                |
| MIDI in/out (bulk)          | yes                                            |
| Audio streaming             | not yet: needs the WinUSB isochronous backend  |
