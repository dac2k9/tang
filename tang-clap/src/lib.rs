//! Tang as a CLAP plugin (step 113; `docs/reference/plugin-host.md`): two plugins in one file, **Tang** (an instrument: notes come in, what the session plays goes out) and **Tang FX** (an effect: audio comes in, and what the
//! session makes of it goes out), for a DAW that hosts CLAP, such as Bitwig.
//!
//! What a plugin is lies in the library (`tang::embed`): the engine that the host's `process` renders, the controller that serves the session to browsers, and the session as the text that a host keeps in its project. This crate is
//! the host's way into it: the CLAP entry, the two descriptors, the ports, the events turned to MIDI (`events`), the state (`state`), and a window that is a page in a browser (`browser`).
//!
//! The session is the plugin's whole state: what a person builds in the page (lanes, effects, cables, modulators, what the plugins inside keep in them) is saved with the host's project and comes back with it.

mod browser;
mod events;
mod state;

use std::ffi::CStr;
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, PluginGuiImpl, Window};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl};
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::{AUDIO_EFFECT, INSTRUMENT, STEREO, SYNTHESIZER};
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;
use tang::embed::{Host, HostMidi, Kind};

/// The ids that the plugins have. A host keeps a plugin by its id, in every project that has it: they are never changed.
pub const INSTRUMENT_ID: &str = "io.github.apanloco.tang.instrument";
pub const EFFECT_ID: &str = "io.github.apanloco.tang.effect";

/// What tells the two plugins apart.
pub trait Flavour: 'static {
    /// What the session starts as, and whether audio comes in.
    const KIND: Kind;
    const ID: &'static str;
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    fn features() -> &'static [&'static CStr];
}

/// Tang, the instrument.
pub struct InstrumentFlavour;

impl Flavour for InstrumentFlavour {
    const KIND: Kind = Kind::Instrument;
    const ID: &'static str = INSTRUMENT_ID;
    const NAME: &'static str = "Tang";
    const DESCRIPTION: &'static str = "A patch of instruments, effects, modulators and cables that is built in a page in the browser: notes in, sound out";
    fn features() -> &'static [&'static CStr] {
        &[INSTRUMENT, SYNTHESIZER, STEREO]
    }
}

/// Tang FX, the effect.
pub struct EffectFlavour;

impl Flavour for EffectFlavour {
    const KIND: Kind = Kind::Effect;
    const ID: &'static str = EFFECT_ID;
    const NAME: &'static str = "Tang FX";
    const DESCRIPTION: &'static str = "A patch of effects, modulators and cables that is built in a page in the browser, for the sound that comes in";
    fn features() -> &'static [&'static CStr] {
        &[AUDIO_EFFECT, STEREO]
    }
}

/// The descriptor that a host is told a plugin has.
fn descriptor<F: Flavour>() -> PluginDescriptor {
    PluginDescriptor::new(F::ID, F::NAME)
        .with_vendor("Tang")
        .with_url("https://github.com/apanloco/tang")
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_description(F::DESCRIPTION)
        .with_features(F::features().iter().copied())
}

/// A plugin of one of the two flavours.
pub struct TangPlugin<F>(PhantomData<fn() -> F>);

impl<F: Flavour> Plugin for TangPlugin<F> {
    type AudioProcessor<'a> = TangAudioProcessor<'a>;
    type Shared<'a> = TangShared;
    type MainThread<'a> = TangMainThread<'a, F>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&TangShared>) {
        builder.register::<PluginAudioPorts>().register::<PluginNotePorts>().register::<PluginParams>().register::<PluginState>().register::<PluginGui>();
    }
}

/// The plugin's one parameter, **Open the page**: a switch that opens the page of the instance in the browser, in the way of every host (a host that has a button for the plugin's window shows the page from it as well, but not every host has
/// one for a window that the plugin shows itself, and every host has the parameters of a plugin). It goes from 0 to 1 when it is pressed, opens the page, and is put back to 0 for the host to show.
pub const PAGE_PARAM: u32 = 0;

/// What the host's threads share: the state of the switch that opens the page, which the host sets on the audio thread (in a block) or on the main thread (when it is not processing) and which the main thread acts on.
pub struct TangShared {
    /// What the host last set the switch to (an `f64`'s bits): 0 or 1.
    page: AtomicU64,
    /// The switch was pressed, and the page has not been opened for it yet: the host's main thread does it when the host calls back (`request_callback`).
    open_requested: AtomicBool,
    /// The page was opened for a press: the switch is put back to 0 for the host, in the next block (or at the end of a flush).
    put_back: AtomicBool,
}

impl TangShared {
    fn new() -> TangShared {
        TangShared { page: AtomicU64::new(0f64.to_bits()), open_requested: AtomicBool::new(false), put_back: AtomicBool::new(false) }
    }

    /// The host sets the switch to `value`: true if that is a press (it was under a half, and is not), which asks for the page to be opened.
    fn host_sets(&self, value: f64) -> bool {
        let before = f64::from_bits(self.page.swap(value.to_bits(), Ordering::AcqRel));
        let pressed = value >= 0.5 && before < 0.5;
        if pressed {
            self.open_requested.store(true, Ordering::Release);
        }
        pressed
    }

    /// The switch as the host has it now.
    fn value(&self) -> f64 {
        f64::from_bits(self.page.load(Ordering::Acquire))
    }

    /// If a press has been acted on and the host does not know that the switch is back to 0: put it back, and say so. The event to tell the host with is for the caller to send.
    fn take_put_back(&self) -> bool {
        let put_back = self.put_back.swap(false, Ordering::AcqRel);
        if put_back {
            self.page.store(0f64.to_bits(), Ordering::Release);
        }
        put_back
    }

    /// The switch, if the event is one for it: the value it is set to.
    fn page_value(event: &UnknownEvent) -> Option<f64> {
        match event.as_core_event() {
            Some(CoreEventSpace::ParamValue(change)) if change.param_id() == Some(ClapId::new(PAGE_PARAM)) => Some(change.value()),
            _ => None,
        }
    }
}

impl PluginShared<'_> for TangShared {}

/// The event that tells a host that the switch is back to 0.
fn switch_is_back() -> ParamValueEvent {
    ParamValueEvent::new(0, ClapId::new(PAGE_PARAM), Pckn::match_all(), 0.0, Cookie::empty())
}

/// The host's main thread's end of an instance: the engine, which is started when the plugin is activated and has the session, the page and the controller.
pub struct TangMainThread<'a, F> {
    host: Host,
    /// Whether the host has made the plugin's window (`gui.create`), which is a page that `show` opens.
    window_made: bool,
    shared: &'a TangShared,
    _handle: HostMainThreadHandle<'a>,
    _flavour: PhantomData<fn() -> F>,
}

impl<'a, F: Flavour> PluginMainThread<'a, TangShared> for TangMainThread<'a, F> {
    /// The host calls back, as the audio thread asked (`request_callback`) when the switch that opens the page was pressed in a block.
    fn on_main_thread(&mut self) {
        if self.shared.open_requested.swap(false, Ordering::AcqRel) {
            // (A page that could not be opened has said why in the log; the switch goes back all the same, for the host to show that the press was seen.)
            let _ = self.open_page();
            self.shared.put_back.store(true, Ordering::Release);
        }
    }
}

impl<F: Flavour> TangMainThread<'_, F> {
    /// Open the page of this instance in the browser, and say why not if it cannot be (the log has the reason as well).
    fn open_page(&mut self) -> Result<(), PluginError> {
        let Some(url) = self.host.url() else {
            log::warn!("The page was asked for, and the plugin has no page: it has not started (the host starts it when it switches the plugin on), or the page could not be served");
            return Err(PluginError::Message("Tang has no page to show: it has not started yet, or the page could not be served (tang-plugin.log in the temporary files says)"));
        };
        log::info!("Opening {url}");
        browser::open(&url).map_err(|why| {
            log::error!("The browser could not be opened on {url}: {why}");
            PluginError::Message("the browser could not be opened: the address is in tang-plugin.log in the temporary files")
        })?;
        Ok(())
    }
}

/// The audio thread's end of an instance.
pub struct TangAudioProcessor<'a> {
    audio: tang::embed::Audio,
    shared: &'a TangShared,
    host: HostSharedHandle<'a>,
    /// The MIDI of the block (made once: a block asks the allocator for nothing unless a host sends more than a thousand messages in it).
    midi: Vec<HostMidi>,
    /// The audio that comes in, as a copy (a host may give the input and the output as the same memory), a side each.
    input: [Vec<f32>; 2],
    /// Whether the host has said `reset`, which the next block makes of an all-notes-off.
    reset: bool,
}

impl<'a, F: Flavour> PluginAudioProcessor<'a, TangShared, TangMainThread<'a, F>> for TangAudioProcessor<'a> {
    fn activate(host: HostAudioProcessorHandle<'a>, main_thread: &mut TangMainThread<'a, F>, shared: &'a TangShared, config: PluginAudioConfiguration) -> Result<Self, PluginError> {
        let frames = config.max_frames_count;
        let audio = main_thread.host.activate(config.sample_rate, frames).map_err(|why| {
            log::error!("Tang could not start: {why}");
            PluginError::Message("Tang could not start: the log, tang-plugin.log in the temporary files, says why")
        })?;
        let input = [vec![0.0; frames as usize], vec![0.0; frames as usize]];
        Ok(TangAudioProcessor { audio, shared, host: host.shared(), midi: Vec::with_capacity(1024), input, reset: false })
    }

    fn process(&mut self, _process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        self.midi.clear();
        if std::mem::take(&mut self.reset) {
            self.midi.extend(events::all_notes_off());
        }
        for event in events.input {
            events::convert(event, &mut self.midi);
            self.switch(event);
        }
        if self.shared.take_put_back() {
            let _ = events.output.try_push(switch_is_back());
        }

        let mut pair = audio.port_pair(0).ok_or(PluginError::Message("Tang has no audio port to play on"))?;
        let mut channels = pair.channels()?.into_f32().ok_or(PluginError::Message("Tang plays 32-bit audio"))?;
        let frames = (channels.frames_count() as usize).min(self.input[0].len());

        // The channels of the port, each as the buffer that is played into and (if the host gives audio in) the copy of what came in on it.
        let mut buffers: [Option<&mut [f32]>; 2] = [None, None];
        let mut heard = 0;
        for (index, (slot, pair)) in buffers.iter_mut().zip(channels.iter_mut()).enumerate() {
            match pair {
                ChannelPair::InputOnly(_) => {}
                ChannelPair::OutputOnly(out) => *slot = Some(out),
                ChannelPair::InPlace(buffer) => {
                    self.input[index][..frames].copy_from_slice(&buffer[..frames]);
                    heard = index + 1;
                    *slot = Some(buffer);
                }
                ChannelPair::InputOutput(input, out) => {
                    self.input[index][..frames].copy_from_slice(&input[..frames]);
                    heard = index + 1;
                    *slot = Some(out);
                }
            }
        }
        let [left, right] = &mut buffers;
        let [first, second] = &self.input;
        let input: [&[f32]; 2] = [&first[..frames], &second[..frames]];
        match (left.as_deref_mut(), right.as_deref_mut()) {
            (Some(left), Some(right)) => self.audio.process(&self.midi, &input[..heard], &mut [left, right]),
            (Some(left), None) => self.audio.process(&self.midi, &input[..heard], &mut [left]),
            _ => {}
        }
        Ok(ProcessStatus::Continue)
    }

    fn reset(&mut self) {
        self.reset = true;
    }
}

impl TangAudioProcessor<'_> {
    /// If the event sets the switch that opens the page, and it is a press: have the host call the main thread back, which opens the page (a page is not opened on the audio thread).
    fn switch(&self, event: &UnknownEvent) {
        if TangShared::page_value(event).is_some_and(|value| self.shared.host_sets(value)) {
            self.host.request_callback();
        }
    }
}

impl PluginAudioProcessorParams for TangAudioProcessor<'_> {
    /// The host sends the changes of its parameters without a block (it is not processing, or it only wants them taken): a press is asked for, and the switch is put back.
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        for event in input {
            self.switch(event);
        }
        if self.shared.take_put_back() {
            let _ = output.try_push(switch_is_back());
        }
    }
}

impl<F: Flavour> PluginMainThreadParams for TangMainThread<'_, F> {
    fn count(&mut self) -> u32 {
        1
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        if param_index == 0 {
            info.set(&ParamInfo {
                id: ClapId::new(PAGE_PARAM),
                flags: ParamInfoFlags::IS_STEPPED | ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Cookie::empty(),
                name: b"Open the page",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.0,
            });
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        (param_id == ClapId::new(PAGE_PARAM)).then(|| self.shared.value())
    }

    fn value_to_text(&mut self, param_id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> std::fmt::Result {
        use std::fmt::Write as _;
        if param_id != ClapId::new(PAGE_PARAM) {
            return Err(std::fmt::Error);
        }
        writer.write_str(if value >= 0.5 { "Open" } else { "Closed" })
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        if param_id != ClapId::new(PAGE_PARAM) {
            return None;
        }
        match text.to_str().ok()?.trim().to_ascii_lowercase().as_str() {
            "open" | "on" | "1" | "yes" => Some(1.0),
            "closed" | "off" | "0" | "no" => Some(0.0),
            _ => None,
        }
    }

    /// The host sends the changes of its parameters while the plugin is not processing (the main thread): a press opens the page at once, and the switch is put back.
    fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
        for event in input {
            if TangShared::page_value(event).is_some_and(|value| self.shared.host_sets(value)) {
                self.shared.open_requested.store(false, Ordering::Release);
                let _ = self.open_page();
                self.shared.put_back.store(true, Ordering::Release);
            }
        }
        if self.shared.take_put_back() {
            let _ = output.try_push(switch_is_back());
        }
    }
}

/// How many audio ports a plugin has on one side: an instrument has no audio in, an effect has one stereo input, and every plugin has one stereo output.
fn audio_ports<F: Flavour>(is_input: bool) -> u32 {
    u32::from(!is_input || F::KIND == Kind::Effect)
}

impl<F: Flavour> PluginAudioPortsImpl for TangMainThread<'_, F> {
    fn count(&mut self, is_input: bool) -> u32 {
        audio_ports::<F>(is_input)
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 && audio_ports::<F>(is_input) == 1 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(u32::from(!is_input)),
                name: if is_input { b"Audio in" } else { b"Audio out" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl<F: Flavour> PluginNotePortsImpl for TangMainThread<'_, F> {
    fn count(&mut self, is_input: bool) -> u32 {
        u32::from(is_input)
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if is_input && index == 0 {
            writer.set(&NotePortInfo { id: ClapId::new(0), name: b"Notes", preferred_dialect: Some(NoteDialect::Midi), supported_dialects: NoteDialects::MIDI | NoteDialects::CLAP });
        }
    }
}

impl<F: Flavour> PluginStateImpl for TangMainThread<'_, F> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let text = self.host.session_text().map_err(|why| {
            log::error!("The session could not be read out for the host's project: {why}");
            PluginError::Message("Tang could not read out its session")
        })?;
        output.write_all(&state::encode(&text)).map_err(|why| {
            log::error!("The host's project would not take the session: {why}");
            PluginError::Message("the host would not take Tang's session")
        })
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut bytes = Vec::new();
        input.take(state::MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|why| {
            log::error!("The host's project would not give the session: {why}");
            PluginError::Message("the host would not give Tang's session")
        })?;
        let text = state::decode(&bytes).map_err(|why| {
            log::error!("The state that the host gave is not a session: {why}");
            PluginError::Message("the state that the host gave is not a session of Tang's")
        })?;
        self.host.set_session_text(text).map_err(|why| {
            log::error!("The session that the host gave could not be loaded: {why}");
            PluginError::Message("Tang could not load the session that the host gave")
        })
    }
}

/// The plugin's window is a page in a browser, which the host's button for the window opens: a window that is a floating one that the plugin makes itself, as far as the host is concerned (it asks the plugin to show it, and does not
/// give it a window of its own to put anything in).
impl<F: Flavour> PluginGuiImpl for TangMainThread<'_, F> {
    fn is_api_supported(&mut self, configuration: GuiConfiguration) -> bool {
        configuration.is_floating
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        GuiApiType::default_for_current_platform().map(|api_type| GuiConfiguration { api_type, is_floating: true })
    }

    fn create(&mut self, configuration: GuiConfiguration) -> Result<(), PluginError> {
        if !configuration.is_floating {
            return Err(PluginError::Message("Tang's window is a page in a browser: it is not put in a window of the host's"));
        }
        self.window_made = true;
        Ok(())
    }

    fn destroy(&mut self) {
        self.window_made = false;
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> {
        Ok(())
    }

    fn get_size(&mut self) -> Option<GuiSize> {
        None
    }

    fn set_size(&mut self, _size: GuiSize) -> Result<(), PluginError> {
        Err(PluginError::Message("Tang's window is a page in a browser, and has no size of the host's to be"))
    }

    fn set_parent(&mut self, _window: Window) -> Result<(), PluginError> {
        Err(PluginError::Message("Tang's window is a page in a browser: it is not put in a window of the host's"))
    }

    fn set_transient(&mut self, _window: Window) -> Result<(), PluginError> {
        Ok(())
    }

    fn show(&mut self) -> Result<(), PluginError> {
        self.open_page()
    }

    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

fn new_shared(_host: HostSharedHandle<'_>) -> Result<TangShared, PluginError> {
    Ok(TangShared::new())
}

fn new_main_thread<'a, F: Flavour>(host: HostMainThreadHandle<'a>, shared: &'a TangShared) -> Result<TangMainThread<'a, F>, PluginError> {
    Ok(TangMainThread { host: Host::new(F::KIND, true), window_made: false, shared, _handle: host, _flavour: PhantomData })
}

/// The two plugins, as the host asks for them.
struct TangFactory {
    instrument: PluginDescriptor,
    effect: PluginDescriptor,
}

impl PluginFactoryImpl for TangFactory {
    fn plugin_count(&self) -> u32 {
        2
    }

    fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
        match index {
            0 => Some(&self.instrument),
            1 => Some(&self.effect),
            _ => None,
        }
    }

    fn create_plugin<'a>(&'a self, host_info: HostInfo<'a>, plugin_id: &CStr) -> Option<PluginInstance<'a>> {
        if plugin_id == self.instrument.id().unwrap_or_default() {
            Some(PluginInstance::new::<TangPlugin<InstrumentFlavour>>(host_info, &self.instrument, new_shared, new_main_thread::<InstrumentFlavour>))
        } else if plugin_id == self.effect.id().unwrap_or_default() {
            Some(PluginInstance::new::<TangPlugin<EffectFlavour>>(host_info, &self.effect, new_shared, new_main_thread::<EffectFlavour>))
        } else {
            None
        }
    }
}

/// Keep this library loaded for as long as the process lives (Windows). A host may unload a plugin's library when the last instance of it has gone, and Tang has threads that can be running by then: the scan of the installed plugins
/// (which loads every one of them, and goes on while nobody is looking) and the program that opens a page. Code that is unmapped under a thread that runs is a crash in the host; a library that is pinned is not unloaded by `FreeLibrary`.
#[cfg(windows)]
fn pin_library() {
    use std::ffi::c_void;

    /// `GET_MODULE_HANDLE_EX_FLAG_PIN`: the module is not unloaded until the process ends. `GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS`: the module is the one that the address is in.
    const PIN: u32 = 0x1;
    const FROM_ADDRESS: u32 = 0x4;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut *mut c_void) -> i32;
    }

    let mut module: *mut c_void = std::ptr::null_mut();
    // SAFETY: with `FROM_ADDRESS` the second argument is an address in the module (this function's, which is in this library), not a name, and the third is a place for a handle that is not used.
    let pinned = unsafe { GetModuleHandleExW(PIN | FROM_ADDRESS, pin_library as *const () as *const u16, &mut module) };
    if pinned == 0 {
        log::warn!("The library could not be pinned: a host that unloads it while a thread of Tang runs would crash");
    }
}

/// (Where a library is not unloaded under a thread by the system's habits, nothing is done.)
#[cfg(not(windows))]
fn pin_library() {}

/// The file's entry: the factory of the two plugins.
pub struct TangEntry {
    factory: PluginFactoryWrapper<TangFactory>,
}

impl Entry for TangEntry {
    fn new(_bundle_path: &CStr) -> Result<Self, EntryLoadError> {
        pin_library();
        Ok(TangEntry { factory: PluginFactoryWrapper::new(TangFactory { instrument: descriptor::<InstrumentFlavour>(), effect: descriptor::<EffectFlavour>() }) })
    }

    fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
        builder.register_factory(&self.factory);
    }
}

clack_export_entry!(TangEntry);
