//! Background connection to the unit's control cable.
//!
//! The thread owns the [`MidiPort`]: it sends queued DT1 messages and
//! forwards every DT1 the unit sends (front-panel changes) to the UI.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use ua101_usb::{MidiPort, protocol::sysex::Message};

pub enum Event {
    /// A DT1 received from the unit.
    Data([u8; 4], Vec<u8>),
    /// A message was sent.
    Sent(Message),
    /// The connection failed and the thread stopped.
    Error(String),
}

pub struct ControlLink {
    tx: Sender<Message>,
    pub events: Receiver<Event>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlLink {
    pub fn start(mut port: MidiPort, ctx: egui::Context) -> Self {
        let (tx, cmd_rx) = mpsc::channel::<Message>();
        let (ev_tx, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::Builder::new()
            .name("ua101-control".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    while let Ok(msg) = cmd_rx.try_recv() {
                        match port.send_control(&msg) {
                            Ok(()) => {
                                let _ = ev_tx.send(Event::Sent(msg));
                            }
                            Err(e) => {
                                let _ = ev_tx.send(Event::Error(e.to_string()));
                                ctx.request_repaint();
                                return;
                            }
                        }
                    }
                    match port.receive_control(Duration::from_millis(20)) {
                        Ok(msgs) => {
                            if !msgs.is_empty() {
                                for (a, d) in msgs {
                                    let _ = ev_tx.send(Event::Data(a, d));
                                }
                                ctx.request_repaint();
                            }
                        }
                        Err(e) => {
                            let _ = ev_tx.send(Event::Error(e.to_string()));
                            ctx.request_repaint();
                            return;
                        }
                    }
                }
                // Dropping the port hands the card back to the kernel driver.
            })
            .expect("spawn control thread");
        Self {
            tx,
            events,
            stop,
            thread: Some(thread),
        }
    }

    pub fn send(&self, msg: Message) {
        let _ = self.tx.send(msg);
    }

    pub fn is_alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for ControlLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
