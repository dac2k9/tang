use std::collections::HashSet;
use std::sync::Arc;

use crossbeam_channel::Sender;
use midir::{MidiInput, MidiInputConnection};

use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::PianoFilter;

pub struct MidiManager {
    sender: Sender<MidiEvent>,
    device_filter: Option<String>,
    connections: Vec<MidiInputConnection<()>>,
    connected_names: HashSet<String>,
    held: Arc<HeldNotes>,
    piano_filter: Arc<PianoFilter>,
    monitor: Arc<MidiMonitor>,
}

impl MidiManager {
    pub fn new(
        sender: Sender<MidiEvent>,
        device_filter: Option<String>,
        held: Arc<HeldNotes>,
        piano_filter: Arc<PianoFilter>,
        monitor: Arc<MidiMonitor>,
    ) -> Self {
        MidiManager {
            sender,
            device_filter,
            connections: Vec::new(),
            connected_names: HashSet::new(),
            held,
            piano_filter,
            monitor,
        }
    }

    /// Open all available MIDI input ports (or those matching the filter).
    /// Returns the number of newly opened connections.
    pub fn open_ports(&mut self) -> anyhow::Result<usize> {
        let midi_in = MidiInput::new("tang")?;
        let ports = midi_in.ports();
        let mut opened = 0;

        for port in &ports {
            let name = match midi_in.port_name(port) {
                Ok(n) => n,
                Err(_) => continue,
            };

            // Skip already connected
            if self.connected_names.contains(&name) {
                continue;
            }

            // Apply device filter
            if let Some(ref filter) = self.device_filter {
                if !name.contains(filter.as_str()) {
                    continue;
                }
            }

            let sender = self.sender.clone();
            let held = self.held.clone();
            let piano_filter = self.piano_filter.clone();
            let monitor = self.monitor.clone();
            let log_name = name.clone();
            let conn_name = name.clone();

            // Need a fresh MidiInput for each connection
            let midi_in_for_port = MidiInput::new("tang")?;
            match midi_in_for_port.connect(
                port,
                &conn_name,
                move |_timestamp_us, bytes, _| {
                    let status = bytes[0];
                    let kind = match status & 0xF0 {
                        0x90 => "NoteOn ",
                        0x80 => "NoteOff",
                        0xB0 => "CC     ",
                        0xE0 => "Bend   ",
                        0xD0 => "ChanPrs",
                        0xA0 => "KeyPrs ",
                        0xC0 => "PgmChg ",
                        _ => "Other  ",
                    };
                    let ch = status & 0x0F;
                    let note_info = match status & 0xF0 {
                        0x90 | 0x80 if bytes.len() >= 2 => {
                            format!(" {}", crate::note_name(bytes[1]))
                        }
                        _ => String::new(),
                    };
                    log::info!("MIDI in  [{log_name}] {kind} ch={ch}{note_info} data={bytes:02x?}");
                    // Feed the on-screen `--debug` monitor (no-op unless enabled).
                    monitor.record(bytes);
                    // Locked mode: drop note-ons for off-scale notes so they
                    // never reach the audio thread or the held bitset. Note-offs
                    // always pass through (clean up any sounding note).
                    let is_note_on = (status & 0xF0) == 0x90 && bytes.len() >= 3 && bytes[2] > 0;
                    if is_note_on && piano_filter.block_note_on(bytes[1]) {
                        return;
                    }

                    // Tap into the stream to track currently-held notes for the
                    // Piano tab. A NoteOn with velocity 0 means NoteOff.
                    if bytes.len() >= 3 {
                        match status & 0xF0 {
                            0x90 if bytes[2] > 0 => held.note_on(bytes[1]),
                            0x90 | 0x80 => held.note_off(bytes[1]),
                            _ => {}
                        }
                    }
                    // Timestamp 0 = place at start of next buffer
                    // Copy into fixed [u8; 3] — skip messages longer than 3 bytes (e.g. SysEx)
                    if !bytes.is_empty() && bytes.len() <= 3 {
                        let mut buf = [0u8; 3];
                        buf[..bytes.len()].copy_from_slice(bytes);
                        if sender.try_send((0, buf)).is_err() {
                            log::warn!("MIDI channel full — dropping event from {log_name}");
                        }
                    }
                },
                (),
            ) {
                Ok(conn) => {
                    log::info!("Opened MIDI input: {name}");
                    self.connected_names.insert(name);
                    self.connections.push(conn);
                    opened += 1;
                }
                Err(e) => {
                    log::warn!("Failed to open MIDI input {name}: {e}");
                }
            }
        }

        Ok(opened)
    }

    /// Poll for newly connected MIDI devices. Call periodically from main loop.
    pub fn poll_new_devices(&mut self) {
        match self.open_ports() {
            Ok(0) => {}
            Ok(n) => log::info!("Opened {n} new MIDI device(s)"),
            Err(e) => log::warn!("MIDI poll error: {e}"),
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}
