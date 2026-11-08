use std::time::Duration;

use nusb::{
    Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, Bulk, In, Out, TransferError},
};
use ua101_protocol::{
    INTF_MIDI,
    descriptors::{Direction, find_bulk_endpoint},
    midi::{Encoder, EventPacket, decode_packet},
    sysex::{self, CONTROL_CABLE},
};

use crate::Result;

/// The UA-101 MIDI port (one USB-MIDI cable in each direction).
pub struct MidiPort {
    device: nusb::Device,
    // Option so Drop can release the interface before handing it back to
    // the kernel driver.
    interface: Option<Interface>,
    out_ep: Option<Endpoint<Bulk, Out>>,
    in_ep: Option<Endpoint<Bulk, In>>,
    encoders: [Encoder; 2],
    /// Bytes received on the control cable that have not formed a complete
    /// SysEx message yet.
    control_rx: Vec<u8>,
}

impl MidiPort {
    pub(crate) fn open(device: &nusb::Device, config: &[u8]) -> Result<Self> {
        let out_addr = find_bulk_endpoint(config, INTF_MIDI, Direction::Out)?.address;
        let in_addr = find_bulk_endpoint(config, INTF_MIDI, Direction::In)?.address;
        let interface = device.detach_and_claim_interface(INTF_MIDI).wait()?;
        let out_ep = interface.endpoint::<Bulk, Out>(out_addr)?;
        let in_ep = interface.endpoint::<Bulk, In>(in_addr)?;
        Ok(Self {
            device: device.clone(),
            interface: Some(interface),
            out_ep: Some(out_ep),
            in_ep: Some(in_ep),
            encoders: [Encoder::new(0), Encoder::new(CONTROL_CABLE)],
            control_rx: Vec::new(),
        })
    }

    /// Sends raw MIDI bytes (any mix of channel, system and SysEx messages)
    /// to the external MIDI OUT port (cable 0).
    pub fn send(&mut self, midi: &[u8], timeout: Duration) -> Result<()> {
        self.send_on_cable(0, midi, timeout)
    }

    /// Sends raw MIDI bytes on cable 0 (MIDI OUT jack) or cable 1 (the
    /// device's internal control port).
    pub fn send_on_cable(&mut self, cable: u8, midi: &[u8], timeout: Duration) -> Result<()> {
        let encoder = &mut self.encoders[(cable as usize).min(1)];
        let mut packets = Vec::with_capacity(midi.len() * 2);
        for &b in midi {
            if let Some(p) = encoder.push(b) {
                packets.extend_from_slice(&p);
            }
        }
        if packets.is_empty() {
            return Ok(());
        }
        self.send_packets_raw(packets, timeout)
    }

    /// Sends pre-built USB-MIDI event packets.
    pub fn send_packets(&mut self, packets: &[EventPacket], timeout: Duration) -> Result<()> {
        let bytes: Vec<u8> = packets.iter().flatten().copied().collect();
        self.send_packets_raw(bytes, timeout)
    }

    fn send_packets_raw(&mut self, bytes: Vec<u8>, timeout: Duration) -> Result<()> {
        let ep = self.out_ep.as_mut().expect("endpoint present until drop");
        let completion = ep.transfer_blocking(Buffer::from(bytes), timeout);
        completion.into_result()?;
        Ok(())
    }

    /// Waits up to `timeout` for incoming event packets. Returns an empty
    /// vector on timeout.
    pub fn receive_packets(&mut self, timeout: Duration) -> Result<Vec<EventPacket>> {
        let ep = self.in_ep.as_mut().expect("endpoint present until drop");
        let len = ep.max_packet_size().max(4);
        let completion = ep.transfer_blocking(Buffer::new(len), timeout);
        let buf = match completion.into_result() {
            Ok(buf) => buf,
            Err(TransferError::Cancelled) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(buf
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .filter(|p| decode_packet(p).is_some())
            .collect())
    }

    /// Waits up to `timeout` for MIDI bytes from the external MIDI IN port
    /// (cable 0). Control-cable traffic received meanwhile is buffered for
    /// [`read_parameters`](Self::read_parameters).
    pub fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for p in self.receive_packets(timeout)? {
            if let Some((cable, data)) = decode_packet(&p) {
                if cable == CONTROL_CABLE {
                    self.control_rx.extend_from_slice(data);
                } else {
                    out.extend_from_slice(data);
                }
            }
        }
        Ok(out)
    }

    /// Sends a pre-built SysEx message on the control cable.
    pub fn send_control(&mut self, message: &sysex::Message) -> Result<()> {
        self.send_on_cable(CONTROL_CABLE, message.as_bytes(), Duration::from_secs(1))
    }

    /// Waits up to `timeout` for DT1 messages the device sends on the
    /// control cable (front-panel changes and replies). External MIDI
    /// traffic received meanwhile is dropped.
    pub fn receive_control(&mut self, timeout: Duration) -> Result<Vec<([u8; 4], Vec<u8>)>> {
        for p in self.receive_packets(timeout)? {
            if let Some((CONTROL_CABLE, data)) = decode_packet(&p) {
                self.control_rx.extend_from_slice(data);
            }
        }
        let mut out = Vec::new();
        while let Some(end) = self.control_rx.iter().position(|&b| b == 0xf7) {
            let msg: Vec<u8> = self.control_rx.drain(..=end).collect();
            let start = msg.iter().position(|&b| b == 0xf0).unwrap_or(0);
            if let Ok(d) = sysex::parse_dt1(&msg[start..]) {
                out.push((d.address, d.data.to_vec()));
            }
        }
        Ok(out)
    }

    /// Writes mixer/device parameters with a Roland DT1 message on the
    /// control cable.
    pub fn write_parameters(&mut self, address: [u8; 4], data: &[u8]) -> Result<()> {
        let msg = sysex::dt1(address, data);
        self.send_on_cable(CONTROL_CABLE, msg.as_bytes(), Duration::from_secs(1))
    }

    /// Reads `size` bytes of device memory at `address` with a Roland RQ1
    /// request and returns every DT1 reply as `(address, data)`. Collection
    /// stops after `timeout` without new data.
    pub fn read_parameters(
        &mut self,
        address: [u8; 4],
        size: u32,
        timeout: Duration,
    ) -> Result<Vec<([u8; 4], Vec<u8>)>> {
        self.control_rx.clear();
        let msg = sysex::rq1(address, size);
        self.send_on_cable(CONTROL_CABLE, msg.as_bytes(), Duration::from_secs(1))?;
        let mut replies = Vec::new();
        let mut received = 0usize;
        loop {
            let packets = self.receive_packets(timeout)?;
            if packets.is_empty() {
                break;
            }
            for p in packets {
                if let Some((CONTROL_CABLE, data)) = decode_packet(&p) {
                    self.control_rx.extend_from_slice(data);
                }
            }
            while let Some(end) = self.control_rx.iter().position(|&b| b == 0xf7) {
                let msg: Vec<u8> = self.control_rx.drain(..=end).collect();
                let start = msg.iter().position(|&b| b == 0xf0).unwrap_or(0);
                match sysex::parse_dt1(&msg[start..]) {
                    Ok(d) => {
                        received += d.data.len();
                        replies.push((d.address, d.data.to_vec()));
                    }
                    Err(e) => log::debug!("ignored control message {:02x?}: {e:?}", &msg[start..]),
                }
            }
            if received >= size as usize {
                break;
            }
        }
        Ok(replies)
    }
}

impl Drop for MidiPort {
    fn drop(&mut self) {
        // Release the endpoints and the interface first, then give the
        // card back to the kernel's snd-ua101 driver (Linux only).
        self.out_ep.take();
        self.in_ep.take();
        if let Some(intf) = self.interface.take() {
            let _ = intf.release().wait();
        }
        crate::device::reattach_kernel_driver(&self.device);
    }
}
