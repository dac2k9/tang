//! The channels between the audio thread and the rest of the program, and what the two share, made in one place: the program (`run_session`) wires its engine to a sound card with it, and Tang
//! inside a host (`embed`, step 113) wires its engine to a plugin's `process` with it. What the audio thread is given (the graph and the end of the MIDI channel that it reads) and what the controller is
//! given (`EngineHandles`) are the two halves of the same set of channels, so they are made together.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use crate::app::controller::EngineHandles;
use crate::audio::{MIDI_CHANNEL_CAPACITY, MidiEvent};
use crate::held_notes::HeldNotes;
use crate::midi_monitor::MidiMonitor;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::plugin::Runtime;
use crate::plugin::chain::{AudioGraph, CHAIN_CHANNELS, GraphCommand, MidiLearnedNotification, PatternNotification};
use crate::scale::ScaleSetting;
use crate::session::PianoConfig;
use crate::telemetry::Telemetry;

/// How many commands the channel to the audio thread holds: what the audio thread has not taken yet. The thread takes all that are waiting at the start of every block.
const COMMANDS: usize = 64;

/// How many plugins the audio thread can hand back at once.
const RETURNED: usize = 16;

/// How many pattern recordings, and how many captured controls, wait for the controller.
const NOTIFICATIONS: usize = 64;

/// How long an undo waits for the audio thread to hand back a plugin that it was asked to keep (`EngineHandles::recovery_wait`): the audio thread takes a command within a block or two, 5 to 20 ms, so this is for one that is stalled, which an undo then does
/// without.
const RECOVERY_WAIT: Duration = Duration::from_millis(250);

/// Both ends of a session's channels.
pub struct Wiring {
    /// What renders: the audio thread's (a sound card's callback, a timer, a host's `process`).
    pub graph: AudioGraph,
    /// The notes that the keys and the devices send, which the audio thread reads at the start of every block.
    pub midi_rx: Receiver<MidiEvent>,
    /// What the controller is given.
    pub handles: EngineHandles,
}

impl Wiring {
    /// The channels for a session whose Piano tab is set as `piano` says (its scale and whether it is locked to it), that plugins are loaded with `runtime` at `sample_rate` in blocks of up to `max_block_size` frames, and
    /// whose incoming MIDI is tallied if `debug`.
    pub fn new(piano: Option<&PianoConfig>, sample_rate: f32, max_block_size: usize, runtime: Runtime, debug: bool) -> Wiring {
        let (midi_tx, midi_rx) = crossbeam_channel::bounded::<MidiEvent>(MIDI_CHANNEL_CAPACITY);
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<GraphCommand>(COMMANDS);
        let (return_tx, return_rx) = crossbeam_channel::bounded::<Box<dyn crate::plugin::Plugin>>(RETURNED);

        // Shared "currently held notes" state: populated by both the hardware MIDI thread and the virtual piano, read by the TUI Piano tab.
        let held = Arc::new(HeldNotes::new());

        // Shared piano-tab filter: current scale + Highlight/Locked mode. Read by both senders to drop off-scale notes when locked; written by the TUI.
        let scale = piano.and_then(|p| p.scale.as_deref().and_then(ScaleSetting::parse)).unwrap_or_default();
        let mode = if piano.is_some_and(|p| p.locked) { PianoMode::Locked } else { PianoMode::Highlight };
        let piano_filter = Arc::new(PianoFilter::new(scale, mode));

        // The empty audio graph (outputs silence until instruments are added), and what the audio thread sends back: finished pattern recordings, and the controls that a MIDI-learn captured.
        let mut graph = AudioGraph::new(CHAIN_CHANNELS, cmd_rx, return_tx);
        let (pattern_tx, pattern_rx) = crossbeam_channel::bounded::<PatternNotification>(NOTIFICATIONS);
        graph.set_pattern_tx(pattern_tx);
        let (learn_tx, learn_rx) = crossbeam_channel::bounded::<MidiLearnedNotification>(NOTIFICATIONS);
        graph.set_learn_tx(learn_tx);
        // The live piano scale, for in-key pattern transposition, and the cells it publishes its measurements in (meters, for now).
        graph.set_piano_filter(piano_filter.clone());
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());

        let handles = EngineHandles {
            cmd_tx,
            midi_tx,
            return_rx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate,
            max_block_size,
            held,
            piano_filter,
            midi_monitor: Arc::new(MidiMonitor::new(debug)),
            telemetry,
            recovery_wait: RECOVERY_WAIT,
        };
        Wiring { graph, midi_rx, handles }
    }
}
