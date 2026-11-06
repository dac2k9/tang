use std::ffi::CStr;
use std::path::Path;

use clack_extensions::latency::PluginLatency;
use clack_extensions::audio_ports::{
    AudioPortInfoBuffer, HostAudioPorts, HostAudioPortsImpl, PluginAudioPorts, RescanType,
};
use clack_extensions::params::{
    HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamInfoBuffer,
    ParamRescanFlags, PluginParams,
};
use clack_extensions::preset_discovery::HostPresetLoadImpl;
use clack_extensions::preset_discovery::prelude::{
    FileType, Flags, HostPresetLoad, IndexerImpl, Location, LocationInfo, MetadataReceiverImpl,
    PluginPresetLoad, PresetDiscoveryFactory, Provider, Soundpack, Timestamp, UniversalPluginId,
};
use clack_host::events::event_types::ParamValueEvent;
use clack_host::prelude::*;
use clack_host::process::StartedPluginAudioProcessor;
use clack_host::utils::Cookie;

use super::{ParameterInfo, Plugin, PluginInfo, Preset};

// ---------------------------------------------------------------------------
// Host handler types (minimal, no-op callbacks)
// ---------------------------------------------------------------------------

struct TangHost;
struct TangHostShared;
struct TangHostMainThread;

impl HostHandlers for TangHost {
    type Shared<'a> = TangHostShared;
    type MainThread<'a> = TangHostMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostAudioPorts>();
        builder.register::<HostParams>();
        builder.register::<HostPresetLoad>();
    }
}

impl<'a> SharedHandler<'a> for TangHostShared {
    fn request_restart(&self) {
        log::debug!("CLAP plugin requested restart (ignored)");
    }
    fn request_process(&self) {
        log::debug!("CLAP plugin requested process (ignored)");
    }
    fn request_callback(&self) {
        log::debug!("CLAP plugin requested callback (ignored)");
    }
}

impl<'a> MainThreadHandler<'a> for TangHostMainThread {}

impl HostParamsImplShared for TangHostShared {
    fn request_flush(&self) {
        log::debug!("CLAP params: request_flush (ignored)");
    }
}

impl HostParamsImplMainThread for TangHostMainThread {
    fn rescan(&mut self, _flags: ParamRescanFlags) {
        log::debug!("CLAP params: rescan (ignored)");
    }
    fn clear(&mut self, _param_id: ClapId, _flags: ParamClearFlags) {
        log::debug!("CLAP params: clear (ignored)");
    }
}

impl HostPresetLoadImpl for TangHostMainThread {
    fn on_error(
        &mut self,
        _location: Location,
        _load_key: Option<&CStr>,
        os_error: i32,
        message: Option<&CStr>,
    ) {
        log::warn!("CLAP preset load error: os_error={os_error}, message={message:?}");
    }
    fn loaded(&mut self, _location: Location, _load_key: Option<&CStr>) {
        log::info!("CLAP preset loaded successfully");
    }
}

impl HostAudioPortsImpl for TangHostMainThread {
    fn is_rescan_flag_supported(&self, _flag: RescanType) -> bool {
        false
    }
    fn rescan(&mut self, _flag: RescanType) {
        log::debug!("CLAP audio_ports: rescan (ignored)");
    }
}

// ---------------------------------------------------------------------------
// Preset discovery helpers
// ---------------------------------------------------------------------------

struct TangIndexer {
    locations: Vec<TangLocation>,
    file_extensions: Vec<String>,
}

#[derive(Clone)]
struct TangLocation {
    is_plugin: bool,
    file_path: Option<String>,
}

impl IndexerImpl for TangIndexer {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        if let Some(ext) = file_type.file_extension.and_then(|s| s.to_str().ok()) {
            log::debug!("CLAP preset discovery: declared filetype extension={ext}");
            self.file_extensions.push(ext.to_string());
        }
        Ok(())
    }
    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        match location.location {
            Location::Plugin => {
                self.locations.push(TangLocation {
                    is_plugin: true,
                    file_path: None,
                });
            }
            Location::File { path } => {
                self.locations.push(TangLocation {
                    is_plugin: false,
                    file_path: path.to_str().ok().map(String::from),
                });
            }
        }
        Ok(())
    }
    fn declare_soundpack(&mut self, _soundpack: Soundpack) -> Result<(), HostError> {
        Ok(())
    }
}

struct TangMetadataReceiver {
    presets: Vec<(String, Option<String>)>,
}

impl MetadataReceiverImpl for TangMetadataReceiver {
    fn on_error(&mut self, error_code: i32, error_message: Option<&CStr>) {
        log::debug!("CLAP preset discovery error {error_code}: {error_message:?}");
    }

    fn begin_preset(
        &mut self,
        name: Option<&CStr>,
        load_key: Option<&CStr>,
    ) -> Result<(), HostError> {
        let name_str = name
            .and_then(|s| s.to_str().ok())
            .unwrap_or("Unknown")
            .to_string();
        let load_key_str = load_key.and_then(|s| s.to_str().ok()).map(String::from);
        self.presets.push((name_str, load_key_str));
        Ok(())
    }

    fn add_plugin_id(&mut self, _plugin_id: UniversalPluginId) {}
    fn set_soundpack_id(&mut self, _soundpack_id: &CStr) {}
    fn set_flags(&mut self, _flags: Flags) {}
    fn add_creator(&mut self, _creator: &CStr) {}
    fn set_description(&mut self, _description: &CStr) {}
    fn set_timestamps(
        &mut self,
        _creation_time: Option<Timestamp>,
        _modification_time: Option<Timestamp>,
    ) {
    }
    fn add_feature(&mut self, _feature: &CStr) {}
    fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

struct ClapPresetData {
    is_plugin_location: bool,
    file_path: Option<String>,
    load_key: Option<String>,
}

/// Recursively collect files matching the given extensions from a directory.
fn collect_preset_files(dir: &str, extensions: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    let path = Path::new(dir);
    if !path.is_dir() {
        // Single file, not a directory
        files.push(dir.to_string());
        return files;
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if extensions.is_empty()
                || entry_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| extensions.iter().any(|fe| fe == ext))
            {
                if let Some(s) = entry_path.to_str() {
                    files.push(s.to_string());
                }
            }
        }
    }
    files.sort();
    files
}

fn discover_presets(bundle: &PluginBundle, host_info: &HostInfo) -> Vec<(Preset, ClapPresetData)> {
    let factory: PresetDiscoveryFactory = match bundle.get_factory() {
        Some(f) => f,
        None => {
            log::debug!("CLAP bundle has no preset discovery factory");
            return Vec::new();
        }
    };
    log::info!(
        "CLAP preset discovery: {} providers",
        factory.provider_count()
    );

    let mut result = Vec::new();

    for descriptor in factory.provider_descriptors() {
        let provider_id = match descriptor.id() {
            Some(id) => id,
            None => continue,
        };

        let mut provider = match Provider::instantiate(
            TangIndexer {
                locations: Vec::new(),
                file_extensions: Vec::new(),
            },
            bundle,
            provider_id,
            host_info,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to instantiate preset provider: {e}");
                continue;
            }
        };

        let locations: Vec<TangLocation> = provider.indexer().locations.clone();
        let extensions: Vec<String> = provider.indexer().file_extensions.clone();
        log::info!(
            "CLAP preset discovery: provider declared {} locations, {} filetypes",
            locations.len(),
            extensions.len()
        );

        for location in &locations {
            log::debug!(
                "CLAP preset discovery: scanning location is_plugin={} path={:?}",
                location.is_plugin,
                location.file_path
            );

            if location.is_plugin {
                let mut receiver = TangMetadataReceiver {
                    presets: Vec::new(),
                };
                provider.get_metadata(Location::Plugin, &mut receiver);
                for (name, load_key) in receiver.presets {
                    let idx = result.len();
                    result.push((
                        Preset {
                            name,
                            id: idx.to_string(),
                        },
                        ClapPresetData {
                            is_plugin_location: true,
                            file_path: None,
                            load_key,
                        },
                    ));
                }
            } else if let Some(ref dir_path) = location.file_path {
                // Walk the directory for matching files
                let files = collect_preset_files(dir_path, &extensions);
                for file_path in &files {
                    let c_path = match std::ffi::CString::new(file_path.as_str()) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    let mut receiver = TangMetadataReceiver {
                        presets: Vec::new(),
                    };
                    provider.get_metadata(Location::File { path: &c_path }, &mut receiver);
                    for (name, load_key) in receiver.presets {
                        let idx = result.len();
                        result.push((
                            Preset {
                                name,
                                id: idx.to_string(),
                            },
                            ClapPresetData {
                                is_plugin_location: false,
                                file_path: Some(file_path.clone()),
                                load_key,
                            },
                        ));
                    }
                }
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// ClapPlugin
// ---------------------------------------------------------------------------

pub struct ClapPlugin {
    name: String,
    is_instrument: bool,
    sample_rate: f32,
    audio_in_channel_count: usize,
    audio_out_channel_count: usize,
    /// How many frames late what the plugin makes is, as it said when it was activated (`Plugin::latency`). A plugin that changes it while it runs, and tells the host by
    /// `clap_host_latency.changed`, is not followed: the plugin is on the audio thread by then, and the extension is the main thread's.
    latency: u32,
    params_ext: Option<PluginParams>,
    params_cache: Vec<ParameterInfo>,
    param_ids: Vec<ClapId>,
    pending_param_changes: Vec<(ClapId, f64)>,
    preset_cache: Vec<Preset>,
    preset_data: Vec<ClapPresetData>,
    preset_load_ext: Option<PluginPresetLoad>,
    _bundle: PluginBundle,
    instance: PluginInstance<TangHost>,
    audio_processor: Option<StartedPluginAudioProcessor<TangHost>>,
    // Pre-allocated buffers
    output_ports: AudioPorts,
    output_port_channel_counts: Vec<u32>,
    output_channel_bufs: Vec<Vec<f32>>,
    input_ports: AudioPorts,
    input_port_channel_counts: Vec<u32>,
    input_channel_bufs: Vec<Vec<f32>>,
    event_buffer: EventBuffer,
}

// Safety: PluginInstance is !Send because CLAP enforces main-thread affinity for
// certain operations (e.g. init, activate, deactivate, destroy). We satisfy this:
// the plugin is created, activated, and preset-loaded on the main thread, then
// moved (by value) into the PluginChain which is moved into the audio callback
// closure — single owner, no concurrent access. On the audio thread only
// process() is called, via the StartedPluginAudioProcessor handle. On shutdown
// the plugin is sent back to the main thread for deactivation and drop.
unsafe impl Send for ClapPlugin {}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        if let Some(processor) = self.audio_processor.take() {
            let stopped = processor.stop_processing();
            self.instance.deactivate(stopped);
        }
    }
}

// ---------------------------------------------------------------------------
// Enumeration (unchanged)
// ---------------------------------------------------------------------------

/// Enumerate all CLAP plugins found on the system.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    let mut plugins = Vec::new();

    let bundle_paths: Vec<_> = clack_finder::ClapFinder::from_standard_paths()
        .into_iter()
        .chain(extra_clap_bundles())
        .collect();

    for bundle_path in bundle_paths {
        let start = std::time::Instant::now();
        match scan_bundle(&bundle_path) {
            Some(found) => {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                let n = found.len().max(1) as u64;
                let per_plugin = elapsed_ms / n;
                for mut p in found {
                    p.scan_ms = per_plugin;
                    plugins.push(p);
                }
            }
            None => {
                log::warn!("Failed to scan CLAP bundle: {}", bundle_path.display());
            }
        }
    }

    plugins
}

/// Resolve extra CLAP paths from config. Paths pointing to `.clap` files are
/// returned directly (bypassing ClapFinder, which rejects flat files on macOS).
/// Paths pointing to directories are scanned via ClapFinder.
fn extra_clap_bundles() -> Vec<std::path::PathBuf> {
    let extra = crate::config::extra_clap_paths();
    if extra.is_empty() {
        return Vec::new();
    }

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for p in extra {
        if p.is_file() {
            files.push(p.clone());
        } else if p.is_dir() {
            dirs.push(p.clone());
        }
    }

    if !dirs.is_empty() {
        for bundle_path in clack_finder::ClapFinder::new(dirs) {
            files.push(bundle_path);
        }
    }

    files
}

fn scan_bundle(path: &Path) -> Option<Vec<PluginInfo>> {
    use clack_host::plugin::features::INSTRUMENT;

    // Safety: loading external dynamic libraries is inherently unsafe
    let bundle = unsafe { PluginBundle::load(path) }.ok()?;
    let factory = bundle.get_plugin_factory()?;

    let host_info =
        HostInfo::new("tang", "akerud", "https://github.com/akerud/tang", "0.1.0").ok()?;

    let mut found = Vec::new();
    for descriptor in factory.plugin_descriptors() {
        let id = descriptor.id()?.to_str().ok()?.to_string();
        let name = descriptor
            .name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);

        // Briefly instantiate to query param count
        let plugin_id = std::ffi::CString::new(id.as_str()).ok()?;
        let param_count = PluginInstance::<TangHost>::new(
            |_| TangHostShared,
            |_| TangHostMainThread,
            &bundle,
            &plugin_id,
            &host_info,
        )
        .ok()
        .and_then(|mut inst| {
            let ext: PluginParams = inst.plugin_shared_handle().get_extension()?;
            let mut handle = inst.plugin_handle();
            Some(ext.count(&mut handle) as usize)
        })
        .unwrap_or(0);

        let preset_count = discover_presets(&bundle, &host_info).len();

        found.push(PluginInfo {
            name,
            id,
            is_instrument,
            param_count,
            preset_count,
            path: path.to_string_lossy().to_string(),
            scan_ms: 0,
        });
    }

    Some(found)
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    let host_info = HostInfo::new("tang", "akerud", "https://github.com/akerud/tang", "0.1.0")?;

    // Resolve plugin ID and bundle
    let (bundle, plugin_id_string, name, is_instrument) = find_plugin(source)?;

    let plugin_id =
        std::ffi::CString::new(plugin_id_string.as_str()).expect("plugin ID contains NUL");

    // Instantiate
    let mut instance = PluginInstance::<TangHost>::new(
        |_| TangHostShared,
        |_shared| TangHostMainThread,
        &bundle,
        &plugin_id,
        &host_info,
    )
    .map_err(|e| anyhow::anyhow!("Failed to instantiate CLAP plugin: {e}"))?;

    // Query audio output ports
    let (audio_out_channel_count, output_port_channel_counts) = {
        let audio_ports_ext: Option<PluginAudioPorts> =
            instance.plugin_shared_handle().get_extension();
        match audio_ports_ext {
            Some(ext) => {
                let mut handle = instance.plugin_handle();
                let mut buf = AudioPortInfoBuffer::new();
                let count = ext.count(&mut handle, false);
                let mut total_channels = 0u32;
                let mut port_channels = Vec::new();
                for i in 0..count {
                    if let Some(info) = ext.get(&mut handle, i, false, &mut buf) {
                        log::info!(
                            "CLAP audio output port {i}: channels={}, name={}",
                            info.channel_count,
                            String::from_utf8_lossy(info.name),
                        );
                        total_channels += info.channel_count;
                        port_channels.push(info.channel_count);
                    }
                }
                if total_channels == 0 {
                    // Fallback: assume stereo
                    log::warn!("CLAP plugin reports 0 output channels, assuming stereo");
                    (2usize, vec![2])
                } else {
                    (total_channels as usize, port_channels)
                }
            }
            None => {
                // No audio-ports extension — assume stereo
                log::warn!("CLAP plugin does not support audio-ports extension, assuming stereo");
                (2usize, vec![2])
            }
        }
    };

    // Query audio input ports
    let (audio_in_channel_count, input_port_channel_counts) = {
        let audio_ports_ext: Option<PluginAudioPorts> =
            instance.plugin_shared_handle().get_extension();
        match audio_ports_ext {
            Some(ext) => {
                let mut handle = instance.plugin_handle();
                let mut buf = AudioPortInfoBuffer::new();
                let count = ext.count(&mut handle, true);
                let mut total_channels = 0u32;
                let mut port_channels = Vec::new();
                for i in 0..count {
                    if let Some(info) = ext.get(&mut handle, i, true, &mut buf) {
                        log::info!(
                            "CLAP audio input port {i}: channels={}, name={}",
                            info.channel_count,
                            String::from_utf8_lossy(info.name),
                        );
                        total_channels += info.channel_count;
                        port_channels.push(info.channel_count);
                    }
                }
                (total_channels as usize, port_channels)
            }
            None => (0usize, Vec::new()),
        }
    };

    // Query parameters
    let params_ext: Option<PluginParams> = instance.plugin_shared_handle().get_extension();
    let (params_cache, param_ids) = match params_ext {
        Some(ext) => {
            let mut handle = instance.plugin_handle();
            let mut info_buf = ParamInfoBuffer::new();
            let count = ext.count(&mut handle);
            let mut params = Vec::with_capacity(count as usize);
            let mut ids = Vec::with_capacity(count as usize);
            for i in 0..count {
                if let Some(info) = ext.get_info(&mut handle, i, &mut info_buf) {
                    ids.push(info.id);
                    params.push(ParameterInfo {
                        index: i,
                        name: String::from_utf8_lossy(info.name).to_string(),
                        min: info.min_value as f32,
                        max: info.max_value as f32,
                        default: info.default_value as f32,
                        ..Default::default()
                    });
                }
            }
            log::info!("CLAP plugin has {} parameters", params.len());
            (params, ids)
        }
        None => {
            log::info!("CLAP plugin does not support params extension");
            (Vec::new(), Vec::new())
        }
    };

    // Discover presets
    let (preset_cache, preset_data): (Vec<Preset>, Vec<ClapPresetData>) =
        discover_presets(&bundle, &host_info).into_iter().unzip();

    // Query preset load extension
    let preset_load_ext: Option<PluginPresetLoad> = instance.plugin_shared_handle().get_extension();

    log::info!(
        "Loaded CLAP plugin: {name} (instrument={is_instrument}, output_channels={audio_out_channel_count}, params={}, presets={})",
        params_cache.len(),
        preset_cache.len(),
    );

    // Activate + start processing
    let config = PluginAudioConfiguration {
        sample_rate: sample_rate as f64,
        min_frames_count: 1,
        max_frames_count: max_block_size as u32,
    };

    let stopped = instance
        .activate(|_, _| (), config)
        .map_err(|e| anyhow::anyhow!("Failed to activate CLAP plugin: {e}"))?;

    // The latency, which a plugin has once it is active.
    let latency_ext: Option<PluginLatency> = instance.plugin_shared_handle().get_extension();
    let latency = latency_ext.map_or(0, |ext| ext.get(&mut instance.plugin_handle()));
    if latency > 0 {
        log::info!("CLAP plugin {name} has a latency of {latency} frames");
    }

    let started = stopped
        .start_processing()
        .map_err(|e| anyhow::anyhow!("Failed to start CLAP processing: {e}"))?;

    // Pre-allocate buffers
    let output_port_count = output_port_channel_counts.len();
    let output_ports = AudioPorts::with_capacity(audio_out_channel_count, output_port_count);
    let output_channel_bufs: Vec<Vec<f32>> =
        (0..audio_out_channel_count).map(|_| Vec::new()).collect();

    let input_port_count = input_port_channel_counts.len();
    let input_ports = AudioPorts::with_capacity(audio_in_channel_count, input_port_count);
    let input_channel_bufs: Vec<Vec<f32>> =
        (0..audio_in_channel_count).map(|_| Vec::new()).collect();

    let event_buffer = EventBuffer::new();

    Ok(Box::new(ClapPlugin {
        name,
        is_instrument,
        sample_rate,
        audio_in_channel_count,
        audio_out_channel_count,
        latency,
        params_ext,
        params_cache,
        param_ids,
        pending_param_changes: Vec::new(),
        preset_cache,
        preset_data,
        preset_load_ext,
        _bundle: bundle,
        instance,
        audio_processor: Some(started),
        output_ports,
        output_port_channel_counts,
        output_channel_bufs,
        input_ports,
        input_port_channel_counts,
        input_channel_bufs,
        event_buffer,
    }))
}

/// Find a CLAP plugin by ID or bundle path.
/// Returns (bundle, plugin_id, name, is_instrument).
pub(super) fn find_plugin(source: &str) -> anyhow::Result<(PluginBundle, String, String, bool)> {
    use clack_host::plugin::features::INSTRUMENT;

    // Try stripping "clap:" prefix for ID-based lookup
    if let Some(plugin_id) = source.strip_prefix("clap:") {
        // Search installed bundles for this ID (standard paths + extra paths)
        let all_paths = clack_finder::ClapFinder::from_standard_paths()
            .into_iter()
            .chain(extra_clap_bundles());

        for bundle_path in all_paths {
            let bundle = match unsafe { PluginBundle::load(&bundle_path) } {
                Ok(b) => b,
                Err(_) => continue,
            };
            let factory = match bundle.get_plugin_factory() {
                Some(f) => f,
                None => continue,
            };

            for descriptor in factory.plugin_descriptors() {
                let id = match descriptor.id() {
                    Some(id) => id,
                    None => continue,
                };
                if id.to_str().ok() == Some(plugin_id) {
                    let name = descriptor
                        .name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| plugin_id.to_string());
                    let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);
                    return Ok((bundle, plugin_id.to_string(), name, is_instrument));
                }
            }
        }
        anyhow::bail!(
            "CLAP plugin not found: {plugin_id}\nRun `tang enumerate plugins` to list available plugins."
        );
    }

    // Direct path to a .clap bundle — load and pick the first plugin
    let path = Path::new(source);
    let bundle = unsafe { PluginBundle::load(path) }
        .map_err(|e| anyhow::anyhow!("Failed to load CLAP bundle {}: {e}", path.display()))?;
    let factory = bundle
        .get_plugin_factory()
        .ok_or_else(|| anyhow::anyhow!("CLAP bundle has no plugin factory: {}", path.display()))?;
    let descriptor = factory
        .plugin_descriptors()
        .next()
        .ok_or_else(|| anyhow::anyhow!("No plugins in CLAP bundle: {}", path.display()))?;

    let id = descriptor
        .id()
        .ok_or_else(|| anyhow::anyhow!("CLAP plugin has no ID"))?
        .to_str()
        .map_err(|_| anyhow::anyhow!("CLAP plugin ID is not valid UTF-8"))?
        .to_string();
    let name = descriptor
        .name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| id.clone());
    let is_instrument = descriptor.features().any(|f| f == INSTRUMENT);

    Ok((bundle, id, name, is_instrument))
}

// ---------------------------------------------------------------------------
// Plugin trait implementation
// ---------------------------------------------------------------------------

impl Plugin for ClapPlugin {
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
        let processor = self
            .audio_processor
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("CLAP audio processor not active"))?;

        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);
        if frames == 0 {
            return Ok(());
        }

        // Push pending parameter changes into the event buffer
        self.event_buffer.clear();
        for (param_id, value) in self.pending_param_changes.drain(..) {
            let event =
                ParamValueEvent::new(0, param_id, Pckn::match_all(), value, Cookie::empty());
            self.event_buffer.push(&event);
        }

        // Convert MIDI events to clack MidiEvent and push to event buffer
        for (timestamp, bytes) in midi_events {
            let midi =
                clack_host::events::event_types::MidiEvent::new(*timestamp as u32, 0, *bytes);
            self.event_buffer.push(&midi);
            log::debug!("CLAP: pushed MIDI event t={timestamp} data={bytes:02x?}",);
        }

        // Resize per-channel output buffers
        for buf in &mut self.output_channel_bufs {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Build output audio buffers (one port per entry in output_port_channel_counts)
        // Collect all channel slices first, then split into ports to satisfy the borrow checker
        let mut all_slices: Vec<&mut [f32]> = self
            .output_channel_bufs
            .iter_mut()
            .map(|b| b.as_mut_slice())
            .collect();

        let mut remainder = all_slices.as_mut_slice();
        let mut port_buffers: Vec<AudioPortBuffer<_, _>> = Vec::new();
        for &ch_count in &self.output_port_channel_counts {
            let (port_slices, rest) = remainder.split_at_mut(ch_count as usize);
            remainder = rest;
            // Take ownership of the &mut [f32] references out of the slice
            let channel_slices: Vec<&mut [f32]> =
                port_slices.iter_mut().map(|s| &mut **s).collect();
            port_buffers.push(AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_output_only(channel_slices),
            });
        }

        let mut output_audio = self.output_ports.with_output_buffers(port_buffers);

        // Build input audio buffers from audio_in
        // Copy caller's data into our internal buffers
        for (ch, buf) in self.input_channel_bufs.iter_mut().enumerate() {
            buf.resize(frames, 0.0);
            if ch < audio_in.len() {
                let copy_len = buf.len().min(audio_in[ch].len());
                buf[..copy_len].copy_from_slice(&audio_in[ch][..copy_len]);
            } else {
                buf.fill(0.0);
            }
        }

        // Collect input channel slices at the same scope level so they live long enough
        let mut in_slices: Vec<&mut [f32]> = self
            .input_channel_bufs
            .iter_mut()
            .map(|b| b.as_mut_slice())
            .collect();

        let input_events = self.event_buffer.as_input();
        let mut output_events = OutputEvents::void();

        if in_slices.is_empty() {
            let input_audio = InputAudioBuffers::empty();
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    None,
                )
                .map_err(|e| anyhow::anyhow!("CLAP process error: {e}"))?;
        } else {
            use clack_host::process::audio_buffers::InputChannel;

            let mut in_remainder = in_slices.as_mut_slice();
            let mut in_port_buffers: Vec<AudioPortBuffer<_, _>> = Vec::new();
            for &ch_count in &self.input_port_channel_counts {
                let (port_slices, rest) = in_remainder.split_at_mut(ch_count as usize);
                in_remainder = rest;
                let channels: Vec<InputChannel<f32>> = port_slices
                    .iter_mut()
                    .map(|s| InputChannel::variable(&mut **s))
                    .collect();
                in_port_buffers.push(AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(channels),
                });
            }

            let input_audio = self.input_ports.with_input_buffers(in_port_buffers);
            processor
                .process(
                    &input_audio,
                    &mut output_audio,
                    &input_events,
                    &mut output_events,
                    None,
                    None,
                )
                .map_err(|e| anyhow::anyhow!("CLAP process error: {e}"))?;
        }

        // Copy from internal channel buffers to caller's output slices
        for (ch, out_slice) in audio_out.iter_mut().enumerate() {
            if ch < self.output_channel_bufs.len() {
                let src = &self.output_channel_bufs[ch];
                let copy_len = out_slice.len().min(src.len());
                out_slice[..copy_len].copy_from_slice(&src[..copy_len]);
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        self.params_cache.clone()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        let param_id = *self.param_ids.get(index as usize)?;
        let ext = self.params_ext?;
        let mut handle = self.instance.plugin_handle();
        ext.get_value(&mut handle, param_id).map(|v| v as f32)
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let param_id = *self
            .param_ids
            .get(index as usize)
            .ok_or_else(|| anyhow::anyhow!("Parameter index out of range: {index}"))?;
        self.pending_param_changes.push((param_id, value as f64));
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        self.preset_cache.clone()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let index: usize = id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid preset ID: {id}"))?;
        let data = self
            .preset_data
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("Preset index out of range: {id}"))?;

        let preset_load = self
            .preset_load_ext
            .ok_or_else(|| anyhow::anyhow!("Plugin does not support preset loading"))?;

        let load_key_cstring = data
            .load_key
            .as_ref()
            .map(|k| std::ffi::CString::new(k.as_str()))
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid load key"))?;
        let load_key: Option<&CStr> = load_key_cstring.as_deref();

        let file_path_cstring = data
            .file_path
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()))
            .transpose()
            .map_err(|_| anyhow::anyhow!("Invalid file path"))?;

        let location = if data.is_plugin_location {
            Location::Plugin
        } else {
            match &file_path_cstring {
                Some(path) => Location::File { path },
                None => anyhow::bail!("Preset has no location"),
            }
        };

        preset_load
            .load_from_location(&mut self.instance.plugin_handle(), location, load_key)
            .map_err(|e| anyhow::anyhow!("Failed to load preset: {e}"))?;

        log::info!("CLAP: loaded preset {id}");
        Ok(())
    }
}
