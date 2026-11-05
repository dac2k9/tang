# The controller and the session state

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): intents, events, the history (undo and redo), snapshots, telemetry. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

### Session state

The main thread maintains an in-memory session model that tracks all user
changes: plugin selections, preset names, parameter values, mix values, volume,
and effect order. This model is the source of truth for `Ctrl+S` serialization.
Every user action (preset load, parameter tweak, plugin add/remove, reorder)
updates both the session model (for saving) and sends a command to the audio
thread (for playback). The dirty indicator is driven by this model. The model
types and the helpers that keep their indices in step with the audio graph live
in `src/app/model.rs`, and the tempo that patterns are recorded at is the model's
(`SessionModel::bpm`: the first recorded pattern's, 120 for a session with none). The
model, the session path, the dirty flag,
the plugin catalog and the engine handles (command channel, plugin runtime,
notification receivers, the state shared with the note senders) belong to
`app::controller::Controller`; the TUI reaches them through `State::ctl`. The
controller folds what the audio thread reports (a finished pattern recording, a
captured MIDI-learn control) into the model, and `poll` tells the TUI what
changed. It also saves the session (`Controller::save`; Save As is the intent `SaveAs`).

Every node of the model (an instrument lane, an effect, a group, a bus effect, a
modulator) has a `NodeId`: allocated by the model, never reused during a run
but for what undo gives back (see "Undo and redo"), not saved. The engine and the tree address nodes by position, which shifts when
others come and go; an id stays with its node. A lane's instrument plugin sits in
the lane's node. `SessionModel::locate` turns an id back into a position.

Edits that have an `Intent` go through `Controller::apply`, which changes the
model and the audio thread together and answers with `Event`s (`app::intent`,
`app::apply`; `docs/web-ui.md` §5). Built so far: `SetParam` (clamped to the
parameter's range, an enum rounded to a whole value), `SetGain` (instrument
volume, effect and bus-effect mix, group volume and pan, and, for the master, its volume and the limiter's ceiling, each with its own
range), `SetLimiter { on }` (the limiter on the output mix put in the sound or taken out, step 102), `LoadPreset` (by preset id; the values the preset set are mirrored
into the model), `SetModulatorField` (an LFO's rate and waveform, an envelope's
stage, a macro's value or a step sequencer's rate, glide, length and retrigger, of a modulator, clamped to `ModulatorField::range`, a
waveform rounded to its index: sine, triangle, saw, square, random, smooth, and a length and a retrigger to a whole number), `SetSteps { node, values }` (all sixteen steps of a
step sequencer, each held to -1..1, told as `StepsSet` with what it has; refused for a list that is not sixteen long, a value that is not a number and a node that is no
step sequencer), `SetEdgeDepth` (a
modulation cable's depth, -1 to 1) and `SetRange { node, range }` (the notes a lane takes, both
ends included, or every note for none; refused for a node that is no lane, a note above 127 and a low
note above the high one; the TUI's `R` popup asks it too), `SetMute { node, muted }` and `SetSolo { node, solo }` (a lane or a group muted or soloed, see "Mute and solo"; refused
for any other node and, as for every intent, not found for the master; told as `MuteSet` and `SoloSet`, which a `Snapshot` applies; the model's `muted` and `solo` and the audio thread's
`SetMute`, `SetSolo`, `SetGroupMute` and `SetGroupSolo`; saved as `mute` and `solo`), `SetRoute { node, route }` (the way out of a lane or a group cut, `none`, or given back, `out`, see "The way out"; refused for any other node and for a way out given
back where the audio would go round, by the cables there are; told as a snapshot, the links of the graph change; the model's `route` and the audio thread's `SetLaneRoute` and `SetGroupRoute`; saved as `route = "none"`; the TUI's `w` asks it), `SetModulatorEar { node, ear }` (whose notes a modulator that is in no lane and no group hears: the lane's or the group's, or every note for none; refused for any other modulator and for an ear that is neither a lane nor a group; what it hears already changes nothing; told as a snapshot, `NodeKind::Modulator { hears }`; the model's `ear` and the audio thread's `SetModulatorEar`; saved as `hears = "instrument[0]"`; the terminal's `h` asks it: see "Free modulators", "The ear"), `SetFeed { node, feed }` (the link into an effect of a lane's chain or of a group's bus cut, `none`, or given back, `chain`, see "The input of an effect"; refused for any other node and for a link given back where the audio would go round, by the cables there are; told as a snapshot, the links of the graph change; the model's `fed` and the audio thread's `SetEffectFeed`; saved as `feed = "none"`; the TUI's `f` asks it), `SetScale { scale }` (the Piano tab's scale, as
the session file writes it, `F# dorian`, flats, long names and any case taken; refused for text that is
not a scale; answered with `ScaleSet`, which has the root and the pitch classes a client draws from) and
`SetLocked { locked }` (the lock to the scale; the TUI's `k` and `l` ask them too), `SetCableGain { edge, gain }` (the level of an audio cable, held to 0 to 2, told as `CableGainSet`: see "Audio cables (`[[cable]]`)"), `SetBpm { bpm }` (the tempo that patterns are recorded at, held to 20 to 300;
refused, with nothing changed, for a value that is not a number; told as `BpmSet` with the tempo it has now; the model's `bpm` and the audio thread's `SetGlobalBpm`,
saved as the file's `bpm`; the patterns that are recorded keep the tempo they were recorded at; the TUI's `b` asks it), `SetRecording { node, recording }` (start a
recording on the lane `node`, as the terminal's `r` does: a lane that has no pattern is given an empty one, four beats, looping, not in key and not heard, at the session's tempo; the
audio thread is told the tempo and the length first, since the count-in and the recording are counted in them, and then to record; a recording that is stopped is stopped at once, and the
pattern that a recording makes when it is done comes from the audio thread, which `Controller::poll` folds in and the browsers are given a snapshot of; one that is started while it is going
on, or stopped while it is not, is told as it is and sends nothing, since a second start would begin the count-in again), `ClearPattern { node }` (the pattern of a lane taken out, and a recording that is going
on with it; refused for a lane that has none), `SetPatternField { node, field, value }` (`length`, 1 to 32 beats, and the switches `enabled`, `looping` and `transpose`, 0 or 1, rounded; each held to
its range, `PatternField::range`, and sent to the audio thread by its own command; a lane that has no pattern is given an empty one with it, so that the length of its first recording can be
chosen; refused for a value that is not a number, which is judged before the node is looked at) — all three refused, with nothing changed, for a node that is no lane (`a pattern belongs to a lane`) —
told as the one event `PatternSet { node, pattern }`, the pattern the lane has after it (`PatternInfo`, or null when it has none), and none of them a step of the history: a pattern is not put
back by undo, and what is done to one does not forget the steps there are (`Plan::Nothing`), `SetPinned { node, params }` (which parameters of a
plugin a front-end shows as knobs on its node, by the plugin's own indices in the order they are shown, replacing the list there was;
none gives the node back to the front-end's own choice; refused, with nothing changed, for a node that has no plugin, a parameter the
plugin has not, a separator, a parameter twice and more than `MAX_PINNED`; told as `PinnedSet`; the engine and the TUI have no use for
it, the session file keeps it, see "Pinned parameters (`pinned`)"), `SetNodeUi { node, ui }` (where a front-end
draws a node, `{ x, y }` in the pixels of its canvas or none for the front-end's own layout; any node, the master included;
kept to a tenth of a pixel and within 100 000 either way, `NodeUi::tidy`; told as `NodeUiSet`; the engine and the TUI have no
use for it, the session file keeps it, see "Node positions (`ui`)"), `RenameGroup { node, name }` (what a group is called: the name
without the spaces round it, at most `MAX_NAME` (64) characters and none of them a control character, and nothing gives the group back the
name it has by default, "Group" and its number; refused, with nothing changed, for a node that is no group and for a name that is not one;
told as `GroupRenamed`, which carries the name the group has now; the engine has no use for it, the session file keeps it, and the TUI's `R`
on a group header asks it); `RenameMacro { node, name }` (what a macro is called, step 110: a group's rule, nothing gives the macro the node name `Macro` back; refused, with nothing changed, for a node that is no macro, `only a macro has a name and a knob to show`, and for a name that is not one; told by a snapshot, the graph naming the node by it; the engine has no use for it, the session file and the library of modules keep it) and `ExposeMacro { node, expose }` (whether the box of a module that holds the macro shows its knob, on to begin with; refused for a node that is no macro; told by a snapshot; kept as the name is); `Save` (the session's own file); `SaveAs { name }` (the session written as the file that
`session::plain_file_name` makes of `name`, beside its own file, which is its file from then on: the spaces round the name go, `.toml` is added
unless it ends so, and it is a plain file name, no `/` or `\`, none of `:*?"<>|` or a control character, something before the extension, not a name
Windows keeps and at most 255 bytes; refused, with nothing written and nothing changed, for a name that is not one and for a file that is there
already, a link that leads nowhere included, unless it is the session's own (that is a save), as `IntentError::Invalid`, and a write that fails is
`Io`; told as `Saved { file }` with the new name; no step of the history; a file that is there is refused by the write itself (an exclusive create,
`session::save_new`), not by a look at the path first; the TUI's `Ctrl+Shift+S` asks it too, and so does the page's "Save as…"), `OpenEditor { node }` (a plugin's own window, step 112: the window of the plugin in `node`, an instrument or an effect of a lane or a bus, opened on the machine that runs Tang after the session's value for each of its parameters was told to the plugin's controller, or brought to the front if it is open; refused, with nothing changed and no revision used, for a node that is not there, `that node has no plugin`, `that instrument has no plugin`, `X has no window of its own`, a scan of the installed plugins that is still going and a machine that has no screen (`Controller::window_check`); told as `EditorOpened { node }`; no step of the history and nothing unsaved, a revision of its own; handled early in `apply`, as `Save` is: `Controller::open_editor_intent` in `app/windows.rs`; what a person does in the window comes back through `poll` as `SetParam`, which `poll` applies for the windows of every plugin, and an open window is told what the session sets (`tell_window`); what a window leaves in the plugin that is not a parameter, its state (`PluginSlot::state`, step 112 part C), is read when the window is closed, when the session is saved and when a module is (`keep_the_states` in `app/states.rs`: the session is unsaved if it changed, the parameters that the plugin changed itself are read back, no step of the history has it): see "A plugin's own window" in `plugins.md`), `SaveModule { node, name }` (the chain of effects of a lane, or of the lane that an effect is in, written to the library of modules under `name`: no change of the session, no step of the history; the model's `modules_dir`, `ModuleSaved { name }`, the catalog told again: see "Modules: effects and instruments, with the cables between them, saved to a file"), `SaveSelectionAsModule { nodes, name }` (the same for the plugins that are chosen, from any lanes: the instruments and effects in `nodes`, with the audio cables that have both ends among them, and the modulators in `nodes` with the cables that they have to what is in the module; refused for a group, a bus effect, a source of notes, a modulator with no plugin to go with it, a lane that has no instrument and for none at all: see "Modules: effects and instruments, with the cables between them, saved to a file"), `Undo` and `Redo` (the history, below); and fifteen that change
the structure (sixteen until step 96, which took `AddModulator` out): `RemoveEdge`
(a cable, with the cables that drive its depth), `AddFreeModulator { kind, hears }` (an LFO,
an envelope, a macro at rest, an armed MIDI control, or (step 52) a velocity or a key control, bound at once, or (step 57) a step sequencer (a staircase), at the end of the session's own rack, hearing the lane or group `hears` or every note: see "Free modulators"; since step 96 the one intent that makes a modulator, `AddModulator { parent, kind }`, which put one in the rack of a lane or a group, being gone), `AddModEdge { modulator, to }` (a cable at depth
`NEW_CABLE_DEPTH`, 0.5, to a plugin parameter or to a modulator's setting: where a cable to a
parameter may end is `SessionModel::param_targets`, any plugin of the session; a cable to a `rate`, `attack`,
`decay`, `sustain` or `release` port of a modulator, or to the `depth` port of one of that
modulator's cables, is as `SessionModel::modulators_in_reach` and `field_targets` say (what the
TUI's `m` on a modulator's field and its `t` offer), the other modulators, never itself; refused
for a port that is not one of these, a node or a plugin out of reach, a modulator that has no
such setting, a parameter or setting it drives already; a cable to a setting has a telemetry
slot like any other, for the value that setting is set to), `Modulate { to, kind, depth }` (a modulator of `kind` made for what the port `to` is, and a cable from it to
the port, as one thing: what the TUI's `m` and `l` do, and what the page's menu on a knob asks. It is one of the session's own and hears
the lane of the plugin (a lane's instrument or an effect of its chain) or the group of a bus effect, and, for a setting of a modulator of the session's own or the depth of one of its cables, what that modulator hears,
where the new one is a sibling that drives it. The cable
starts at half depth, or at `depth` clamped as `SetEdgeDepth` clamps it, and a MIDI control is armed to learn as for
`AddFreeModulator`. Refused, with nothing changed (nothing is made and no id is used), for what `AddModEdge` refuses of a cable
to `to` from a modulator that is not there yet, for a group, which has no parameter, and for a depth that is not a number:
`Controller::modulate_intent`; the plan of the cable is `param_cable` / `field_cable`, which `AddModEdge` uses too),
`AddCable { from, to, gain }` (an audio cable from an output of audio to an input of audio: see "Audio cables (`[[cable]]`)"; with the ports of events, a cable of notes from a source to a lane, with no level: see "Sources of notes and cables of notes"), `AddSource { source }` (the keys or a device
put in the session as a source of notes, at the end of the sources: the same section), `AddEffect { parent, plugin }` (a plugin of the catalog, loaded by the controller
and nothing else, at the end of the chain of a lane, `parent` being the lane's node, or of a
group's bus: see `Controller::add_effect`), `AddFreeEffect { plugin }` (a plugin of the catalog as an effect that stands by itself: a lane at the end of the session that has no instrument, full range, in no group and with its way out cut, with the effect in it,
the lane's node made first and the effect's second; refused, with no lane made and no id used, for what `AddEffect` refuses of a plugin; the page's "+ Effect", the terminal's `N`: see "An effect that stands by itself"), `AddInstrument { plugin }` (a lane at the end of
the session, full range and in no group, with an instrument of the catalog in it: see
`Controller::add_instrument_lane`), `ReplaceInstrument { node, plugin }` (the instrument of
the lane `node`, or an empty lane's, replaced by one of the catalog; the cables that drove
the old plugin's parameters are taken out: see `Controller::put_instrument`), `ClearInstrument { node }` (the instrument of the lane `node` taken out, the lane staying with its effects,
its modulators, its pattern and the rest; the cables that drove the plugin's parameters are taken out, and the plugin is handed to the history's channel: see `Controller::clear_instrument`), `MoveEffect { node, index }`
(an effect of a lane's chain or of a group's bus put at place `index` of its chain, 0 being the first: the effects
between move one place the other way, the audio path goes through them in the new order and every cable stays
on the plugin it drove; refused for a node that is no effect, a place the chain has not and the place the
effect is at: see `Controller::move_effect` / `move_bus_effect`), `AddGroup { members }` (a group at the end, with no effects and
no modulators, full volume and a centred pan, and the lanes in `members` put in it at once, as `SetLaneGroup` does for each; none
makes an empty group; refused, with no group made, for a member that is no lane), `SetLaneGroup { node, group }` (a lane put in a
group, or taken out of the one it is in for none: the group it leaves lets go of it, its cables to the lane's plugins and
modulators go, with the cables that drive their depths, and what they drove has the value that was set; the cables to the members
after it name their nodes, and the audio thread finds them where they are now; a lane put in the group it is in is as it was; refused for a node that is no lane and a group
that is no group: `Controller::set_lane_group`, which the TUI's `g` asks too) and
`RemoveNode` (a modulator, with its cables and
those of the others that drove its settings; an effect or a bus effect, with the cables that
drive its parameters and what drove their depths, the audio path closing up; a lane, with its
instrument, its effects and its modulators and the cables of its group to it, but not the last
lane of a session (`a session keeps its last lane`); a group, with its bus effects, its modulators and every cable of it, its
members staying in no group, and what the cables drove in them given back: `Controller::remove_lane` / `remove_group`. A `RemoveEdge` and a
`RemoveNode` of a modulator, of an effect, of a bus effect, of a lane and of a group are put back by undo (step 34, "Undo and redo"); the page still asks first, in the
words of the time before there was an undo). An intent that changes the
structure has no event for a client to apply: `Applied::restructured` is set, `apply` calls
`resync` at once, and the subscribers are given a snapshot at the revision after the
intent's. Every applied intent bumps `Controller::rev`;
one that fails (`NotFound`, `EdgeNotFound`, `Invalid`) changes nothing. The TUI's
parameter, gain, preset and modulator-row edits use them (what each row of a
modulator's parameter pane is comes from one table, `modulator_row` in
`src/app/model.rs`, which says its range, whether it is an enum and which intent it
makes; the pane is drawn (`modulator_pane`) and the value popup opened
(`modulator_edit_state`) from it too, so a row's label, range and default are in one
place in the TUI; the Type row, which turns an LFO into an envelope, is still the TUI's
own edit). Step 33 moved the TUI's structure edits that have intents onto them: its plugin selector (`AddEffect`,
`ReplaceInstrument`), `d` (`RemoveNode`, and `ClearInstrument` for an instrument that has a plugin), `n` (`AddInstrument`), `g` (`AddGroup`, `SetLaneGroup`), Shift+Up and Shift+Down on an effect
(`MoveEffect`), `m`, `l` and `t` (`Modulate` for a new modulator and its cable, `AddModEdge` for a modulator that is there, `AddFreeModulator` and
`SetEdgeDepth`; the target that a popup lists is a positional kind, which `SessionModel::endpoint_of` turns into the node and port that
`AddModEdge` and `Modulate` name) all go through `State::apply`, as the browser's requests
go through the controller, so that they are in the history, and `Controller::remove_effect`, `move_effect`, `remove_lane` and the others are
the intents' own (`SessionModel::remove_effect` / `move_effect` and the bus's keep the modulator targets of the lane and of its group
pointing at the same plugins, as the audio thread does). What the TUI still edits itself, for want of an intent: the order of the lanes
(`Controller::swap_lanes`, which forgets the history) and the Type row. (The pattern was one of them until
step 50: `r`, `d` and `Ctrl+R` on a pattern and the rows of its pane ask `SetRecording`, `ClearPattern` and `SetPatternField` now, which are no step of the history.)

**Undo and redo** (`src/app/history.rs`, `src/app/journal.rs`, `docs/web-ui.md` §5, D35 to D37). The controller keeps a history of what a person did: a
step for each change that has an inverse, `SetParam`, `SetGain`, `SetModulatorField`, `SetSteps` (all the steps of a step sequencer are one thing: a stroke across the bars is one step, "set steps"), `SetEdgeDepth`, `SetRange`, `SetMute` and `SetSolo` (in the words `mute the lane of Oscillator`,
`unmute Pad`, `solo an empty lane`: never merged with the one before them, since a toggle that is merged with its opposite leaves a step that does nothing, and none for what changes nothing), `SetRoute` (in the words `cut the way out of the lane of Oscillator`, `give Pad its way out`: as a mute is, and what is given back goes where the lane's group and the cables allow), `SetFeed` (in the words `cut the input of Filter`, `give Filter its input back`: as a way out is, and what is given back is fed where the cables allow), `SetModulatorEar` (in the words `make an LFO hear the lane of Oscillator`, `make an envelope hear every note`: never merged, none for what is as it is), `SetNodeUi`,
`SetPinned`, `RenameGroup`, `RenameMacro` ("rename a macro") and `ExposeMacro` ("show a macro on its box", "hide a macro from its box": neither is merged with another, and none is a step for a name or a flag that the macro has already, nor for a name that is refused), `SetScale`, `SetLocked`, `SetBpm`, `MoveEffect`, `SetCableGain` (the level of an audio cable, merged as a depth is: `Key::Level`, "set the level of an audio cable"), and what is added: `AddEffect` (a lane's or a bus's), `AddFreeEffect` (in the words `add Reverb`: undo takes the lane out with the effect in it), `AddInstrument`,
`AddFreeModulator`, `AddModEdge`, `AddCable` ("draw an audio cable", or "draw a cable of notes" for the ports of events), `AddSource` ("add Keys", "add Seaboard"), `Modulate`, `CollapseIntoModule` ("collapse into Room": an add of the record alone, taken out by `UngroupModule`, step 109), `AddGroup` (of lanes that are in no group) and `SetLaneGroup` (always since step 96: the lane's group no longer drives it), made of the
intents that put it back and the intents that do it again (read from the model before the change, so that it puts back what the change
overwrote and does again what was asked) and the words of a button for it (`set cutoff`, `add Reverb`, `draw a cable`). **What is added is
taken out by undo (`RemoveNode`, `RemoveEdge`) and made again by redo under the ids it had**: `NodeIds::reuse` says which ids come next
before any new one, `Controller::carry_out` writes down the ids it hands out (`made_last`, `Made`), a `Plan::Added` becomes a step once they are
known (`Step::redo_made`), and `carry_out_all` says them to the allocator, refuses ids that are in use (`node 7 is there already`) and drops those
the intents did not use, so every step that names the node or the cable goes on naming it, whatever is undone and redone around it. (A telemetry
slot is not given again: a node that comes back has new slots, and they are never reused, 4,096 of them.) **What is taken out is put back by
what was taken** (step 34, D37), exactly as it was: while an intent is carried out the controller writes down what it takes out (`Controller::journal`,
a `Journal` of `Inverse`s; the TUI's and the tests' own removals leave nothing), the model hands back what it removed (`SessionModel::remove_edge`
returns the cables as `RemovedCable`: the cable with its id and its slot of the telemetry, its place, and what it was centred on, `centre_of`), a
`Plan::Removed` becomes a step that holds the journal (`Step::removed`, `Taken`), and undo puts everything back in the reverse order
(`Controller::rewind`, one revision, the structure said to have changed): each cable goes into its modulator at its place
(`SessionModel::insert_target_at`, `GraphCommand::InsertModTarget` / `InsertGroupModTarget`, which carry the cable, its id and what it drives by node: a cable that drives the depth of a later one names it by its id, so
nothing is told a place), with the id and the slot it had; redo takes it out again and the step holds what that took.
Built for `RemoveEdge` and for `RemoveNode` of a modulator: the cables that address the modulator (its siblings', and its group's when the lane is a
member) go first, each written down as a cable, and then the modulator is written down with its own cables and what each was centred on
(`Inverse::Modulator`; `SessionModel::remove_modulator_at` hands it back), and undo puts it back at its place in its rack
(`SessionModel::insert_modulator_at`; the audio thread's `InsertModulator` / `InsertGroupModulator`, then `SetModulatorOutput` for the slot it publishes to and
`AddModTarget` / `AddGroupModTarget` for each cable, with its id and what it drives by node; a cable that drives nothing that is there now, because the
session was changed without the history, is not put back, in the model or on the audio thread) before the cables that addressed it. A step is made of it in the words of the button that adds one like it (`remove an LFO`, `an envelope`, `a MIDI control`), from the page and
from the TUI's `d` alike. An audio cable is put back by `Inverse::AudioCable` (the model's `AudioCable`, which goes back among the cables by its id, and the audio thread's `AddCable` at its level; "remove an audio cable"): it
is taken out before the nodes that it starts and ends at (`Controller::take_out_audio_cables_at`, for a lane, a group, an effect and a bus effect that go), so it is put back after them. So is a cable of notes (`Inverse::EventCable`, "remove a cable of notes";
`take_out_event_cables_at`, for a lane or a source that goes: the audio thread has nothing of it, the lane is told what it hears), and a source of notes goes back by `Inverse::Source` at its place among the sources ("remove Keys", "remove Seaboard"; see "Sources of notes and cables of notes"). An effect of a lane's chain or of a group's bus is put back the same way, with **the very plugin that was in the chain**
(`Inverse::Effect`: the model's `PluginSlot` as `SessionModel::remove_effect` / `remove_bus_effect` hands it back, with its node, its values, its preset and its
mix, and the place it was at; the cables that drive its parameters go first, each written down as a cable). The controller makes a channel for the plugin
(`Controller::channel_for`: one for each removal that an intent carries out, none for a call that no intent asks for), sends the other end with the
command (`RemoveEffect { keep }`, `RemoveGroupEffect { keep }`), and the audio thread hands the plugin to it in place of the channel that gives plugins back to
the main thread (and to that one if no one is listening any more, `hand_back`); the journal holds the end that listens (`Kept::Asked`). Undo gets the plugins
first, so that what can fail fails before anything is changed (`Controller::rewind`): it waits for them for `recovery_wait` in all (`EngineHandles::recovery_wait`,
250 ms in the program: the audio thread takes a command within a block or two), and a plugin that has not come, or that the history let go of, is loaded again
(`reload_plugin`: from the id it was loaded from, with the preset it had and then the values the model has, which are the user's); a plugin that does not load,
or has other outputs than the chain's, refuses the undo with the reason (`Reverb could not be loaded`) and the step is dropped. Then everything goes back in the
reverse order (`SessionModel::insert_effect_at` / `insert_bus_effect_at`; the audio thread's `InsertEffect` / `InsertGroupEffect`, at the mix it had; the cables that address the
effects after it, the modulators' that drive them, name their plugins, and the audio thread tells its own the place they have now). A plugin can hold a good deal of memory, so the history keeps the plugins of the newest steps only (`MAX_KEPT_PLUGINS`, 8: a
step that cannot keep all of its plugins keeps none, `History::keep_the_newest_plugins`) and an older step loads them again; and `Controller::shutdown` lets go of
what the history holds before the plugin runtime goes, since a plugin has to be dropped while the libraries it came from are there. A step is made of an effect
in the words of the button that adds one (`remove Reverb`). **A lane is put back whole**: the controller takes out the cables of its group to it first (each
written down as a cable), and then the lane, handed to a channel of its own (`RemoveInstrument { keep }`, `Inverse::Lane`: the model's `InstrumentNode`, with its
plugins' slots, its modulators and their cables, its pattern and its group, and `KeptLane`, the lane that the audio thread hands back with its plugins and
whatever they hold, its modulators and what they have in hand, its pattern and its meter); the lane's own modulators' cables go with it. Undo gets the lane first
(`Controller::recover_lane`): it waits `recovery_wait` for it, and **refuses the undo with the reason** if it did not come (`the lane of Oscillator was not handed
back by the audio thread`; a lane cannot be built again from the model, as a plugin can be loaded again, and the step is dropped); a lane that gave up its plugins
(the history keeps eight plugins in all, and lets go of the oldest steps' by taking them out of the lane it keeps, `KeptLane::take_plugins`) has them loaded again
into it (`put_plugins`: with the preset and the values each had). Then everything goes back in the reverse order: the lane (`SessionModel::insert_lane_at`, the audio
thread's `InsertInstrument`, at its place among the lanes and in its group; the pattern players are told the places the lanes have), the tempo (`SetGlobalBpm`: the pattern player of the lane has the one there was when it went), and the cables of its group.
What the lane counted as held and what its pattern was playing is forgotten when it goes in (the notes were let go of while it was out, and a plugin that was not sent
the note-off keeps its voice, as it does when a lane's range is changed under a held note), and a group that is not there is no group. The words of a step are
`remove the lane of Oscillator`, or `remove an empty lane` for a lane that has no instrument. **A group is put back whole**, as a lane is: the controller takes out the cables
of its modulators first (each written down as a cable), and then the group, handed to a channel of its own (`RemoveGroup { group, keep }`, `Inverse::Group`: the model's
`GroupNode`, with its bus effects' slots, its modulators and its meter; the lanes that were its members, by node, read before it goes, since they are in no group once it has
gone; and `KeptGroup`, which the audio thread hands back with its bus effects and whatever they hold, its modulators, its meter, its volume and its pan). Undo gets the group
first (`Controller::recover_group`): it waits `recovery_wait` for it and **refuses the undo with the reason** if it did not come (`Pad was not handed back by the audio
thread`, or `Group 2` for a group that has no name, by the place it had), and a group that gave up its bus effects (`KeptGroup::take_plugins`) has them loaded again into
it (`put_plugins`: with the preset and the values each had); before anything is changed it refuses a group whose node, effects, modulators or cables are there already
and a member that is gone or is in a group (what was done after the removal was undone first, so that only a front-end that changed the session without an intent can have
made it so). Then everything goes back in the reverse order: the group (`SessionModel::insert_group_at`, the audio thread's `InsertGroup`: at its place among the groups,
the lanes in the groups from there on told the group they are in now, and its members, as the places their lanes have now, put in it) and then its cables, the last that
went first. The words of a step are `remove Pad`, or `remove Group 2`. With the group every removal of a node is a step: `Plan::Barrier` is no longer made for a node. **What is replaced is put back** too (step 35, the same
journal). An instrument that another replaces (`ReplaceInstrument`) is taken out of its lane as an effect is taken out of a chain: `Controller::install_instrument` makes a channel for it, asks the audio thread to hand it there
(`SwapInstrument { keep }`) and writes down, as `Inverse::Instrument`, the model's plugin (its values, its preset and its pins), the lane's note remap and pitch bend range (which go with the instrument they were for) and what
keeps the plugin, after the cables that drove it, each written down as a cable. Undo gets the plugin first (`recover_plugin`, told what the place asks of a plugin: an instrument has audio outputs, an effect has the chain's; one
that did not come, or that the history let go of, is loaded again with its preset and the values the user set), and then swaps it back in (`SwapInstrument`, with the remapper made again from the model) before the cables come
back, or empties the lane (`ClearInstrument`) if it had none, and refuses a lane that is gone or is no lane any more. The words are `change the instrument to Oscillator` (by the catalog's name for the new one), or `put
Oscillator in a lane` for a lane that was empty; redo replaces it again, loading the new instrument from the catalog. **Taking the instrument out of a lane** (`ClearInstrument`, step 85) is the same record read the other way: `Controller::clear_instrument` takes out the cables that drove the plugin (each written down), asks the audio thread to hand the plugin to a channel of its own (`ClearInstrument { keep }`) and writes down `Inverse::Instrument` with the model's plugin, the note remap and the pitch bend range and what keeps the plugin; undo gets the plugin and swaps it back in (`SwapInstrument`) before the cables come back, and redo takes it out again. The words are `take Oscillator out of its lane`. A preset that is loaded (`LoadPreset`) stays in the plugin and overwrites what it had, so what it overwrote is
written down before the model is given the preset's values (`Inverse::Preset`: the preset the plugin had, and the rows that the preset changes or that the user set over what they had, with their baselines,
`rows_a_preset_changes`: the preset that is loaded again would put its own over the user's); undo has the audio thread load the preset it had again, if the plugin has one by that name (a name that a session file gave and the
plugin has not is kept as the name), gives each row its value and its baseline in the model and sends the values (`SetParameter`, which centres the cables that drive them), and refuses a plugin that is gone, a bus effect's
and a lane that has no instrument any more. The words are `load preset Small Room`; redo loads the preset again. What a plugin holds that is neither a preset nor a parameter (a sample loaded in its own window, say) is not put
back by this: `Plugin` has no state to save. Two cables that drive each other's depths (only a file can make that) are not put back (`RemovedCable::still_driven`: one goes
while the other still drives its place, and once it has gone that place cannot be told from the next cable's), and a step is not made of such a
removal. A journal holds what it took by the places it had, so a structural edit that no intent asks for (the TUI's swap of two lanes, a direct call of a
method of the controller that takes something out, moves an effect or puts a lane in a group) forgets the history
(`Controller::forget_the_history_unless_an_intent_asks`: nothing is forgotten while an intent is carried out, which is a step or forgets it itself). A new
modulator and the cable to it are one intent, `Modulate`, and so one step in the words of the TUI's `m` and `l` (`modulate cutoff`, `learn cutoff`, `modulate rate`
for a setting of a modulator): it is a `Plan::Added`, whose step takes out the modulator, which takes its cable with it (`takes_out`: one node and one cable were
made), and redo makes both again under the ids they had. (There is no joining of the steps of several intents: the TUI's `m` and `l` were the one thing that
did it, until `Modulate`, and nothing asks for it any more.) Changes of one thing (one parameter, gain, setting or cable) a second or less apart are one step, so that a drag
is one (`SAME_STEP`; 200 steps are kept, `MAX_STEPS`; both are generated into `constants.ts`), a change after an undo ends what could be
redone, and an intent that is refused leaves no step. `Undo` and `Redo` are intents, answered as any are: each intent of the step is a
revision of its own (`Controller::undo` / `redo`), neither is a step, and one with nothing to do is refused (`there is nothing to undo`) and
is no revision; a step whose node has gone (a front-end took it out without an intent) is dropped, with the reason, and so is one whose cable's
id is in use (`cable 7 is there already`). What cannot be undone yet
(a group made of a lane that is in another (the lane leaves it, and goes back to none and not to that one when the new group is taken out; a lane put in a group or out of one was a barrier too, where its old group's modulators drove it, until step 96, when the cables of the modulators that hear the group stopped going with the lane), a `RemoveEdge` of cables that drive each other's depths,
and `Controller::set_instrument` and `clear_instrument`
when no intent asks for them: the tests' own calls) empties the history, since a step from before it would be put back over what it changed; `Save` is no change. The TUI
asks by intents for what has them (the plugin selector, `d`, `n`, `g`, Shift+Up and Shift+Down on an effect, `m`, `l` and `t`), so they are in the
history; what has none is not (the Type row; the order of the lanes empties it) and the others do not empty it, and what is done to a pattern has intents and is no step (a
pattern is not put back by undo: Open question 11 of `docs/web-ui.md`). What Undo and Redo would do is
told to every browser as a message of its own, `History { undo, redo }` (the words of a button, or null), after the snapshot and the catalog
and after every change of it, before the answer to the request that made it (`web::owner`): it is no event, since a second event of one
revision is taken for a repeat. The page's buttons and keys ask for them (`ClientState.history`, `app/UndoRedo.tsx`) and so do the TUI's
Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z; the mock engine has the same history (`client/history.ts`), pinned to the controller's by
`fixtures/history.json`.

Whoever mirrors the session (the web runtime, for its browsers) subscribes an
`EventSink` (`app::subscribers`). The controller tells each about every applied
intent, and keeps what they have been told as a snapshot with every intent applied
to it (`Snapshot::apply`: the rules of `apply` read from the other side, which the web
client's `applyEvent` follows too). Because the TUI's other edits and what the audio
thread reports change the model with no intent, `Controller::resync` compares that
snapshot with the session now, and if they differ gives the sinks a snapshot to start
again from (`EventSink::resync`), at a revision of its own. The TUI loop calls it
after handling its input, and `poll` when the audio thread's reports changed the
model; with nobody subscribed it does nothing. Each TUI edit that becomes an intent
leaves less for it to find. (Compiled with the `web` feature or in tests only.)

The audio thread publishes what it is doing through `telemetry::Telemetry`
(`src/telemetry.rs`, `docs/web-ui.md` §6): a fixed array of atomic cells, one
*slot* each, that any thread can read. The model owns the `SlotAllocator` and
gives a slot to each modulator (its output this buffer) and a second to each step sequencer (where it is: the step as the whole part of the number, how far into it as the fraction), to each modulator target
(the value that what it drives was set to this buffer, after modulation: a plugin
parameter, or a setting of a modulator, which the engine publishes as the last write to it) and two to each
lane and group (a peak and a clip count; the master's are slots 0 and 1,
`telemetry::MASTER`, and how far its limiter brings the sound down is the last of the 4,096, `telemetry::MASTER_REDUCTION`, which is never handed out). They sit on the model's nodes (`ModulatorSlot.output`, `ModulatorSlot.position`,
`ModTargetSlot.effective`, `InstrumentNode.meter`, `GroupNode.meter`) and reach
the engine in the `ModTarget` or in `SetModulatorOutput` / `SetModulatorPosition` / `SetLaneMeter` /
`SetGroupMeter` (`modulator_slot_commands` makes the first two, the output's first). A slot is never reused, and a session that runs out of them just
has items without telemetry. The audio thread only stores (relaxed atomics, no
allocation, no lock). Lanes, groups, modulators and targets are created through
the loader or `Controller::{add_lane, add_group, add_modulator, add_target}`,
which allocate and send the slots; `SessionModel::check_ids` verifies them. The
`CLIP` light is the only reader of the meters' counts so far. Besides the slots, `Telemetry` has
the scope ring, the last frames of the master mix (see "Architecture").

Loading a preset clears all parameter overrides for that slot. The preset
sets every parameter to its own values, so previous overrides are discarded.
Any parameter tweaks made after the preset load are recorded as new overrides.
