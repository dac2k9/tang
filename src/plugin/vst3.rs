// vst3-rs binding constants (kRealtime, kSample32, kAudio, kInput, kOutput,
// kPlaying, kTempoValid) are typed as `u32` on Linux/macOS but `i32` on
// Windows. We cast `as i32` to satisfy Linux/macOS; on Windows those casts
// are no-ops and clippy::unnecessary_cast fires.
#![cfg_attr(target_os = "windows", allow(clippy::unnecessary_cast))]

use std::cell::UnsafeCell;
use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::Event_::EventTypes_::{
    kLegacyMIDICCOutEvent, kNoteOffEvent, kNoteOnEvent,
};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsHidden, kIsProgramChange, kIsReadOnly};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kPlaying;
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::kTempoValid;
use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
use vst3::Steinberg::Vst::SpeakerArr::{kMono, kStereo};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, BusInfo, Event, Event__type0, IAudioProcessor,
    IAudioProcessorTrait as _, IComponent, IComponentHandler, IComponentHandlerTrait,
    IComponentTrait as _, IConnectionPoint, IConnectionPointTrait as _, IEditController,
    IEditControllerTrait as _, IEventList, IEventListTrait, IHostApplication,
    IHostApplicationTrait, IMidiMapping, IMidiMappingTrait as _, IParamValueQueue,
    IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, IUnitInfo,
    IUnitInfoTrait as _, LegacyMIDICCOutEvent, NoteOffEvent,
    NoteOnEvent,
    ParameterInfo as Vst3ParameterInfo, ProcessContext, ProcessData, ProcessSetup,
    ProgramListInfo, String128,
};
use vst3::Steinberg::{
    self, FUnknown, IPluginBaseTrait as _, IPluginFactory, IPluginFactory2,
    IPluginFactory2Trait as _, IPluginFactoryTrait as _, PClassInfo, PClassInfo2, kResultOk,
};
use vst3::{Class, ComPtr, ComWrapper, Interface};

use super::{Editor, ParameterInfo, Plugin, PluginInfo, Preset};

mod editor;

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

fn string128_to_string(s: &String128) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn string_to_string128(s: &str) -> String128 {
    let mut buf: String128 = [0u16; 128];
    for (i, ch) in s.encode_utf16().take(127).enumerate() {
        buf[i] = ch;
    }
    buf
}

fn char_array_to_string(s: &[c_char]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    s[..end].iter().map(|&c| c as u8 as char).collect()
}

/// Convert a `Guid` ([u8; 16]) to a TUID ([c_char; 16]) for passing to createInstance.
fn guid_to_tuid(guid: &vst3::com_scrape_types::Guid) -> Steinberg::TUID {
    let mut tuid: Steinberg::TUID = [0; 16];
    for i in 0..16 {
        tuid[i] = guid[i] as c_char;
    }
    tuid
}

// ---------------------------------------------------------------------------
// Platform-specific paths
// ---------------------------------------------------------------------------

fn vst3_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".vst3"));
        }
        paths.push(PathBuf::from("/usr/lib/vst3"));
        paths.push(PathBuf::from("/usr/local/lib/vst3"));
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"));
        }
        paths.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            paths.push(PathBuf::from(local).join("Programs/Common/VST3"));
        }
        paths.push(PathBuf::from(r"C:\Program Files\Common Files\VST3"));
    }

    paths.extend(crate::config::extra_vst3_paths().iter().cloned());

    paths
}

/// Resolve a .vst3 bundle to its platform-specific shared library path.
fn bundle_binary_path(bundle: &Path) -> PathBuf {
    let stem = bundle
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    #[cfg(target_os = "linux")]
    {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64-linux",
            _ => "x86_64-linux",
        };
        bundle
            .join("Contents")
            .join(arch)
            .join(format!("{stem}.so"))
    }

    #[cfg(target_os = "macos")]
    {
        bundle.join("Contents").join("MacOS").join(stem)
    }

    #[cfg(target_os = "windows")]
    {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "aarch64-win",
            _ => "x86_64-win",
        };
        bundle
            .join("Contents")
            .join(arch)
            .join(format!("{stem}.vst3"))
    }
}

/// Recursively find all .vst3 bundles under the given directory.
fn find_vst3_bundles(dir: &Path) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("vst3"))
                {
                    bundles.push(path);
                } else {
                    stack.push(path);
                }
            }
        }
    }
    bundles.sort();
    bundles
}

// ---------------------------------------------------------------------------
// macOS CoreFoundation helpers for VST3 bundle loading
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod cf {
    use std::ffi::{c_void, CString};
    use std::path::Path;

    // Opaque CoreFoundation types
    pub type CFBundleRef = *mut c_void;
    type CFURLRef = *mut c_void;
    type CFAllocatorRef = *mut c_void;
    type CFStringRef = *mut c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFAllocatorDefault: CFAllocatorRef;
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFURLCreateWithFileSystemPath(
            allocator: CFAllocatorRef,
            file_path: CFStringRef,
            path_style: i32,
            is_directory: bool,
        ) -> CFURLRef;
        fn CFBundleCreate(allocator: CFAllocatorRef, bundle_url: CFURLRef) -> CFBundleRef;
        fn CFRelease(cf: *mut c_void);
    }

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;
    const K_CF_URL_POSIX_PATH_STYLE: i32 = 0;

    /// Create a `CFBundleRef` for the given path. Returns null on failure.
    /// The caller must release it with `release_bundle` when done.
    pub fn create_bundle_ref(path: &Path) -> CFBundleRef {
        let c_path = match CString::new(path.to_str().unwrap_or("")) {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };
        unsafe {
            let cf_str = CFStringCreateWithCString(
                kCFAllocatorDefault,
                c_path.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            if cf_str.is_null() {
                return std::ptr::null_mut();
            }
            let cf_url = CFURLCreateWithFileSystemPath(
                kCFAllocatorDefault,
                cf_str,
                K_CF_URL_POSIX_PATH_STYLE,
                true,
            );
            CFRelease(cf_str);
            if cf_url.is_null() {
                return std::ptr::null_mut();
            }
            let bundle = CFBundleCreate(kCFAllocatorDefault, cf_url);
            CFRelease(cf_url);
            bundle
        }
    }

    /// Release a `CFBundleRef` obtained from `create_bundle_ref`.
    pub fn release_bundle(bundle: CFBundleRef) {
        if !bundle.is_null() {
            unsafe { CFRelease(bundle) };
        }
    }
}

// ---------------------------------------------------------------------------
// Module loading
// ---------------------------------------------------------------------------

struct Vst3Module {
    factory: Option<ComPtr<IPluginFactory>>,
    #[cfg(target_os = "linux")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    #[cfg(target_os = "macos")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    #[cfg(target_os = "windows")]
    exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>>,
    #[cfg(target_os = "macos")]
    _bundle_ref: cf::CFBundleRef,
    // SAFETY: Library must be dropped after factory and exit_fn.
    // Rust drops fields in declaration order, so this is correct.
    _library: libloading::Library,
}

impl Vst3Module {
    fn load(bundle_path: &Path) -> anyhow::Result<Self> {
        let binary = bundle_binary_path(bundle_path);
        if !binary.exists() {
            anyhow::bail!("VST3 binary not found: {}", binary.display());
        }

        // Safety: loading external dynamic libraries is inherently unsafe
        let library = unsafe { libloading::Library::new(&binary) }.map_err(|e| {
            anyhow::anyhow!("Failed to load VST3 library {}: {e}", binary.display())
        })?;

        // Call platform entry function
        #[cfg(target_os = "linux")]
        {
            let entry: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> bool> =
                unsafe { library.get(b"ModuleEntry") }
                    .map_err(|e| anyhow::anyhow!("ModuleEntry not found: {e}"))?;
            let ok = unsafe { entry(std::ptr::null_mut()) };
            if !ok {
                anyhow::bail!("ModuleEntry returned false");
            }
        }
        #[cfg(target_os = "macos")]
        let bundle_ref = {
            let bundle_ref = cf::create_bundle_ref(bundle_path);
            if bundle_ref.is_null() {
                anyhow::bail!(
                    "Failed to create CFBundleRef for {}",
                    bundle_path.display()
                );
            }
            let entry: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> bool> =
                unsafe { library.get(b"bundleEntry") }
                    .map_err(|e| anyhow::anyhow!("bundleEntry not found: {e}"))?;
            let ok = unsafe { entry(bundle_ref) };
            if !ok {
                cf::release_bundle(bundle_ref);
                anyhow::bail!("bundleEntry returned false");
            }
            bundle_ref
        };
        #[cfg(target_os = "windows")]
        {
            if let Ok(entry) = unsafe { library.get::<unsafe extern "C" fn() -> bool>(b"InitDll") }
            {
                let ok = unsafe { entry() };
                if !ok {
                    anyhow::bail!("InitDll returned false");
                }
            }
        }

        // Get exit function (stored for Drop)
        // SAFETY: We transmute the lifetime of the Symbol to 'static because
        // we guarantee _library outlives exit_fn (field drop order).
        #[cfg(target_os = "linux")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"ModuleExit")
                .ok()
                .map(|s| std::mem::transmute(s))
        };
        #[cfg(target_os = "macos")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"bundleExit")
                .ok()
                .map(|s| std::mem::transmute(s))
        };
        #[cfg(target_os = "windows")]
        let exit_fn: Option<libloading::Symbol<'static, unsafe extern "C" fn() -> bool>> = unsafe {
            library
                .get::<unsafe extern "C" fn() -> bool>(b"ExitDll")
                .ok()
                .map(|s| std::mem::transmute(s))
        };

        // Get plugin factory
        let get_factory: libloading::Symbol<unsafe extern "C" fn() -> *mut IPluginFactory> =
            unsafe { library.get(b"GetPluginFactory") }
                .map_err(|e| anyhow::anyhow!("GetPluginFactory not found: {e}"))?;
        let factory_ptr = unsafe { get_factory() };
        let factory = unsafe { ComPtr::from_raw(factory_ptr) }
            .ok_or_else(|| anyhow::anyhow!("GetPluginFactory returned null"))?;

        Ok(Vst3Module {
            factory: Some(factory),
            exit_fn,
            #[cfg(target_os = "macos")]
            _bundle_ref: bundle_ref,
            _library: library,
        })
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory.as_ref().expect("factory already dropped")
    }
}

impl Drop for Vst3Module {
    fn drop(&mut self) {
        // Drop factory first to release COM references
        self.factory.take();
        // Call exit function
        if let Some(ref exit) = self.exit_fn {
            unsafe {
                exit();
            }
        }
        // Release the CFBundleRef on macOS
        #[cfg(target_os = "macos")]
        cf::release_bundle(self._bundle_ref);
        // _library drops last (unloads the .so/.dylib/.dll)
    }
}

// Safety: a module is shared (`Arc`) between a plugin and the editor of its window to keep the library loaded for as long as either is, and nobody calls into it from two threads: the factory is used by `load`, on the
// thread that loads, and what a shared module does is be dropped, by whichever of the two is the last, on the thread that drops it (as a plugin's module always was).
unsafe impl Send for Vst3Module {}
unsafe impl Sync for Vst3Module {}

// ---------------------------------------------------------------------------
// Host COM objects
// ---------------------------------------------------------------------------

struct TangHostApp;

impl Class for TangHostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for TangHostApp {
    unsafe fn getName(&self, name: *mut String128) -> Steinberg::tresult {
        unsafe { *name = string_to_string128("tang") };
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        _cid: *mut Steinberg::TUID,
        _iid: *mut Steinberg::TUID,
        _obj: *mut *mut c_void,
    ) -> Steinberg::tresult {
        vst3::Steinberg::kNotImplemented
    }
}

struct TangComponentHandler;

impl Class for TangComponentHandler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for TangComponentHandler {
    unsafe fn beginEdit(&self, _id: vst3::Steinberg::Vst::ParamID) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn performEdit(
        &self,
        _id: vst3::Steinberg::Vst::ParamID,
        _value_normalized: vst3::Steinberg::Vst::ParamValue,
    ) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: vst3::Steinberg::Vst::ParamID) -> Steinberg::tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: Steinberg::int32) -> Steinberg::tresult {
        log::debug!("VST3 plugin requested restart (ignored)");
        kResultOk
    }
}

// ---------------------------------------------------------------------------
// Process-time COM objects
// ---------------------------------------------------------------------------

struct TangEventList {
    events: UnsafeCell<Vec<Event>>,
}

impl Class for TangEventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for TangEventList {
    unsafe fn getEventCount(&self) -> Steinberg::int32 {
        unsafe { (*self.events.get()).len() as Steinberg::int32 }
    }

    unsafe fn getEvent(&self, index: Steinberg::int32, e: *mut Event) -> Steinberg::tresult {
        unsafe {
            let events = &*self.events.get();
            if let Some(event) = events.get(index as usize) {
                *e = *event;
                kResultOk
            } else {
                vst3::Steinberg::kResultFalse
            }
        }
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> Steinberg::tresult {
        vst3::Steinberg::kResultFalse
    }
}

struct TangParamValueQueue {
    param_id: UnsafeCell<u32>,
    value: UnsafeCell<f64>,
}

impl Class for TangParamValueQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for TangParamValueQueue {
    unsafe fn getParameterId(&self) -> vst3::Steinberg::Vst::ParamID {
        unsafe { *self.param_id.get() }
    }

    unsafe fn getPointCount(&self) -> Steinberg::int32 {
        1
    }

    unsafe fn getPoint(
        &self,
        index: Steinberg::int32,
        sample_offset: *mut Steinberg::int32,
        value: *mut vst3::Steinberg::Vst::ParamValue,
    ) -> Steinberg::tresult {
        if index == 0 {
            unsafe {
                *sample_offset = 0;
                *value = *self.value.get();
            }
            kResultOk
        } else {
            vst3::Steinberg::kResultFalse
        }
    }

    unsafe fn addPoint(
        &self,
        _sample_offset: Steinberg::int32,
        value: vst3::Steinberg::Vst::ParamValue,
        index: *mut Steinberg::int32,
    ) -> Steinberg::tresult {
        // Accept the point (store latest value), but we only track one point
        unsafe {
            *self.value.get() = value;
            if !index.is_null() {
                *index = 0;
            }
        }
        kResultOk
    }
}

const MAX_PARAM_QUEUES: usize = 64;

struct TangParameterChanges {
    count: UnsafeCell<i32>,
    queues: Vec<ComWrapper<TangParamValueQueue>>,
}

impl Class for TangParameterChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for TangParameterChanges {
    unsafe fn getParameterCount(&self) -> Steinberg::int32 {
        unsafe { *self.count.get() }
    }

    unsafe fn getParameterData(&self, index: Steinberg::int32) -> *mut IParamValueQueue {
        if (index as usize) < (unsafe { *self.count.get() } as usize) {
            self.queues
                .get(index as usize)
                .and_then(|q| q.as_com_ref::<IParamValueQueue>())
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut())
        } else {
            std::ptr::null_mut()
        }
    }

    unsafe fn addParameterData(
        &self,
        id: *const vst3::Steinberg::Vst::ParamID,
        index: *mut Steinberg::int32,
    ) -> *mut IParamValueQueue {
        unsafe {
            let count = *self.count.get();
            if (count as usize) < self.queues.len() {
                *self.queues[count as usize].param_id.get() = *id;
                *self.queues[count as usize].value.get() = 0.0;
                *self.count.get() = count + 1;
                if !index.is_null() {
                    *index = count;
                }
                self.queues[count as usize]
                    .as_com_ref::<IParamValueQueue>()
                    .map(|r| r.as_ptr())
                    .unwrap_or(std::ptr::null_mut())
            } else {
                std::ptr::null_mut()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Vst3Plugin
// ---------------------------------------------------------------------------

pub struct Vst3Plugin {
    name: String,
    is_instrument: bool,
    sample_rate: f32,
    audio_in_channel_count: usize,
    audio_out_channel_count: usize,
    audio_in_bus_count: usize,
    audio_out_bus_count: usize,
    /// How many frames late what the plugin makes is (`IAudioProcessor::getLatencySamples`), as it said when it was set up (`Plugin::latency`). A plugin that changes it while it
    /// runs, and tells the host by `restartComponent(kLatencyChanged)`, is not followed: the plugin is on the audio thread by then, and the host's handler ignores the restart.
    latency: u32,
    separate_controller: bool,
    params_cache: Vec<ParameterInfo>,
    param_ids: Vec<u32>,
    /// For each parameter, how many steps it has as a list (0 for a number): the value that the page and the terminal give is the step, not the plain value.
    param_steps: Vec<u32>,
    pending_param_changes: Vec<(u32, f64)>,
    preset_cache: Vec<Preset>,
    preset_param_id: Option<u32>,
    preset_count: usize,
    // Pre-allocated audio buffers
    output_bufs: Vec<Vec<f32>>,
    input_bufs: Vec<Vec<f32>>,
    // Pre-allocated per-callback scratch arrays (sized at load time).
    // Held to keep the channelBuffers32 raw pointers in output_buses/input_buses valid.
    #[expect(dead_code, reason = "backing storage for raw pointers in output_buses/input_buses")]
    output_ptrs: Vec<*mut f32>,
    #[expect(dead_code, reason = "backing storage for raw pointers in output_buses/input_buses")]
    input_ptrs: Vec<*mut f32>,
    output_buses: Vec<AudioBusBuffers>,
    input_buses: Vec<AudioBusBuffers>,
    // Process-time COM objects (pre-allocated, reused each process() call)
    param_changes: ComWrapper<TangParameterChanges>,
    output_param_changes: ComWrapper<TangParameterChanges>,
    event_list: ComWrapper<TangEventList>,
    // MIDI CC → parameter mapping (index = CC number, 128 = pitch bend)
    cc_param_map: Vec<Option<u32>>,
    // Note expression: noteId counter and channel→noteId tracking for pitch bend→tuning
    next_note_id: i32,
    // Active notes per channel: channel (0-15) → Vec of (pitch, noteId)
    channel_notes: Vec<Vec<(i16, i32)>>,
    // Connection points (for disconnect on Drop)
    comp_connection: Option<ComPtr<IConnectionPoint>>,
    ctrl_connection: Option<ComPtr<IConnectionPoint>>,
    // COM pointers into the loaded library — must drop before _module.
    // Rust drops fields in declaration order, so these must come before _module
    // to ensure Release() calls go through valid vtable pointers.
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: ComPtr<IEditController>,
    _handler: ComWrapper<TangComponentHandler>,
    _host_app: ComWrapper<TangHostApp>,
    // SAFETY: _module must be the last field. It unloads the shared library on
    // drop, so all ComPtrs referencing objects from the library must drop first.
    // (Shared with the plugin's editor, which a window of the plugin holds, so that no window outlives the library; the last of the two to go unloads it.)
    _module: Arc<Vst3Module>,
}

// Safety: Same justification as CLAP — the plugin is created, activated, and
// preset-loaded on the main thread, then moved into the audio callback closure.
// Only process() is called on the audio thread. On shutdown the plugin is sent
// back to the main thread for deactivation and drop.
unsafe impl Send for Vst3Plugin {}

impl Drop for Vst3Plugin {
    fn drop(&mut self) {
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            // Disconnect before terminating
            if let (Some(cc), Some(tc)) = (&self.comp_connection, &self.ctrl_connection) {
                cc.disconnect(tc.as_ptr());
                tc.disconnect(cc.as_ptr());
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}

// ---------------------------------------------------------------------------
// MIDI → VST3 event conversion
// ---------------------------------------------------------------------------

fn make_note_on(
    channel: i16,
    pitch: i16,
    velocity: f32,
    sample_offset: i32,
    note_id: i32,
) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: sample_offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kNoteOnEvent as u16,
        __field0: Event__type0 {
            noteOn: NoteOnEvent {
                channel,
                pitch,
                tuning: 0.0,
                velocity,
                length: 0,
                noteId: note_id,
            },
        },
    }
}

fn make_note_off(
    channel: i16,
    pitch: i16,
    velocity: f32,
    sample_offset: i32,
    note_id: i32,
) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: sample_offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kNoteOffEvent as u16,
        __field0: Event__type0 {
            noteOff: NoteOffEvent {
                channel,
                pitch,
                velocity,
                noteId: note_id,
                tuning: 0.0,
            },
        },
    }
}

fn make_legacy_pitch_bend(channel: i8, lsb: i8, msb: i8, sample_offset: i32) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: sample_offset,
        ppqPosition: 0.0,
        flags: 0,
        r#type: kLegacyMIDICCOutEvent as u16,
        __field0: Event__type0 {
            midiCCOut: LegacyMIDICCOutEvent {
                controlNumber: 129, // kPitchBend
                channel,
                value: lsb,
                value2: msb,
            },
        },
    }
}

// ---------------------------------------------------------------------------
// Plugin trait implementation
// ---------------------------------------------------------------------------

impl Plugin for Vst3Plugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_instrument(&self) -> bool {
        self.is_instrument
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_input_count(&self) -> usize {
        self.audio_in_channel_count
    }

    fn audio_output_count(&self) -> usize {
        self.audio_out_channel_count
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }
        // Clamp to our pre-allocated capacity. Should never trigger in practice
        // since we allocate with 2x headroom, but better silent truncation than
        // a panic on the audio thread.
        let frames = frames.min(self.output_bufs.first().map(|b| b.len()).unwrap_or(frames));

        // Populate event list with MIDI note events
        let events = unsafe { &mut *self.event_list.events.get() };
        events.clear();

        // Populate parameter changes
        let param_changes_count = unsafe { &mut *self.param_changes.count.get() };
        *param_changes_count = 0;
        // Reset output parameter changes (plugin writes into this)
        unsafe { *self.output_param_changes.count.get() = 0 };
        let mut queue_idx = 0usize;

        // Queue pending parameter changes from set_parameter()
        for &(param_id, value) in &self.pending_param_changes {
            if queue_idx < MAX_PARAM_QUEUES {
                unsafe {
                    *self.param_changes.queues[queue_idx].param_id.get() = param_id;
                    *self.param_changes.queues[queue_idx].value.get() = value;
                }
                queue_idx += 1;
            }
        }
        self.pending_param_changes.clear();

        // Convert MIDI events
        for &(timestamp, bytes) in midi_events {
            let status = bytes[0] & 0xF0;
            let channel = (bytes[0] & 0x0F) as i16;
            let pitch = bytes[1] as i16;
            let sample_offset = timestamp as i32;

            match status {
                0x90 if bytes[2] > 0 => {
                    let velocity = bytes[2] as f32 / 127.0;
                    let note_id = self.next_note_id;
                    self.next_note_id = self.next_note_id.wrapping_add(1);
                    self.channel_notes[channel as usize].push((pitch, note_id));
                    events.push(make_note_on(
                        channel,
                        pitch,
                        velocity,
                        sample_offset,
                        note_id,
                    ));
                    log::debug!(
                        "VST3: note on ch={channel} pitch={pitch} vel={} id={note_id}",
                        bytes[2],
                    );
                }
                0x80 | 0x90 => {
                    let velocity = bytes[2] as f32 / 127.0;
                    // Find and remove the noteId for this channel+pitch
                    let note_id = self.channel_notes[channel as usize]
                        .iter()
                        .rposition(|(p, _)| *p == pitch)
                        .map(|idx| self.channel_notes[channel as usize].remove(idx).1)
                        .unwrap_or(-1);
                    events.push(make_note_off(
                        channel,
                        pitch,
                        velocity,
                        sample_offset,
                        note_id,
                    ));
                    log::debug!(
                        "VST3: note off ch={channel} pitch={pitch} vel={} id={note_id}",
                        bytes[2],
                    );
                }
                0xE0 => {
                    // Per-channel pitch bend via LegacyMIDICCOutEvent.
                    // This preserves the MIDI channel, which the note remapper
                    // uses (channels 2-16) for per-note detune.
                    events.push(make_legacy_pitch_bend(
                        channel as i8,
                        bytes[1] as i8,
                        bytes[2] as i8,
                        sample_offset,
                    ));
                    log::debug!(
                        "VST3: pitch bend ch={channel} lsb={} msb={}",
                        bytes[1],
                        bytes[2],
                    );
                }
                0xB0 => {
                    // CC → parameter change via MIDI mapping
                    let cc = bytes[1] as usize;
                    if let Some(param_id) = self.cc_param_map.get(cc).copied().flatten() {
                        let value = bytes[2] as f64 / 127.0;
                        if queue_idx < MAX_PARAM_QUEUES {
                            unsafe {
                                *self.param_changes.queues[queue_idx].param_id.get() = param_id;
                                *self.param_changes.queues[queue_idx].value.get() = value;
                            }
                            queue_idx += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        *param_changes_count = queue_idx as i32;

        // Zero output buffers; copy input audio into preallocated input buffers.
        // Buffers are sized to max_block_size at load time so resize never grows.
        for buf in &mut self.output_bufs {
            buf[..frames].fill(0.0);
        }
        for (ch, buf) in self.input_bufs.iter_mut().enumerate() {
            if ch < audio_in.len() {
                let copy_len = frames.min(audio_in[ch].len());
                buf[..copy_len].copy_from_slice(&audio_in[ch][..copy_len]);
                if copy_len < frames {
                    buf[copy_len..frames].fill(0.0);
                }
            } else {
                buf[..frames].fill(0.0);
            }
        }

        let param_changes_ptr = self
            .param_changes
            .as_com_ref::<IParameterChanges>()
            .unwrap()
            .as_ptr();
        let event_list_ptr = self.event_list.as_com_ref::<IEventList>().unwrap().as_ptr();

        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.state = (kPlaying | kTempoValid) as _;
        context.sampleRate = self.sample_rate as f64;
        context.tempo = 120.0;

        let mut process_data = ProcessData {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.audio_in_bus_count as i32,
            numOutputs: self.audio_out_bus_count as i32,
            inputs: if self.audio_in_bus_count > 0 {
                self.input_buses.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            outputs: if self.audio_out_bus_count > 0 {
                self.output_buses.as_mut_ptr()
            } else {
                std::ptr::null_mut()
            },
            inputParameterChanges: param_changes_ptr,
            outputParameterChanges: self
                .output_param_changes
                .as_com_ref::<IParameterChanges>()
                .unwrap()
                .as_ptr(),
            inputEvents: event_list_ptr,
            outputEvents: std::ptr::null_mut(),
            processContext: &mut context,
        };

        let result = unsafe { self.processor.process(&mut process_data) };
        if result != kResultOk {
            log::warn!("VST3 process returned {result}");
        }

        // Copy output to caller's buffers (only the frames we processed)
        for (ch, out_slice) in audio_out.iter_mut().enumerate() {
            if ch < self.output_bufs.len() {
                let copy_len = out_slice.len().min(frames);
                out_slice[..copy_len].copy_from_slice(&self.output_bufs[ch][..copy_len]);
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        self.params_cache.clone()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        let param_id = *self.param_ids.get(index as usize)?;
        let normalized = unsafe { self.controller.getParamNormalized(param_id) };
        let steps = self.param_steps[index as usize];
        Some(normalized_to_value(steps, normalized, |normalized| unsafe { self.controller.normalizedParamToPlain(param_id, normalized) }))
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let param_id = *self
            .param_ids
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("Parameter index out of range: {index}"))?;
        let steps = self.param_steps[index as usize];
        let normalized = value_to_normalized(steps, value, |plain| unsafe { self.controller.plainParamToNormalized(param_id, plain) });
        self.pending_param_changes.push((param_id, normalized));
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        self.preset_cache.clone()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let index: usize = id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid preset ID: {id}"))?;
        let preset_param_id = self
            .preset_param_id
            .ok_or_else(|| anyhow::anyhow!("Plugin does not support program changes"))?;

        if self.preset_count == 0 {
            anyhow::bail!("No presets available");
        }

        // Set program change parameter: normalized value = index / (count - 1)
        let normalized = if self.preset_count == 1 {
            0.0
        } else {
            index as f64 / (self.preset_count - 1) as f64
        };

        unsafe {
            self.controller
                .setParamNormalized(preset_param_id, normalized);
        }
        self.pending_param_changes
            .push((preset_param_id, normalized));

        log::info!("VST3: loaded preset {id}");
        Ok(())
    }

    /// The plugin's own window (step 112; `editor.rs`). Every VST3 plugin is offered as having one: whether it has is only known when its controller is asked for a view, which is done when the window is opened, and a plugin
    /// that has none says so then.
    fn editor(&self) -> Option<Rc<dyn Editor>> {
        Some(Rc::new(editor::Vst3Editor::of(self.controller.clone(), self._module.clone())))
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// How many MIDI channels and controllers a plugin can map to parameters: 16 channels, and 130 controllers (0-127 CC, 128 pitch bend, 129 aftertouch).
const MIDI_CHANNELS: i16 = 16;
const MIDI_CONTROLLERS: i16 = 130;

/// Whether a person is shown a parameter of a plugin: not one that the plugin hides or says is read only, and not one that is only there so that a
/// host can send MIDI controllers through the parameters (`midi_controller`: the plugin's MIDI mapping points at it) and that cannot be automated.
/// A plugin that does the last has a parameter for each of 16 channels and 130 controllers, which is most of its list (2,080 of the 2,294 of
/// Pianoteq 9, and of 2,089 of ROLI Airwave Player's); a real parameter that a controller is mapped to, a mod wheel to a depth, can be automated and stays.
fn is_listed(flags: i32, midi_controller: bool) -> bool {
    let hidden = flags & (kIsHidden | kIsReadOnly) != 0;
    let only_for_midi = midi_controller && flags & kCanAutomate == 0;
    !hidden && !only_for_midi
}

/// The most steps that a parameter can have and still be shown as a list of their names: one with more (a program number of 128) is a number.
const MAX_LIST_STEPS: i32 = 64;

/// Whether a parameter that has `steps` steps (VST3's `stepCount`: 0 for a continuous one, 1 for a switch) is a list: one that has steps, and not very many.
/// A list is an enum to the page and the terminal (`ParameterInfo::labels`): its names are what the plugin says at each step ("Off" and "On", "C" to "B").
fn is_list(steps: i32) -> bool {
    (1..=MAX_LIST_STEPS).contains(&steps)
}

/// The normalized value of step `step` of a parameter with `steps` steps: the steps of a VST3 parameter are k/steps, whatever the plain range that the plugin
/// says (Pianoteq's is 0 to 1 for all of them, a plugin that is made with a choice parameter has 0 to steps). The page and the terminal give the step.
fn step_to_normalized(step: f32, steps: u32) -> f64 {
    (step as f64 / steps as f64).clamp(0.0, 1.0)
}

/// The step of a parameter with `steps` steps that a normalized value is nearest.
fn normalized_to_step(normalized: f64, steps: u32) -> f32 {
    (normalized * steps as f64).round() as f32
}

/// The normalized value for the `value` that is given for a parameter with `steps` steps (0 for a number): the step, for a list, and the plugin's own
/// conversion of a plain value (`plain_to_normalized`) for any other.
fn value_to_normalized(steps: u32, value: f32, plain_to_normalized: impl FnOnce(f64) -> f64) -> f64 {
    if steps == 0 { plain_to_normalized(value as f64) } else { step_to_normalized(value, steps) }
}

/// The value to give for a parameter with `steps` steps (0 for a number) that is at the normalized value `normalized`: the step, for a list, and the
/// plugin's own conversion to a plain value (`normalized_to_plain`) for any other.
fn normalized_to_value(steps: u32, normalized: f64, normalized_to_plain: impl FnOnce(f64) -> f64) -> f32 {
    if steps == 0 { normalized_to_plain(normalized) as f32 } else { normalized_to_step(normalized, steps) }
}

/// What a step of a list is called: the plugin's own text for it, and the step's number where it says none.
fn step_name(text: Option<String>, step: u32) -> String {
    text.filter(|name| !name.is_empty()).unwrap_or_else(|| step.to_string())
}

/// What the plugin calls each of the `steps + 1` values of a list (`step_name`).
fn step_names(controller: &ComPtr<IEditController>, id: u32, steps: u32) -> Vec<String> {
    (0..=steps)
        .map(|step| {
            let mut text: String128 = [0u16; 128];
            let result = unsafe { controller.getParamStringByValue(id, step_to_normalized(step as f32, steps), &mut text) };
            step_name((result == kResultOk).then(|| string128_to_string(&text)), step)
        })
        .collect()
}

/// What a MIDI mapping points at, from what `assignment(channel, controller)` says of it (the plugin's mapping, for its first bus): the ids of the
/// parameters that a controller on any channel is mapped to, and, for the controllers of channel 0 (which the adapter sends to: every channel's), the
/// parameter of each, by the controller's number.
fn midi_controller_assignments(assignment: impl Fn(i16, i16) -> Option<u32>) -> (HashSet<u32>, Vec<Option<u32>>) {
    let mut targets = HashSet::new();
    let mut by_controller = vec![None; MIDI_CONTROLLERS as usize];
    for channel in 0..MIDI_CHANNELS {
        for cc in 0..MIDI_CONTROLLERS {
            if let Some(param_id) = assignment(channel, cc) {
                targets.insert(param_id);
                if channel == 0 {
                    by_controller[cc as usize] = Some(param_id);
                    log::debug!("VST3 MIDI mapping: CC{cc} → param {param_id}");
                }
            }
        }
    }
    (targets, by_controller)
}

/// The parameters that the plugin's MIDI mapping points at, and the routing of the controllers of channel 0 (`midi_controller_assignments`).
fn midi_controller_parameters(controller: &ComPtr<IEditController>) -> (HashSet<u32>, Vec<Option<u32>>) {
    match controller.cast::<IMidiMapping>() {
        Some(mapping) => midi_controller_assignments(|channel, cc| {
            let mut param_id: u32 = 0;
            let result = unsafe { mapping.getMidiControllerAssignment(0, channel, cc, &mut param_id) };
            (result == kResultOk).then_some(param_id)
        }),
        None => midi_controller_assignments(|_, _| None),
    }
}

pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let (module, class_cid, name, is_instrument) = find_plugin(source)?;

    let host_app = ComWrapper::new(TangHostApp);
    let handler = ComWrapper::new(TangComponentHandler);

    // Get host context as FUnknown pointer
    let host_unknown: ComPtr<FUnknown> = host_app
        .to_com_ptr::<FUnknown>()
        .ok_or_else(|| anyhow::anyhow!("Failed to get FUnknown from host app"))?;

    let factory = module.factory();

    // Create IComponent
    let component: ComPtr<IComponent> = {
        let iid = guid_to_tuid(&<IComponent as Interface>::IID);
        let mut obj: *mut c_void = std::ptr::null_mut();
        let result = unsafe { factory.createInstance(class_cid.as_ptr(), iid.as_ptr(), &mut obj) };
        if result != kResultOk || obj.is_null() {
            anyhow::bail!("Failed to create VST3 component (result={result})");
        }
        unsafe { ComPtr::from_raw(obj as *mut IComponent) }
            .ok_or_else(|| anyhow::anyhow!("createInstance returned null IComponent"))?
    };

    // Initialize component
    let result = unsafe { component.initialize(host_unknown.as_ptr()) };
    if result != kResultOk {
        anyhow::bail!("IComponent::initialize failed (result={result})");
    }

    // Get IAudioProcessor from component
    let processor: ComPtr<IAudioProcessor> = component
        .cast::<IAudioProcessor>()
        .ok_or_else(|| anyhow::anyhow!("Component does not implement IAudioProcessor"))?;

    // Get IEditController — try single-component first, then separate
    let (controller, separate_controller): (ComPtr<IEditController>, bool) =
        if let Some(ec) = component.cast::<IEditController>() {
            log::info!("VST3: single-component design (component implements IEditController)");
            (ec, false)
        } else {
            let mut controller_cid: Steinberg::TUID = [0; 16];
            let result = unsafe { component.getControllerClassId(&mut controller_cid) };
            if result != kResultOk {
                anyhow::bail!("Failed to get controller class ID (result={result})");
            }

            let iid = guid_to_tuid(&<IEditController as Interface>::IID);
            let mut obj: *mut c_void = std::ptr::null_mut();
            let result =
                unsafe { factory.createInstance(controller_cid.as_ptr(), iid.as_ptr(), &mut obj) };
            if result != kResultOk || obj.is_null() {
                anyhow::bail!("Failed to create separate IEditController (result={result})");
            }
            let ec = unsafe { ComPtr::from_raw(obj as *mut IEditController) }
                .ok_or_else(|| anyhow::anyhow!("createInstance returned null controller"))?;

            let result = unsafe { ec.initialize(host_unknown.as_ptr()) };
            if result != kResultOk {
                anyhow::bail!("IEditController::initialize failed (result={result})");
            }

            log::info!("VST3: separate controller design");
            (ec, true)
        };

    // Set component handler
    let handler_ptr = handler
        .to_com_ptr::<IComponentHandler>()
        .ok_or_else(|| anyhow::anyhow!("Failed to get IComponentHandler from handler"))?;
    unsafe {
        controller.setComponentHandler(handler_ptr.as_ptr());
    }

    // Connect component and controller via IConnectionPoint (if separate)
    let (comp_connection, ctrl_connection) = if separate_controller {
        let comp_conn = component.cast::<IConnectionPoint>();
        let ctrl_conn = controller.cast::<IConnectionPoint>();
        if let (Some(cc), Some(tc)) = (&comp_conn, &ctrl_conn) {
            unsafe {
                cc.connect(tc.as_ptr());
                tc.connect(cc.as_ptr());
            }
            log::info!("VST3: connected component and controller via IConnectionPoint");
        }
        (comp_conn, ctrl_conn)
    } else {
        (None, None)
    };

    // Set bus arrangements (stereo)
    let mut input_arr: vst3::Steinberg::Vst::SpeakerArrangement = kStereo;
    let mut output_arr: vst3::Steinberg::Vst::SpeakerArrangement = kStereo;

    // Query bus counts to determine arrangement
    let audio_out_bus_count = unsafe { component.getBusCount(kAudio as i32, kOutput as i32) };
    let audio_in_bus_count = unsafe { component.getBusCount(kAudio as i32, kInput as i32) };

    // Query output bus info
    let audio_out_channel_count = if audio_out_bus_count > 0 {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { component.getBusInfo(kAudio as i32, kOutput as i32, 0, &mut info) };
        if result == kResultOk {
            log::info!(
                "VST3 audio output bus 0: channels={}, name={}",
                info.channelCount,
                string128_to_string(&info.name),
            );
            output_arr = match info.channelCount {
                1 => kMono,
                _ => kStereo,
            };
            info.channelCount as usize
        } else {
            2
        }
    } else {
        2
    };

    // Query input bus info
    let audio_in_channel_count = if audio_in_bus_count > 0 {
        let mut info: BusInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { component.getBusInfo(kAudio as i32, kInput as i32, 0, &mut info) };
        if result == kResultOk {
            log::info!(
                "VST3 audio input bus 0: channels={}, name={}",
                info.channelCount,
                string128_to_string(&info.name),
            );
            input_arr = match info.channelCount {
                1 => kMono,
                _ => kStereo,
            };
            info.channelCount as usize
        } else {
            0
        }
    } else {
        0
    };

    // Set bus arrangements
    let arr_result = if audio_in_bus_count > 0 {
        unsafe {
            processor.setBusArrangements(&mut input_arr, 1, &mut output_arr, 1)
        }
    } else {
        unsafe {
            processor.setBusArrangements(std::ptr::null_mut(), 0, &mut output_arr, 1)
        }
    };
    if arr_result != kResultOk {
        log::warn!("VST3 setBusArrangements returned {arr_result}");
    }

    // Activate buses
    if audio_out_bus_count > 0 {
        let r = unsafe { component.activateBus(kAudio as i32, kOutput as i32, 0, 1) };
        if r != kResultOk {
            log::warn!("VST3 activateBus(audio out) returned {r}");
        }
    }
    if audio_in_bus_count > 0 {
        let r = unsafe { component.activateBus(kAudio as i32, kInput as i32, 0, 1) };
        if r != kResultOk {
            log::warn!("VST3 activateBus(audio in) returned {r}");
        }
    }
    // Activate event input bus (for MIDI)
    let event_in_bus_count = unsafe { component.getBusCount(kEvent as i32, kInput as i32) };
    if event_in_bus_count > 0 {
        let r = unsafe { component.activateBus(kEvent as i32, kInput as i32, 0, 1) };
        if r != kResultOk {
            log::warn!("VST3 activateBus(event in) returned {r}");
        }
    }

    // Query MIDI CC → parameter mapping (which parameters exist only for it are not listed, below)
    let (midi_controller_ids, cc_param_map) = midi_controller_parameters(&controller);

    // Query parameters
    let param_count = unsafe { controller.getParameterCount() };
    let mut params_cache = Vec::with_capacity(param_count as usize);
    let mut param_ids = Vec::with_capacity(param_count as usize);
    let mut param_steps = Vec::with_capacity(param_count as usize);
    let mut preset_param_id: Option<u32> = None;

    for i in 0..param_count {
        let mut info: Vst3ParameterInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { controller.getParameterInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        // Check for program change parameter
        if info.flags & kIsProgramChange != 0 {
            preset_param_id = Some(info.id);
            // Don't expose program change as a regular parameter
            continue;
        }
        if !is_listed(info.flags, midi_controller_ids.contains(&info.id)) {
            continue;
        }

        let name_str = string128_to_string(&info.title);
        let default_normalized = info.defaultNormalizedValue;
        // A list is its steps: 0 to the last, with the names the plugin gives them. Any other parameter has the plain range that the plugin says.
        let list_steps = if is_list(info.stepCount) { info.stepCount as u32 } else { 0 };
        let (min, max, default, labels) = if list_steps > 0 {
            (0.0, list_steps as f32, normalized_to_step(default_normalized, list_steps), Some(step_names(&controller, info.id, list_steps)))
        } else {
            let min = unsafe { controller.normalizedParamToPlain(info.id, 0.0) } as f32;
            let max = unsafe { controller.normalizedParamToPlain(info.id, 1.0) } as f32;
            let default = unsafe { controller.normalizedParamToPlain(info.id, default_normalized) } as f32;
            (min, max, default, None)
        };

        let param_index = params_cache.len() as u32;
        param_ids.push(info.id);
        param_steps.push(list_steps);
        params_cache.push(ParameterInfo {
            index: param_index,
            name: name_str,
            min,
            max,
            default,
            labels,
            step_count: info.stepCount.max(0) as u32,
            ..Default::default()
        });
    }
    log::info!("VST3 plugin has {} parameters", params_cache.len());

    // Discover presets via IUnitInfo
    let mut preset_cache = Vec::new();
    let mut preset_count = 0usize;

    if let Some(unit_info) = controller.cast::<IUnitInfo>() {
        let list_count = unsafe { unit_info.getProgramListCount() };
        for list_idx in 0..list_count {
            let mut list_info: ProgramListInfo = unsafe { std::mem::zeroed() };
            let result = unsafe { unit_info.getProgramListInfo(list_idx, &mut list_info) };
            if result != kResultOk {
                continue;
            }

            let count = list_info.programCount;
            for prog_idx in 0..count {
                let mut name_buf: String128 = [0u16; 128];
                let result =
                    unsafe { unit_info.getProgramName(list_info.id, prog_idx, &mut name_buf) };
                if result == kResultOk {
                    let preset_name = string128_to_string(&name_buf);
                    let id = preset_cache.len().to_string();
                    preset_cache.push(Preset {
                        name: preset_name,
                        id,
                    });
                }
            }
        }
        preset_count = preset_cache.len();
    }
    log::info!("VST3 plugin has {} presets", preset_count);

    log::info!(
        "Loaded VST3 plugin: {name} (instrument={is_instrument}, \
         output_channels={audio_out_channel_count}, params={}, presets={})",
        params_cache.len(),
        preset_count,
    );

    // Allocate with 2x headroom to absorb jitter from audio backends that
    // don't honor the requested block size strictly (e.g. CoreAudio sometimes
    // delivers slightly more frames than `BufferSize::Fixed` requested).
    let buffer_capacity = max_block_size * 2;

    // Setup processing
    let mut setup = ProcessSetup {
        processMode: kRealtime as i32,
        symbolicSampleSize: kSample32 as i32,
        maxSamplesPerBlock: buffer_capacity as i32,
        sampleRate: sample_rate as f64,
    };
    let result = unsafe { processor.setupProcessing(&mut setup) };
    if result != kResultOk {
        log::warn!("VST3 setupProcessing returned {result}");
    }

    // Activate and start processing
    let result = unsafe { component.setActive(1) };
    if result != kResultOk {
        anyhow::bail!("IComponent::setActive(true) failed (result={result})");
    }
    let result = unsafe { processor.setProcessing(1) };
    if result != kResultOk {
        log::warn!("VST3 setProcessing returned {result}");
    }

    // The latency, which a plugin knows once it is set up.
    let latency = unsafe { processor.getLatencySamples() };
    if latency > 0 {
        log::info!("VST3 plugin {name} has a latency of {latency} frames");
    }

    // Pre-allocate buffers sized to capacity — never grow on audio thread.
    let mut output_bufs: Vec<Vec<f32>> = (0..audio_out_channel_count)
        .map(|_| vec![0.0f32; buffer_capacity])
        .collect();
    let mut input_bufs: Vec<Vec<f32>> = (0..audio_in_channel_count)
        .map(|_| vec![0.0f32; buffer_capacity])
        .collect();

    // Pre-build channel pointer arrays. These point into output_bufs/input_bufs
    // which are stable since they live in the heap-allocated Vst3Plugin (Box).
    let output_ptrs: Vec<*mut f32> = output_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
    let input_ptrs: Vec<*mut f32> = input_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();

    // Pre-build AudioBusBuffers arrays sized to the plugin's full bus count.
    // Bus 0 wired to our channel pointers; remaining buses are placeholders.
    let bus_count_out = audio_out_bus_count.max(0) as usize;
    let bus_count_in = audio_in_bus_count.max(0) as usize;
    let output_buses: Vec<AudioBusBuffers> = (0..bus_count_out)
        .map(|i| AudioBusBuffers {
            numChannels: if i == 0 { audio_out_channel_count as i32 } else { 0 },
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: if i == 0 && !output_ptrs.is_empty() {
                    output_ptrs.as_ptr() as *mut *mut f32
                } else {
                    std::ptr::null_mut()
                },
            },
        })
        .collect();
    let input_buses: Vec<AudioBusBuffers> = (0..bus_count_in)
        .map(|i| AudioBusBuffers {
            numChannels: if i == 0 { audio_in_channel_count as i32 } else { 0 },
            silenceFlags: 0,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: if i == 0 && !input_ptrs.is_empty() {
                    input_ptrs.as_ptr() as *mut *mut f32
                } else {
                    std::ptr::null_mut()
                },
            },
        })
        .collect();

    // Pre-allocate process-time COM objects
    let param_changes = ComWrapper::new(TangParameterChanges {
        count: UnsafeCell::new(0),
        queues: (0..MAX_PARAM_QUEUES)
            .map(|_| {
                ComWrapper::new(TangParamValueQueue {
                    param_id: UnsafeCell::new(0),
                    value: UnsafeCell::new(0.0),
                })
            })
            .collect(),
    });
    let output_param_changes = ComWrapper::new(TangParameterChanges {
        count: UnsafeCell::new(0),
        queues: (0..MAX_PARAM_QUEUES)
            .map(|_| {
                ComWrapper::new(TangParamValueQueue {
                    param_id: UnsafeCell::new(0),
                    value: UnsafeCell::new(0.0),
                })
            })
            .collect(),
    });
    let event_list = ComWrapper::new(TangEventList {
        events: UnsafeCell::new(Vec::with_capacity(256)),
    });

    Ok(Box::new(Vst3Plugin {
        name,
        is_instrument,
        sample_rate,
        audio_in_channel_count,
        audio_out_channel_count,
        audio_in_bus_count: audio_in_bus_count.max(0) as usize,
        audio_out_bus_count: audio_out_bus_count.max(0) as usize,
        latency,
        _module: Arc::new(module),
        component,
        processor,
        controller,
        _handler: handler,
        _host_app: host_app,
        separate_controller,
        params_cache,
        param_ids,
        param_steps,
        pending_param_changes: Vec::new(),
        preset_cache,
        preset_param_id,
        preset_count,
        output_bufs,
        input_bufs,
        output_ptrs,
        input_ptrs,
        output_buses,
        input_buses,
        param_changes,
        output_param_changes,
        event_list,
        cc_param_map,
        next_note_id: 0,
        channel_notes: vec![Vec::new(); 16],
        comp_connection,
        ctrl_connection,
    }))
}

/// Find a VST3 plugin by name or bundle path.
/// Returns (module, class_cid, name, is_instrument).
fn find_plugin(source: &str) -> anyhow::Result<(Vst3Module, Steinberg::TUID, String, bool)> {
    // Try stripping "vst3:" prefix for name-based lookup
    if let Some(plugin_name) = source.strip_prefix("vst3:") {
        let search_name = plugin_name.to_lowercase();

        // Collect all bundles, then split into (likely matches by filename stem)
        // and the rest. dlopening a VST3 bundle can be expensive (Pianoteq pulls
        // in Qt + Komplete Kontrol; iLok plugins talk to license daemons), so we
        // try filename matches first and only fall back to the full scan if none
        // of those bundles export a matching class name.
        let mut likely: Vec<PathBuf> = Vec::new();
        let mut rest: Vec<PathBuf> = Vec::new();
        for search_dir in vst3_search_paths() {
            if !search_dir.exists() {
                continue;
            }
            for bundle_path in find_vst3_bundles(&search_dir) {
                let stem = bundle_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_lowercase());
                if stem.is_some_and(|s| s.contains(&search_name)) {
                    likely.push(bundle_path);
                } else {
                    rest.push(bundle_path);
                }
            }
        }

        for bundle_path in likely.into_iter().chain(rest) {
            if let Ok((module, cid, name, is_instrument)) =
                scan_bundle_for_name(&bundle_path, &search_name)
            {
                return Ok((module, cid, name, is_instrument));
            }
        }
        anyhow::bail!(
            "VST3 plugin not found: {plugin_name}\n\
             Run `tang enumerate plugins` to list available plugins."
        );
    }

    // Direct path to a .vst3 bundle
    let path = Path::new(source);
    let module = Vst3Module::load(path)?;
    let factory = module.factory();

    let count = unsafe { factory.countClasses() };
    if count == 0 {
        anyhow::bail!("No classes in VST3 bundle: {}", path.display());
    }

    // Pick the first Audio Module Class
    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        let is_instrument = is_class_instrument(factory, i);

        return Ok((module, info.cid, name, is_instrument));
    }

    anyhow::bail!(
        "No Audio Module Class found in VST3 bundle: {}",
        path.display()
    );
}

fn scan_bundle_for_name(
    bundle_path: &Path,
    search_name: &str,
) -> anyhow::Result<(Vst3Module, Steinberg::TUID, String, bool)> {
    let module = Vst3Module::load(bundle_path)?;
    let factory = module.factory();
    let count = unsafe { factory.countClasses() };

    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        if name.to_lowercase().contains(search_name) {
            let is_instrument = is_class_instrument(factory, i);
            return Ok((module, info.cid, name, is_instrument));
        }
    }

    anyhow::bail!("No matching class in {}", bundle_path.display());
}

/// Check if a class is an instrument by examining subCategories from IPluginFactory2.
fn is_class_instrument(factory: &ComPtr<IPluginFactory>, index: Steinberg::int32) -> bool {
    if let Some(f2) = factory.cast::<IPluginFactory2>() {
        let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
        let result = unsafe { f2.getClassInfo2(index, &mut info2) };
        if result == kResultOk {
            let subcats = char_array_to_string(&info2.subCategories);
            return subcats.contains("Instrument");
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Enumeration
// ---------------------------------------------------------------------------

pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let mut plugins = Vec::new();

    for search_dir in vst3_search_paths() {
        if !search_dir.exists() {
            continue;
        }
        for bundle_path in find_vst3_bundles(&search_dir) {
            let start = std::time::Instant::now();
            match scan_bundle_for_enum(&bundle_path) {
                Some(found) => {
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    // Distribute total scan time evenly across classes in this bundle.
                    // Most bundles have a single class, so this is usually exact.
                    let n = found.len().max(1) as u64;
                    let per_class = elapsed_ms / n;
                    for mut p in found {
                        p.scan_ms = per_class;
                        plugins.push(p);
                    }
                }
                None => {
                    log::warn!("Failed to scan VST3 bundle: {}", bundle_path.display());
                }
            }
        }
    }

    plugins
}

fn scan_bundle_for_enum(bundle_path: &Path) -> Option<Vec<PluginInfo>> {
    let module = Vst3Module::load(bundle_path).ok()?;
    let factory = module.factory();
    let count = unsafe { factory.countClasses() };

    let mut found = Vec::new();
    for i in 0..count {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        let result = unsafe { factory.getClassInfo(i, &mut info) };
        if result != kResultOk {
            continue;
        }

        let category = char_array_to_string(&info.category);
        if category != "Audio Module Class" {
            continue;
        }

        let name = char_array_to_string(&info.name);
        let is_instrument = is_class_instrument(factory, i);

        // Briefly instantiate to query param count and preset count
        let (param_count, preset_count) =
            brief_instantiate(factory, &info.cid, &module).unwrap_or((0, 0));

        found.push(PluginInfo {
            name: name.clone(),
            id: format!("vst3:{name}"),
            is_instrument,
            param_count,
            preset_count,
            path: bundle_path.to_string_lossy().to_string(),
            scan_ms: 0,
        });
    }

    Some(found)
}

/// Briefly instantiate a VST3 plugin to query parameter and preset counts.
fn brief_instantiate(
    factory: &ComPtr<IPluginFactory>,
    class_cid: &Steinberg::TUID,
    _module: &Vst3Module,
) -> Option<(usize, usize)> {
    let host_app = ComWrapper::new(TangHostApp);
    let host_unknown: ComPtr<FUnknown> = host_app.to_com_ptr::<FUnknown>()?;

    let iid = guid_to_tuid(&<IComponent as Interface>::IID);
    let mut obj: *mut c_void = std::ptr::null_mut();
    let result = unsafe { factory.createInstance(class_cid.as_ptr(), iid.as_ptr(), &mut obj) };
    if result != kResultOk || obj.is_null() {
        return None;
    }
    let component = unsafe { ComPtr::from_raw(obj as *mut IComponent) }?;
    let init_result = unsafe { component.initialize(host_unknown.as_ptr()) };
    if init_result != kResultOk {
        return None;
    }

    // Get controller
    let (controller, separate) = if let Some(ec) = component.cast::<IEditController>() {
        (ec, false)
    } else {
        let mut ctrl_cid: Steinberg::TUID = [0; 16];
        unsafe { component.getControllerClassId(&mut ctrl_cid) };
        let ctrl_iid = guid_to_tuid(&<IEditController as Interface>::IID);
        let mut ctrl_obj: *mut c_void = std::ptr::null_mut();
        let r =
            unsafe { factory.createInstance(ctrl_cid.as_ptr(), ctrl_iid.as_ptr(), &mut ctrl_obj) };
        if r != kResultOk || ctrl_obj.is_null() {
            unsafe { component.terminate() };
            return None;
        }
        let ec = unsafe { ComPtr::from_raw(ctrl_obj as *mut IEditController) }?;
        unsafe { ec.initialize(host_unknown.as_ptr()) };
        (ec, true)
    };

    // Connect component and controller for separate-controller plugins
    let (comp_conn, ctrl_conn) = if separate {
        let cc = component.cast::<IConnectionPoint>();
        let tc = controller.cast::<IConnectionPoint>();
        if let (Some(cc), Some(tc)) = (&cc, &tc) {
            unsafe {
                cc.connect(tc.as_ptr());
                tc.connect(cc.as_ptr());
            }
        }
        (cc, tc)
    } else {
        (None, None)
    };

    // Count the parameters a person is shown, as a load lists them (not the program change, nor those that are hidden, read only or only for MIDI)
    let (midi_controller_ids, _) = midi_controller_parameters(&controller);
    let param_count = (0..unsafe { controller.getParameterCount() })
        .filter(|&i| {
            let mut info: Vst3ParameterInfo = unsafe { std::mem::zeroed() };
            let result = unsafe { controller.getParameterInfo(i, &mut info) };
            result == kResultOk && info.flags & kIsProgramChange == 0 && is_listed(info.flags, midi_controller_ids.contains(&info.id))
        })
        .count();

    // Count presets via IUnitInfo
    let preset_count = if let Some(unit_info) = controller.cast::<IUnitInfo>() {
        let list_count = unsafe { unit_info.getProgramListCount() };
        let mut total = 0usize;
        for list_idx in 0..list_count {
            let mut list_info: ProgramListInfo = unsafe { std::mem::zeroed() };
            let r = unsafe { unit_info.getProgramListInfo(list_idx, &mut list_info) };
            if r == kResultOk {
                total += list_info.programCount as usize;
            }
        }
        total
    } else {
        0
    };

    // Clean up — disconnect before terminate, drop connection points before controller
    if let (Some(cc), Some(tc)) = (&comp_conn, &ctrl_conn) {
        unsafe {
            cc.disconnect(tc.as_ptr());
            tc.disconnect(cc.as_ptr());
        }
    }
    drop(comp_conn);
    drop(ctrl_conn);
    if separate {
        unsafe { controller.terminate() };
    }
    drop(controller);
    unsafe { component.terminate() };

    Some((param_count, preset_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsBypass, kIsHidden, kIsList, kIsReadOnly, kNoFlags};

    /// A person is shown the parameters that the plugin does not hide or say are read only, and none that is only there so that a host
    /// can send MIDI controllers through the parameters (one that cannot be automated and is the target of the plugin's MIDI mapping).
    #[test]
    fn a_parameter_is_shown_unless_the_plugin_hides_it_or_it_is_only_there_for_midi_controllers() {
        // (flags, whether the MIDI mapping points at it, whether it is shown)
        for (flags, midi_controller, shown) in [
            (kNoFlags, false, true),
            (kCanAutomate, false, true),
            (kCanAutomate | kIsBypass, false, true),
            (kCanAutomate | kIsList, false, true),
            (kIsHidden, false, false),
            (kCanAutomate | kIsHidden, false, false),
            (kIsReadOnly, false, false),
            (kCanAutomate | kIsReadOnly, false, false),
            (kNoFlags, true, false),
            (kIsList, true, false),
            // A real parameter that a controller is mapped to (a mod wheel to a depth) can be automated, and is shown.
            (kCanAutomate, true, true),
            (kCanAutomate | kIsList, true, true),
            // What the plugin hides stays hidden whatever else is said of it.
            (kCanAutomate | kIsHidden, true, false),
        ] {
            assert_eq!(is_listed(flags, midi_controller), shown, "flags {flags:#x}, mapped to a MIDI controller: {midi_controller}");
        }
    }

    #[test]
    fn a_parameter_with_steps_is_a_list_of_names_unless_it_has_very_many() {
        for (steps, list) in [(-1, false), (0, false), (1, true), (2, true), (18, true), (64, true), (65, false), (127, false), (1000, false)] {
            assert_eq!(is_list(steps), list, "{steps} steps");
        }
    }

    /// A parameter with n steps has the normalized values k/n, and the page and the terminal give the step k: so that a switch is 0 or 1 and a list
    /// of 12 root notes is 0 to 11, whatever the plain range that the plugin says (Pianoteq's is 0 to 1 for all of them).
    #[test]
    fn a_step_is_a_place_among_the_normalized_values_of_the_parameter_and_back() {
        for (step, steps, normalized) in [(0.0, 1, 0.0), (1.0, 1, 1.0), (0.0, 29, 0.0), (10.0, 29, 10.0 / 29.0), (29.0, 29, 1.0), (11.0, 11, 1.0), (3.0, 4, 0.75)] {
            assert!((step_to_normalized(step, steps) - normalized).abs() < 1e-12, "step {step} of {steps}");
            assert_eq!(normalized_to_step(normalized, steps), step, "normalized {normalized} of {steps}");
        }
        // A step that is not one is the nearest, and one beyond the ends is the end.
        assert_eq!(normalized_to_step(0.49, 4), 2.0);
        assert_eq!(normalized_to_step(0.51, 4), 2.0);
        assert_eq!(normalized_to_step(0.9, 4), 4.0);
        assert_eq!(step_to_normalized(7.0, 3), 1.0);
        assert_eq!(step_to_normalized(-2.0, 3), 0.0);
    }

    /// The value that is given for a parameter is its step for a list and its plain value for any other (`plain_to_normalized` and `normalized_to_plain` are
    /// the plugin's own conversions, which a list does not use: here they are made up, ten times the normalized value, to tell the two apart).
    #[test]
    fn a_list_is_given_and_taken_by_its_step_and_any_other_parameter_by_the_plugins_own_plain_value() {
        let (to_plain, to_normalized) = (|normalized: f64| normalized * 10.0, |plain: f64| plain / 10.0);
        // A number: 5 is the plain value 5, which is the normalized 0.5.
        assert_eq!(value_to_normalized(0, 5.0, to_normalized), 0.5);
        assert_eq!(normalized_to_value(0, 0.5, to_plain), 5.0);
        // A list of 4 steps: 3 is the step 3, which is the normalized 0.75, and the plugin's plain values are not asked.
        assert_eq!(value_to_normalized(4, 3.0, to_normalized), 0.75);
        assert_eq!(normalized_to_value(4, 0.75, to_plain), 3.0);
        // The first step of a list of 29, which is the default of a parameter of Pianoteq's, is 1 and not 1/29 of its plain range.
        assert_eq!(normalized_to_value(29, 1.0 / 29.0, to_plain), 1.0);
    }

    #[test]
    fn a_step_that_the_plugin_gives_no_name_is_called_by_its_number() {
        assert_eq!(step_name(Some("On".to_string()), 1), "On");
        assert_eq!(step_name(Some(String::new()), 3), "3");
        assert_eq!(step_name(None, 7), "7");
    }

    #[test]
    fn the_parameters_a_midi_mapping_points_at_are_those_of_every_channel_and_the_routing_is_that_of_channel_0() {
        // A plugin with a parameter for each channel and controller (the way the placeholders are made): the id says which.
        let (targets, by_controller) = midi_controller_assignments(|channel, cc| Some(1000 + 130 * channel as u32 + cc as u32));
        assert_eq!(targets.len(), 16 * 130);
        assert!(targets.contains(&1000) && targets.contains(&(1000 + 16 * 130 - 1)));
        assert_eq!(by_controller.len(), 130);
        for (cc, param) in by_controller.iter().enumerate() {
            assert_eq!(*param, Some(1000 + cc as u32), "controller {cc} goes to the parameter of channel 0");
        }
        // A plugin that maps the mod wheel, on every channel, to one parameter: that is the only target and the only route.
        let (targets, by_controller) = midi_controller_assignments(|_, cc| (cc == 1).then_some(7));
        assert_eq!(targets, HashSet::from([7]));
        assert_eq!(by_controller.iter().filter(|param| param.is_some()).count(), 1);
        assert_eq!(by_controller[1], Some(7));
        // A mapping that only the channels after the first have: targets, and no route.
        let (targets, by_controller) = midi_controller_assignments(|channel, cc| (channel > 0 && cc == 74).then_some(9));
        assert_eq!(targets, HashSet::from([9]));
        assert!(by_controller.iter().all(Option::is_none));
        // No mapping at all.
        let (targets, by_controller) = midi_controller_assignments(|_, _| None);
        assert!(targets.is_empty() && by_controller.iter().all(Option::is_none));
    }

    /// Pianoteq 9 has 2,294 parameters besides its program change, and 2,080 of them are placeholders for MIDI controllers ("MIDI CC 3|47":
    /// 16 channels of 130) that bury the 214 that are its own. Needs it installed: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs Pianoteq 9 installed"]
    fn pianoteq_lists_its_own_parameters_and_none_of_the_placeholders_for_midi_controllers() {
        // (What the plugin list says of it, which is counted by another function than the load.)
        let scanned = enumerate_plugins().into_iter().find(|info| info.name == "Pianoteq 9").expect("Pianoteq 9 is installed");
        assert_eq!(scanned.param_count, 214);
        let mut plugin = crate::plugin::load("vst3:Pianoteq 9", 48000.0, 512, &crate::plugin::Runtime::default()).expect("Pianoteq 9 is installed");
        let listed = plugin.parameters();
        assert!(listed.iter().all(|p| !p.name.starts_with("MIDI CC ")), "no placeholder is listed");
        assert_eq!(listed.len(), 214);
        // What its own parameters are, by name, and that an index reaches the parameter it says (they are places in the list, which a hidden one leaves out).
        let volume = listed.iter().find(|p| p.name == "Volume").expect("a volume");
        assert!(listed.iter().any(|p| p.name == "Dynamics"));
        assert!(listed.iter().enumerate().all(|(place, p)| p.index as usize == place), "the indices are the places in the list");
        // A switch has the names that the plugin gives its two ends, and is a list of two steps (0 and 1); a parameter with no steps is a number.
        let switch = listed.iter().find(|p| p.name == "Equalizer Switch").expect("a switch");
        assert_eq!((switch.step_count, switch.labels.clone(), switch.min, switch.max), (1, Some(vec!["Off".to_string(), "On".to_string()]), 0.0, 1.0));
        assert_eq!((volume.step_count, volume.labels.clone()), (0, None));
        // A list of 30 (the speeds of a bounce, "16d" to "1/32t"), whose default is the 14th, "1" (step 13): read as the step that it is, and not as 13/29 of a plain range.
        let speed = listed.iter().find(|p| p.name == "Bounce Sync Speed").expect("a list of speeds");
        assert_eq!((speed.step_count, speed.min, speed.max, speed.default), (29, 0.0, 29.0, 13.0));
        assert_eq!(speed.labels.as_ref().map(|labels| (labels.len(), labels[0].as_str(), labels[13].as_str(), labels[29].as_str())), Some((30, "16d", "1", "1/32t")));
        assert_eq!(plugin.get_parameter(speed.index), Some(13.0));
        // Set to the step 5 ("8t"), which the plugin takes with the next blocks and says back as 5: the step goes through as the step it is.
        plugin.set_parameter(speed.index, 5.0).unwrap();
        let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
        for _ in 0..4 {
            let mut out: [&mut [f32]; 2] = [&mut left, &mut right];
            plugin.process(&[], &[], &mut out).unwrap();
        }
        assert_eq!(plugin.get_parameter(speed.index), Some(5.0));
        for name in ["Volume", "Dynamics"] {
            let info = listed.iter().find(|p| p.name == name).expect("a parameter of its own");
            // (A new instance is at its defaults: the index reads the parameter that its name says, and not one that was before it in the plugin's own list.)
            let value = plugin.get_parameter(info.index).expect("a parameter that can be read");
            assert!((value - info.default).abs() < 1e-4, "{name}: read {value}, its default is {}", info.default);
        }
        assert!(volume.default != listed.iter().find(|p| p.name == "Dynamics").map_or(volume.default, |p| p.default), "the two have defaults of their own");
    }
}
