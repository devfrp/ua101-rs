use std::time::Duration;

use nusb::{
    Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, Bulk, In, Out, TransferError},
};
use ua101_protocol::{
    INTF_MIDI,
    descriptors::{Direction, find_bulk_endpoint},
    midi::{Encoder, EventPacket, decode_packet},
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
    encoder: Encoder,
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
            encoder: Encoder::new(0),
        })
    }

    /// Sends raw MIDI bytes (any mix of channel, system and SysEx messages).
    pub fn send(&mut self, midi: &[u8], timeout: Duration) -> Result<()> {
        let mut packets = Vec::with_capacity(midi.len() * 2);
        for &b in midi {
            if let Some(p) = self.encoder.push(b) {
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

    /// Waits up to `timeout` for incoming MIDI bytes.
    pub fn receive(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for p in self.receive_packets(timeout)? {
            if let Some((_, data)) = decode_packet(&p) {
                out.extend_from_slice(data);
            }
        }
        Ok(out)
    }
}

impl Drop for MidiPort {
    fn drop(&mut self) {
        // Release the endpoints and the interface first, then give the
        // interface back to the kernel's snd-ua101 driver (Linux only).
        self.out_ep.take();
        self.in_ep.take();
        if let Some(intf) = self.interface.take() {
            let _ = intf.release().wait();
        }
        let _ = self.device.attach_kernel_driver(INTF_MIDI);
    }
}
