/// How much state a plugin may give (`Editor::state`), and a session keep: a plugin that gives more is wrong, or keeps a sample library in its state, and a session file is not the place for that.
pub const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;

pub mod autodetect;
pub mod builtin;
pub mod chain;
pub mod clap;
pub mod clap_editor;
pub mod editors;
#[cfg(test)]
pub mod fake;
#[cfg(feature = "lv2")]
pub mod lv2;
#[cfg(feature = "vst3")]
pub mod vst3;

#[derive(Clone, Default)]
pub struct ParameterInfo {
    pub index: u32,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Names for discrete parameter values (e.g. filter types). When set,
    /// the parameter is an enum: the value is an index into this list (min
    /// 0, max len-1), and the UI shows names and steps by 1 instead of
    /// rendering a continuous bar.
    pub labels: Option<Vec<String>>,
    /// What the value is measured in (`"Hz"`, `"s"`, `"st"`, ...), for showing it.
    /// `None` for a plain number, or when the plugin does not say.
    pub unit: Option<String>,
    /// The sections the plugin files the parameter under, outermost first
    /// (`["Envelope"]`); empty for one at the top level. A client lays a big
    /// plugin's parameters out by it.
    pub group: Vec<String>,
    /// How many steps there are between `min` and `max`: 0 for a continuous
    /// parameter, 3 for one with four values (an enum with four labels).
    pub step_count: u32,
    /// When the parameter has an effect at all, for one that has it only sometimes; `None` for one that always does (and for every plugin that does not say).
    pub used_when: Option<UsedWhen>,
}

/// When a parameter has an effect at all, for a parameter that has one only sometimes (step 95b): the Math's `bits` only in its logic modes, its carrier's `pitch` only while nothing is patched at
/// its second input. A front-end shows a parameter that has none dimmed, and says why (`why`); it does not keep it from being turned, since the value is kept for when it has one. The plugin
/// says the rule and does not work it out: the host does that where the values are (the model's `PluginSlot::unused_because`, the page's `unusedBecause`), from what the user set, so that the rule
/// is data that any front-end reads, and a plugin's parameter that is turned is not a message to the plugin.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct UsedWhen {
    /// What has to hold for the parameter to have an effect.
    pub condition: UsedCondition,
    /// The rule in words, for a front-end to say when it does not hold: `only while nothing is patched at B`, `only in the modes xor, and, or`.
    pub why: String,
}

/// What it is that a parameter's having an effect depends on.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UsedCondition {
    /// The choice `param` (a `ParameterInfo::index` of the same plugin) is set to one of `values`, places in its list of labels.
    Choice { param: u32, values: Vec<u32> },
    /// Nothing is patched at the plugin's second input (`Plugin::side_input`): what the plugin makes of its own is what is heard there then.
    SideUnpatched,
}

impl UsedCondition {
    /// Whether it holds, with `value_of` saying what the user set each parameter of the plugin to (nothing for one it has not: a rule that names no parameter is no rule, and holds)
    /// and `side_patched` whether a cable ends at the plugin's second input.
    pub fn holds(&self, value_of: impl Fn(u32) -> Option<f32>, side_patched: bool) -> bool {
        match self {
            UsedCondition::Choice { param, values } => match value_of(*param) {
                Some(value) => {
                    // (A place below the list is not the first of it: a cast to a whole number would make it one.)
                    let place = value.round();
                    place >= 0.0 && values.contains(&(place as u32))
                }
                None => true,
            },
            UsedCondition::SideUnpatched => !side_patched,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Preset {
    pub name: String,
    pub id: String,
}

/// What an effect that takes a second audio input calls its two inputs (P3g of `docs/patch-graph.md` §11.6: the small half of "the ports that plugins have"): the main one, which is
/// what its chain and the cables that end at its first input give it, and the *side* one, which the cables that end at its second input bring. A page names the two handles by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SideInput {
    pub main: &'static str,
    pub side: &'static str,
}

/// A plugin's own window (step 112): the face that a plugin draws for itself, where a person turns its knobs and picks its sounds. A plugin that has one makes its `Editor` when it is loaded and keeps it in the registry
/// of the windows (`plugin::editors`) under an id that it gives out (`Plugin::editor_id`): the handle is the main thread's (it is an `Rc` there, and a window is not for any other thread), while the plugin goes
/// to the audio thread and comes back.
pub trait Editor {
    /// Open the window, on the main thread, and say why if it cannot be (there is no screen, or the plugin has no window of its own after all). `focus` is whether the window takes the keyboard from the program that has it
    /// (`window::Spec::focus`). One window at a time is the caller's business (the registry sees to it).
    fn open(&self, title: &str, focus: bool) -> Result<Box<dyn OpenEditor>, String>;

    /// What the person has changed in the plugin's window since this was last asked (and what the plugin has changed of itself), as the plugin's own parameter index and the value in the units that the plugin's parameter
    /// has (`ParameterInfo`), in the order it was done: the host applies them as it applies the change of any other front-end. They are taken, so that each is told once. A plugin that has no parameters in its window
    /// says none, which is the default.
    fn take_edits(&self) -> Vec<(u32, f32)> {
        Vec::new()
    }

    /// Tell the plugin's controller the values that the session has for these parameters (the index and the value, as `take_edits` has them), so that a window that is open shows them and one that opens does. The
    /// default is a plugin that has nothing to tell.
    fn set_values(&self, values: &[(u32, f32)]) {
        let _ = values;
    }

    /// The plugin's state (step 112, part C): bytes that only the plugin reads, of what it is that is not a parameter (the sound that a person picked in its window, a model that it loaded), which a session keeps so that the plugin
    /// comes back as it was. None for a plugin that has none to give. Asked on the main thread, while the plugin may be playing (the formats say that it may be).
    fn state(&self) -> Option<Vec<u8>> {
        None
    }

    /// Give the plugin a state that `state` made, before it plays or after, and say why in words if it does not take it. The default is a plugin that keeps none.
    fn set_state(&self, state: &[u8]) -> Result<(), String> {
        let _ = state;
        Err("this plugin keeps no state".to_string())
    }

    /// What the plugin's parameters are now, as `take_edits` has them (the index and the value): a plugin that was given a state, or whose window has been used, may have changed some of them itself, and the session reads them
    /// back so that what it shows is what the plugin has. The default is none.
    fn values(&self) -> Vec<(u32, f32)> {
        Vec::new()
    }
}

/// A plugin's window while it is open. Dropping it closes it.
pub trait OpenEditor {
    /// See to the window (tell the plugin that the person has made it another size) and say whether it is still there: false once the person has closed it, and the plugin's view has been let go of by then. Called on every
    /// turn of whatever pumps the window's events.
    fn poll(&mut self) -> bool;

    /// Bring the window to the front again (a person who asks for a window that is open is shown it); `focus` is whether it takes the keyboard.
    fn raise(&mut self, focus: bool);

    /// Close the window: the plugin's view is let go of first, as the plugin formats say. Closing a window that is closed is nothing.
    fn close(&mut self);

    /// The size of what is in the window, in points.
    fn size(&self) -> (i32, i32);

    /// What is in the window, as a PNG, if that can be had (`window::Window::snapshot_png`).
    fn snapshot_png(&self) -> Option<Vec<u8>>;

    /// The views that are in the window, as text (`window::Window::describe`).
    fn describe(&self) -> String;

    /// Make the window this size, in points, as a host does that has kept the size that a person gave it (`tang window --size`), and say why in words if it cannot be: the default is a window that is the size of its view,
    /// which a VST3 plugin says.
    fn resize(&mut self, width: i32, height: i32) -> Result<(), String> {
        let _ = (width, height);
        Err("the size of this window is the size of its plugin's view".to_string())
    }
}

/// A loaded plugin instance ready to process audio.
pub trait Plugin: Send {
    fn name(&self) -> &str;
    fn is_instrument(&self) -> bool;
    fn sample_rate(&self) -> f32;
    fn audio_output_count(&self) -> usize;
    #[allow(dead_code)]
    fn audio_input_count(&self) -> usize;
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()>;

    fn parameters(&self) -> Vec<ParameterInfo>;
    #[allow(dead_code)]
    fn get_parameter(&mut self, index: u32) -> Option<f32>;
    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()>;

    fn presets(&self) -> Vec<Preset>;
    fn load_preset(&mut self, id: &str) -> anyhow::Result<()>;

    /// How many frames later what comes out is than what went in (the look-ahead of a limiter, the filters of an oversampler), or, for an instrument, than the notes that made it.
    /// The host puts the paths that are added up in step by it (P3f of `docs/patch-graph.md` §11.6: where two paths of different latency meet, the shorter is delayed; the dry part of an
    /// effect that is blended with its output is as late as the output is), so it has to be what the plugin does: too little leaves a comb filter where the dry sound and the
    /// plugin's meet, and too much puts them out of step the other way. It is asked once a block, so an adapter answers from what it already has and does not call into the plugin.
    /// A plugin whose latency changes while it runs may say the new one: the host makes its plan again for it. The default is none.
    fn latency(&self) -> u32 {
        0
    }

    /// Whether the effect has a second audio input, and what its two inputs are called (the default is none: an effect has the one that the chain feeds, and a plugin of another format
    /// that has a sidechain gets silence there, as it did). The host gives an effect that has one a second port in its plan, which cables can end at, and calls
    /// [`Plugin::process_side`] for it.
    fn side_input(&self) -> Option<SideInput> {
        None
    }

    /// Whether a lane that is made for this instrument has its way out cut to begin with (`Route::None`: it is heard where audio cables are drawn from it, and nowhere before). A
    /// plugin whose sound is not its own to put in the mix says so: the audio input is a microphone, which is heard through the speakers that it listens to. The default is a plugin that
    /// is heard as it is.
    fn starts_bare(&self) -> bool {
        false
    }

    /// Whether the plugin does anything with the notes that it is given (step 99c): an instrument does, and an audio input has none to play. A lane whose instrument says no has no key
    /// range, no pattern and no input of events on the canvas, hears no source and takes no cable of notes. The default is a plugin that takes notes.
    fn takes_notes(&self) -> bool {
        true
    }

    /// `process` for an effect that has a side input: `side` is the sound that the cables at its second input bring (a buffer for each channel, as `audio_in` has), or `None` while no
    /// cable ends there, so that the plugin can have a sound of its own in its place. The default is a plugin that has no side input, and leaves it.
    fn process_side(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        side: Option<&[&[f32]]>,
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let _ = side;
        self.process(midi_events, audio_in, audio_out)
    }

    /// The id of the plugin's own window in the registry of the windows (`plugin::editors`), if it has one (step 112): what a front-end opens for a person to use the plugin's own face. The plugin registered its editor when
    /// it was loaded, on the main thread, and forgets it when it is let go of, closing the window first (a window that is open when the plugin is terminated is a view of an object that is gone). The default is a plugin
    /// that has none.
    fn editor_id(&self) -> Option<editors::EditorId> {
        None
    }
}

/// Summary info returned by plugin enumeration.
pub struct PluginInfo {
    pub name: String,
    pub id: String,
    pub is_instrument: bool,
    pub param_count: usize,
    pub preset_count: usize,
    pub path: String,
    pub scan_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginType {
    #[cfg(feature = "lv2")]
    Lv2,
    Clap,
    #[cfg(feature = "vst3")]
    Vst3,
}

#[derive(Default)]
pub struct Runtime {
    /// Lazily created on the first LV2 load — building the world scans every
    /// installed LV2 bundle (~hundreds of ms), which sessions without LV2
    /// plugins shouldn't pay at startup.
    #[cfg(feature = "lv2")]
    lv2: Option<(std::sync::OnceLock<lv2::Lv2Runtime>, usize)>,
    /// What the audio input plays when Tang is inside a host (step 113): the audio that comes into the host's `process`. None for a program that has a sound card, where the audio input opens one.
    host_input: Option<std::sync::Arc<builtin::HostInput>>,
}

impl Runtime {
    #[cfg(feature = "lv2")]
    pub fn with_lv2(max_block_size: usize) -> Self {
        Self {
            lv2: Some((std::sync::OnceLock::new(), max_block_size)),
            host_input: None,
        }
    }

    /// Plugins that are loaded with this runtime hear what `host_input` is given: the audio input that is made plays it and opens no sound card.
    pub fn with_host_input(mut self, host_input: std::sync::Arc<builtin::HostInput>) -> Self {
        self.host_input = Some(host_input);
        self
    }

    #[cfg(feature = "lv2")]
    fn lv2(&self) -> Option<&lv2::Lv2Runtime> {
        self.lv2
            .as_ref()
            .map(|(cell, max_block_size)| {
                cell.get_or_init(|| lv2::Lv2Runtime::new(*max_block_size))
            })
    }
}

/// Load a plugin from the given source, returning a boxed Plugin trait object.
pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
    runtime: &Runtime,
) -> anyhow::Result<Box<dyn Plugin>> {
    // (A test can have a plugin of its own for a name: `fake::install`.)
    #[cfg(test)]
    if let Some(loaded) = fake::load(source) {
        return Ok(loaded);
    }
    if source.starts_with("builtin:") {
        return builtin::load_with(source, sample_rate, max_block_size, runtime.host_input.as_ref());
    }

    let (plugin_type, resolved) = autodetect::resolve(source)?;
    match plugin_type {
        #[cfg(feature = "lv2")]
        PluginType::Lv2 => lv2::load(
            &resolved,
            sample_rate,
            max_block_size,
            runtime.lv2(),
        ),
        PluginType::Clap => clap::load(&resolved, sample_rate, max_block_size),
        #[cfg(feature = "vst3")]
        PluginType::Vst3 => vst3::load(&resolved, sample_rate, max_block_size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_holds_when_its_value_rounds_to_one_of_its_places_and_not_below_the_list() {
        let choice = UsedCondition::Choice { param: 3, values: vec![0, 2] };
        let at = |value: f32| choice.holds(|param| (param == 3).then_some(value), false);
        assert!(at(0.0) && at(0.4) && at(2.0) && at(1.6), "a value is the place that it rounds to");
        assert!(!at(1.0) && !at(0.6) && !at(3.0) && !at(7.0), "and a place that is not one of them is outside");
        // (A place below the list is not the first of it: a cast to a whole number would make it one.)
        assert!(!at(-1.0) && !at(-3.0) && !at(f32::NAN));
        // It reads the parameter that it names and no other, and a rule that names one that is not there is no rule.
        assert!(!choice.holds(|param| (param == 4).then_some(0.0).or((param == 3).then_some(1.0)), false));
        assert!(choice.holds(|_| None, false));
        assert!(choice.holds(|param| (param == 3).then_some(2.0), true), "(the cables do not matter to it)");
    }

    #[test]
    fn a_carrier_is_used_while_nothing_is_patched_at_the_second_input_and_the_values_do_not_matter_to_it() {
        let condition = UsedCondition::SideUnpatched;
        assert!(condition.holds(|_| None, false));
        assert!(!condition.holds(|_| None, true));
        assert!(!condition.holds(|_| Some(0.0), true) && condition.holds(|_| Some(7.0), false));
    }

    /// A window that says nothing of its own about its size: all that `OpenEditor` asks of one.
    struct Bare;

    impl OpenEditor for Bare {
        fn poll(&mut self) -> bool {
            true
        }

        fn raise(&mut self, _focus: bool) {}

        fn close(&mut self) {}

        fn size(&self) -> (i32, i32) {
            (1, 1)
        }

        fn snapshot_png(&self) -> Option<Vec<u8>> {
            None
        }

        fn describe(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn a_window_that_is_the_size_of_its_view_says_so_when_it_is_asked_to_be_another_size() {
        assert_eq!(Bare.resize(900, 600), Err("the size of this window is the size of its plugin's view".to_string()));
    }
}
