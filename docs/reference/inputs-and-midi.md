# Sources of notes, the audio input, MIDI and the host

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): sources of notes and cables of notes, MIDI routing, the virtual piano, the audio input (the microphone), the audio host. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

### Sources of notes and cables of notes (`[keys]`, `[[device]]`, `[[cable]]`)

Which lane hears which input (P4b of `docs/patch-graph.md` §11.6, step 82; the audio thread's side, what a lane does with the set of sources it is told, is "Sources" under "MIDI routing"). A **source of notes**
is a node of the session with an output of events: **the keys** (the computer's keys, the page's piano and keys and every browser's: one source, `Source::KEYS`) or **a MIDI device**, by its name. A session has **none
until a person adds one**, and a lane that **no cable of notes ends at hears every source**: a session with no source is the file it was, every lane hears everything, bit for bit (the oracle's corpus and the golden
snapshots are as they were). A **cable of notes** goes from a source to a lane; a lane that has some **hears the sources they start at, and no others**, so that two keyboards can play two synths (the keys to the first,
a device to the second). Adding a source, or a cable from a source to a lane that heard it already, changes nothing that is heard; a cable that leaves a lane with fewer sources than it had makes it let go of what the
others had sounding in it (no note, pedal or bend hangs); taking the last cable of notes out of a lane gives it every source again.

```toml
[keys]                  # the keys as a source (optional: a cable that starts at "keys" makes it too; an empty table is the keys with no place)
ui = { x = 0, y = 40 }  # where a front-end draws it, as every node's `ui`

[[device]]              # a MIDI device: `name` is what its port is called, or as much of it as tells it
name = "Launchkey"      # the ports whose name is this exactly, and else those that have it in them, in any case (a Launchkey Mini has two: "… MIDI Out" and "… DAW Out")
ui = { x = 0, y = 300 }

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"

[[cable]]
from = "keys"           # or "device[0]": the devices are counted from 0 in the order of the file
to = "instrument[0]"    # always a lane, and no `gain`: a cable of notes has no level

[[cable]]
from = "device[0]"
to = "instrument[1]"
```

- **The file.** `[keys]` comes first, then the `[[device]]` tables, then the instruments and groups, and the `[[cable]]` tables after them, as the audio cables are (`app::save::sources_to_save`, `cables_to_save`: the audio cables and the cables of notes are one series of ids and are written in the
  order of the ids; `session::to_toml` takes the sources as `SaveSources`). A cable's kind is told by its places (`CablePlace::{Keys, Device(n)}`, `is_source`): from `keys` or `device[n]` it is a cable of notes and must end at `instrument[n]`; one that ends at a lane and
  starts at an audio place is refused with the rest. A device's `name` is the text without the spaces round it, not blank, at most `MAX_DEVICE_NAME` (256) characters and with no control character (`session::device_name`: one rule for the file and for `AddSource`, so
  that a name the intent takes is one that its file loads; the case is the name's, two names that differ by it are two nodes, and a name that is there twice refuses the session). **What refuses the session**, with the cable's place (`app::load::load_event_cable`): a source
  that is not in the session (`device[2]` with two devices), an end that is not a lane, a cable between two nodes that another is between already, and a `gain` (`cable[0].gain: a cable of notes has no level`). The ids are those of the other cables' series, in the order of the file.
- **The model.** `SessionModel.sources` (`SourceNode { node, kind: SourceKind::{Keys, Device(name)} }`, in the order they were added; a file's keys come first) and `SessionModel.event_cables` (`EventCable { id, from, to }`, in the order of the ids; `from` is a source's node and `to` a lane's, which is its
  instrument's node); `NodePath::Source(index)`, which `nodes()` lists after the lanes; `SessionModel::lane_sources(inst, resolve)` (every source for a lane that no cable ends at, and otherwise the sources of the nodes the cables start at, as `resolve` says them: the keys are `Source::KEYS`, a device is
  the sources of the open ports that its name stands for, **none while it is not plugged in**, so that a lane that is wired to a device that is not there hears nothing, and not everything); `event_cable_ends` (the rule of what a cable of notes can join), `insert_source_at`, `event_cable`, `remove_event_cable`, `event_cables_at`; `check_ids`
  holds the keys to one, a device to one node by name, a cable to a source and a lane, and one cable between two nodes.
- **The graph a client gets.** The node `NodeKind::Source { source: SourceInfo }` (`{"type": "keys"}` or `{"type": "device", "name"}`) with a port `eventsOut`; a lane has a port `eventsIn`. The edges: `EdgeKind::Events { id }` for a cable that was drawn (after the audio cables, before the modulation cables) and the derived
  `EdgeKind::Hears` for what a lane hears by having none: one from each source node to each lane that takes notes and that no cable of notes ends at (none while the session has no source node; none to the lane of an audio input, which takes no notes: step 99c), as the links of the audio path are derived. The page draws them (see "The page" below).
- **The intents.** `AddSource { source }` (a node at the end of the sources; refused for the keys when they are a source already, for a device whose name is not one and for one whose name is a source already), `AddCable` with the ports `eventsOut` and `eventsIn` (a source to a lane, with
  no `gain`: refused for a start that is no source, an end that is no lane, a lane whose instrument takes no notes (an audio input: step 99c), the mix at either end, a node that is not there, one port of audio and one of events, a level, and a cable between the two that is there already), `RemoveEdge` of a cable of notes and `RemoveNode` of a source (with the cables of notes that start at it, each written
  down) or of a lane (with those that end at it). Every one is a step of the history ("add Keys", "add Seaboard", "draw a cable of notes", "remove a cable of notes", "remove Keys", "remove Seaboard") and is told as a snapshot (the edges of the graph change). Undo takes out what an add made (under the ids it had) and puts
  back what was taken out, a source at its place among the sources and its cables after it (`Inverse::Source`, `Inverse::EventCable`): it is refused when the node is there already or the session has a source of the kind (the keys, a device by that name), and a cable whose source or lane is not there any more (only a change made behind the controller's back
  can do that) is left out, with the rest of the undo done. A source is placed as any node is (`SetNodeUi`: the file's `[keys]` and `[[device]]` have a `ui`, and a source that is taken out and put back by undo is where it was).
- **What the controller tells the audio thread** (`app/sources.rs`). The engine has no cable of notes: it has, for each lane, the set of sources it hears (P4a), which the controller works out (`lane_sources`, from the model's cables and `DeviceWatcher::sources_of`, the open MIDI ports that a device's name stands for: `midi::matching`) and sends as `SetLaneSources` when it
  is not what the lane was told last (`sync_lane_sources`, with `sent_sources` by the lane's node; a lane that was never told is told only when the session has a cable of notes, so that a session with none sends nothing). It looks when the controller is made, when the devices are watched, at every look at the MIDI ports (`devices_changed`: a device that was plugged in or went changes
  what the lanes that are wired to it hear, and one that went has `ReleaseSource` sent first), and when an intent has changed the structure (`conclude`).
- **The oracle** has the scenario `edited:sources` (the keys and a device that is never plugged in are added, cables of notes are drawn and taken out while a chord, a pedal and a held key sound, a source is taken out and put back by Undo, and a lane is taken out with its cable and put back by Undo; `performance_with_a_late_key` is its script) and through the real engine the tests that a lane
  that is wired to a device lets go of the key it holds, and plays the next key when the cable is taken out.
- **The page** (P4c, step 83; `docs/web-ui.md` D83). *Which inputs are open.* The message `inputs { names }` (`ServerMessage::Inputs`) says which MIDI inputs the program has open, in the order it opened them (`Controller::inputs`, `DeviceWatcher::open_names`): it follows `history` when a client subscribes and is told again whenever the list changes (`Owner::refresh` compares it with what it told last), and is empty for a program with no MIDI subsystem;
  `ClientState.inputs` keeps it (replaced whole, so that a list that is the same changes nothing; kept while the connection is lost). The page has the controller's rule for which of them a device stands for (`client/inputs.ts`, `inputsStoodFor`: the name without the white space round it, as Rust's `trim` takes it and the page's own `trim` does not; the inputs called exactly that, else those that have it in their names, in any case; a blank name stands for none),
  pinned to `midi::matching` by `fixtures/inputs.json` (145 names that Rust writes, among them a next-line character, a byte order mark and a final sigma): it says of a device node that it is plugged in or not, and of an input that a device of the session has it already.
  *The canvas.* A source is a node of the kind `source` (`SourceNode`, `app/patch/nodes.tsx`): its name, a badge (`Keyboard` or `MIDI input`), a line (`Computer keys and the piano`, or `Plugged in: <the inputs it stands for>` or `Not plugged in`), an output of events on its right edge (`NotesPort`, the handle `eventsOut`), the menu `♫` "Send notes from X" (`NotesToMenu`, the id `notes-to-<node>`: the lanes that have no cable from it; shut with none)
  and the button "Remove X" (the focus goes to the bar's `add-source`). A lane has an input of events on the left edge of its instrument node (the handle `eventsIn`) and the menu `♫` "Take notes into X" (`NotesFromMenu`, `notes-from-<node>`: the sources that have no cable to it), both on the badge's line beside the menu that sends audio. `toFlow` puts the sources in a column of their own at `x = -COLUMN`, one under the other in the order of the graph a row's gap apart, level
  with the middle of the lanes as a whole and not above the top of the canvas (`SOURCE_HEIGHT`, 74, as a browser measures it); a source stays where it was while the canvas is not arranged (slot `source`), a new one goes below the ones that stayed, and a source that has a place (`ui`) is drawn there. A cable of notes is the edge `notes:<id>` (`--tang-notes`, violet, plain, over the links and under the audio cables, with the button that takes it out at its middle and no level: it has none) and the link by which
  a lane hears every source is the edge `hears:<from>><to>` (the same colour, faint and dotted, with nothing on it); drawing one takes the other's links off the lane, as the controller's graph does. A cable is dragged from an output of events to an input that takes it (`connection.ts`: `notesCableOf`, `notesTakersOf` and `canDrawNotes`, which are `client/noteWiring.ts`'s `noteTakers`, the controller's `event_cable_ends` and the check for a cable that is there already; `kindOfDrag` gives `data-connecting="notes"`, and the inputs that take it grow to a strip, the line is violet and plain;
  `drawCable` is what a drop asks of the editor, for the three kinds of cable). The bar's "+ Source" (`app/AddSource.tsx`, `client/noteItems.ts`: `sourceItems`) offers the keys while the session has none and each open input that no device of the session stands for (a second device for an input would put every note in twice); with nothing to offer it is shut. `Editor.addSource` and `addNotesCable` ask for them (`Could not add the keys as a source of notes: the keys are a source already`).
  *The mock engine* does what the controller does with them (the intents `addSource`, `addCable` with ports of events, `removeEdge` and `removeNode`, the links a lane that has no cable hears by: `client/noteWiring.ts`, `withHears`; the history's words), pinned by `fixtures/sources.json` (273 intents on a session of two sources, a group and three lanes, and on a session of two lanes: every start of a cable with every end, the names of devices, every intent that names a source as a node) and by four more scripts of `history.json`
  (the shape of a session now lists its sources, cables of notes, links and the order of the kinds of edge: `shapeOf`), and says that two inputs are open (`MOCK_INPUTS` in `app/engine.ts`, so that `?engine=mock` can add a device). It found a discrepancy in step 76's code (f177): the mock put the link of a lane that was added after the audio cables, where the controller has it among the links of the audio path. The browser tests are `e2e/sources.spec.ts` (8: on the mock and on the real program, which has no input open).
TODO: a source has no light for what it has sent lately (a count of events for each source in the telemetry); the bar's "+ Source" offers the open inputs only, and a device that is not plugged in (a session that names one) can be added by a name that is typed from a client that sends the intent, not from the page; the terminal shows no source (P4d: the Patch tab as a matrix of sources against lanes).
TODO: the keys are one source for every input of the computer and every browser; a source for a connection (a tablet's piano and a laptop's keys), or for the page's piano apart from the computer's keys, is a small change when it is wanted.

### The audio input (`builtin:input`, step 99)

The computer's microphone, or another input of the sound card, as an instrument that audio cables go from: the user's direction of 2026-10-28 ("for future (after this) I want to be able to add my computer microphone as an input that I can connect to other devices via audio cable.", and, when step 98b was done, "then lets go on the microphone"). It is built as **a built-in instrument in a lane that starts bare**, and not as a node of a new kind: the lane gives it what every lane has (a volume, a mute and a solo, a meter and a scope, effects in its chain, modulators on its parameters, a place in the history), the audio cables of P3 are what connect it, and so the model, the engine, the file and the intents have nothing new (`docs/web-ui.md` step 99 says why a node of its own waits for P5c, which dissolves the lane).

- **The plugin** (`src/plugin/builtin/input.rs`: `builtin:input`, "Audio Input"; an instrument with two audio outputs and none in, no presets and no latency of its own, which `tang describe builtin:input` says). Parameters: `gain` (0 to 4, linear, default 1), `pan` (a balance, −1 to 1: the middle leaves both sides as they are) and `device` (a choice: "Default input", and then each input that the system had when the plugin was made, by its place in that list; `tang enumerate audio` lists them). `gain` and `pan` are gone to over 5 ms (in 64 bits: a one-pole in 32 stops short of what it follows), and a value that was set before the first block is the first block's, with no glide from the defaults. A device of one channel is put in both sides, one of more channels has its first two, and a device that runs at another rate than the engine is resampled, linearly (`Resampler`: the user's webcam microphone is at 16 kHz).
- **It starts bare** (`Plugin::starts_bare`: false by default, true for the input). `Controller::place_instrument_in_new_lane`, which `AddInstrument` ends in, cuts the new lane's way out when the plugin says so (`route = "none"` in the model and `SetLaneRoute` to the engine, before the instrument is in, as `AddFreeEffect` makes its lane), so that **nothing of it is heard until a cable is drawn from it** to an effect, a group's bus or the mix: a microphone that went to the mix by itself would be heard through the speakers that it listens to, and howl. A cable from it to the mix is the way out given back (`link_or_cable`: "give the lane of Audio Input its way out", a link, with no `[[cable]]` of its own); one to an effect is an audio cable. There is no new intent: `AddInstrument { plugin: "builtin:input" }` adds it, from the page's "+ Instrument" menu and the terminal's selector. **`ReplaceInstrument` leaves the lane's way out as it was** (an input that is put in a lane that has its way out is heard at once: the lane is the user's), `ClearInstrument` leaves the lane as it is, and undo and redo keep bare what was bare; the file has `route = "none"` while the lane is bare, and a program that loads it has the lane bare (`an_audio_input_is_added_in_a_lane_whose_way_out_is_cut_…`, `an_audio_input_that_replaces_the_instrument_of_a_lane_leaves_the_way_out_as_it_was`).
- **Where the sound comes from.** A thread of its own (`tang-audio-input`, `Capture`) owns the cpal input stream (which is not `Send`) and fills a lock-free ring of 32,768 stereo frames (`InputRing`: one writer and one reader, a cell an atomic of its own, nothing allocated after it is made); the audio thread reads a block at a time (`RingReader::pull`). It waits for a backlog of two blocks (768 frames at least) before it begins, and again after a dropout, so that the clocks of two devices, which are never the same, do not make a crackle of every block; a backlog of more than 8,192 frames is skipped to the prime, so that the delay stays short (a clock that runs fast is a skipped stretch now and then and one that runs slow a dropout: a click either way, rare). **The microphone is not opened until the first block has been rendered** (`Capture::start`, sent by the plugin's first `process`: a catalog scan, `tang describe` and `tang enumerate` make the plugin and do not open it, so that macOS does not ask a person for a permission that nobody asked for), **and is closed when no block has been rendered for a second** (a heartbeat, `beats`: a lane that was taken out and is kept by the history, or an engine that is stopped, does not keep the microphone open) and opened again when blocks come back; one that cannot be opened, or whose stream fails, is tried again every 3 s (the log says why: `Audio input: …`). The thread's loop is `listen`, generic over what it opens and over its clock (`Timing`), and is held with a fake card and a clock in milliseconds (`nothing_is_opened_until_the_first_block_has_been_rendered_…`, `the_stream_is_closed_when_no_block_is_rendered_…`, `another_device_closes_the_stream_…`, `a_stream_that_says_it_has_failed_…`, `an_input_that_cannot_be_opened_…`, `the_thread_ends_and_the_stream_is_closed_…`). A change of `device` is told to the thread once, by name (`Command::Device`), and the stream is opened on the new one. **The host** is `audio::input_host()`: the system's default host, or JACK if `--jack` was given and it has an input device (the output's host: see "Audio host selection"); it sets nothing in the environment (`select_audio_host` does that, once, at startup).
- **No device (`--audio-device none`, the tests, CI): a tone.** There is no sound card to listen to, so the plugin plays a sine of 220 Hz at a level of 0.25 in both sides (`TONE_HZ`, `TONE_LEVEL`): in the tests (`cfg!(test)`) and under `--audio-device none` (`plugin::builtin::use_test_tone`, which `run_session` sets before any plugin is made: the TUI, `play` and `serve` alike). It is what the oracle's session and scenario, the real-program test and the browser spec hear and measure.
- **The terminal, `enumerate` and `listen`.** `tang enumerate audio` lists the inputs after the outputs (`=== Audio Input Devices ===`, the default marked `*`, with channels, rate and format; it opens nothing); `tang listen` opens one and says what it hears (see "Subcommands"). The instrument selector lists "Audio Input" first (the catalog is by name) and **starts on the instrument that the lane has**, as the preset selector starts on the preset that is loaded, so that `n` and Enter, and `i` and Enter, still keep the oscillator that a hand has always chosen (on a lane that has no instrument it starts at the first row): `the_instrument_selector_starts_on_the_instrument_that_the_lane_has_so_that_enter_keeps_it`. **In the terminal `n` adds an oscillator lane and the selector then replaces it, so an Audio Input chosen there is in a lane that has its way out and is heard at once** (`w` cuts it; the Patch tab has its row: `an_audio_input_chosen_in_the_selector_is_in_a_lane_that_has_its_way_out_and_w_cuts_it`).
- **The page.** "+ Instrument" (and "Change the instrument of X") lists "Audio Input" with the oscillator; the lane is a card with the input's three knobs (`gain`, `pan`, `device`), its volume, its group, its mute and solo, and no link out and (step 99c, below) no key range, no pattern and no input of events, and (step 100) its level along the top edge of the card, which reads the tone before any cable is drawn. The mock engine makes the lane bare as the controller does, from what its fixture says (`builtins.instruments.json` has `route: "none"` for the input, written by Rust: `the_web_clients_fixture_of_the_builtin_instruments_is_current`), pinned by a script of `history.json` (the input added, a cable to the mix that gives the way out back, a cable to an effect, the way out cut again, all of it undone and redone) and by a test of its own in `connection.test.ts`. `web/e2e/input.spec.ts` runs it on the mock (the lane is bare, its knobs, the cable gives the way out back, undo and redo keep it) and on the real program (silent until the cable, the tone at -12 dB through it, the gain, the mute, the file, a page opened again).
- **It takes no notes** (step 99c: the user, of the audio input's card on the page, "I dont understand why the audio input has keys and record atm though, maybe just leftovers?"). They were not leftovers but the lane's: a plugin that is in a lane has the lane's whole card, and a microphone has no notes to play. `Plugin::takes_notes` (true by default, false for `builtin:input`) is read when the plugin is loaded into `PluginSlot::takes_notes` (`fresh_slot` and the loader's instrument; it goes where the plugin goes, an undo that puts it back included) and told by the graph as `Plugin.takesNotes`, **left out of the wire for a plugin that takes notes** (no other fixture changed; the generated type is `takesNotes?: boolean`, by `#[ts(as = "Option<bool>", optional)]`; a client reads an absent word as true). `SessionModel::lane_takes_notes`: a lane takes notes unless its instrument says that it does not, and a lane that has none, which may be given one, does. For a lane that does not: the graph draws no link by which it hears a source (`hears_edges`), `event_cable_ends` refuses a cable of notes to it (`that instrument takes no notes: an audio input has no input of events`, with nothing changed and no id used) and so does the loader for a file (`cable[0].to: instrument[0] takes no notes: …`, with the cable's place), and the engine is as it was (the lane hears every source, and the input ignores events). **The page**: `laneTakesNotes(node)` (`client/noteWiring.ts`, the controller's rule read from the graph) is `PatchNodeData.notes`; `PluginNode` draws no keys (`RangeCell`), no pattern (`PatternCell`), no input of events (`NotesPort`) and no "Take notes into" menu (`NotesFromMenu`) for such a lane, and the layout counts what is drawn (`controlCount`: three controls beside the knobs and not four; `nodeHeight`: no `PATTERN_HEIGHT`); `eventCableEnds`, `noteTakers` and `withHears` follow the same rule, which is how the mock engine and the menus of the sources have it. What is the lane's and not about notes stays on the card: its group, its mute and solo, its volume and its audio out. Held by five Rust tests (`an_audio_input_takes_no_notes_and_every_other_plugin_does`, `a_lane_whose_instrument_takes_no_notes_has_no_link_by_which_it_hears_a_source_and_is_refused_a_cable_of_notes_with_nothing_changed_and_no_id_used`, `a_lane_takes_notes_as_its_instrument_does_and_what_undo_gives_back_says_what_it_said`, `a_cable_of_notes_to_a_lane_whose_instrument_takes_no_notes_refuses_the_session_with_the_place_of_the_cable_and_the_lane_that_has_none_takes_one`, `the_graph_says_of_an_audio_input_that_it_takes_no_notes_and_of_the_oscillator_that_it_does_and_the_wire_has_a_word_of_it_for_the_input_only`), ten of the page (`noteWiring.test.ts` 4, `toFlow.test.ts` 2, `PatchView.sources.test.tsx` 4) and `web/e2e/input.spec.ts`, on the mock and on the program. Rust 2485 tests (2671 with the `web` feature, 2479 without the default plugin formats), web 2882 in 122 files, browsers 201 (the whole suite was run after this step). **Held by mutants** (step 99c): 36 mutants of the logic of the step, one at a time against the unit tests that should catch them, **all caught** (none only by the build, no gap to close): 20 of the Rust (the trait's default and the input's word 2, what `fresh_slot` and the loader read from the plugin and the slot's default 4, `lane_takes_notes` 2, the controller's guard in `event_cable_ends` 4, the loader's guard 2, the graph's links by which a lane hears a source 2, the graph's word and the wire's: that it is left out for a plugin that takes notes, read as true when it is absent 4) and 16 of the page (`laneTakesNotes` 5, the guard of `eventCableEnds` 1, `noteTakers` 1, `withHears` 1, `controlCount` 2, `nodeHeight` 1, `PatchNodeData.notes` 1 and the four gates of `PluginNode`: the input of events, the menu, the keys and the pattern 4).
- **The oracle** has a session (`input`: an audio input in a bare lane, with a gain of 1.5 and a balance of −0.4, an oscillator, and a lane of its own with a reverb, the input's cables going to the reverb and to the mix) and a scenario (`edited:input`, `INPUT_EDITS`, 14 edits: an audio input is added in a lane of its own, which is bare, two cables are drawn from it, its gain, balance, mute, solo and volume are turned, a lane is taken out with its cables, and Undo, Redo and Undo go through it): six renders, **1029 in all, and the 1023 that were there are the same bits** (`Scenario::has_cut_routes` counts a scenario that adds an input: the loop that the plan replaced cannot run a bare lane). The stored hashes are written by `TANG_UPDATE_ORACLE=1 cargo test --release oracle::tests::the_whole_corpus_is_what_it_was_and_the_same_twice -- --ignored --exact`.
- **Held by tests.** Rust: 24 in `input.rs` (the ring and the reader's prime, dropout, skip and boundaries, the resampler, the channels, the tone, the smoothing in numbers and before the first block, the balance, the parameters and their limits, the commands to the thread and the thread's loop with a fake card), the controller's two (`apply.rs`), the terminal's two (`golden_tests.rs`), two of `tests/cli.rs` and the one of `tests/serve.rs` that runs the real program on the null backend (silent while bare, the tone through the cable, the gain, the file, a program started on the file). Mutants: 86 of the Rust logic of the step, in two runs, one at a time against the unit tests that should catch them (the ring 5, the reader's prime, skip, limits and dropouts 15, the resampler 7, the channels 3, the thread's commands 3 and its loop 16, the plugin 31, the controller's bare lane 4, the trait's default and the registry 2), 4 of the page (the mock's bare lane) and 5 of the terminal's selector (where it starts). The first run, of 82, was stopped at 28 (one mutant made a loop that never ends and grows a list without end, and another would have made a test wait for ever: the tests' harness fails now where it waited) and had three survivors, and the second, of 87, caught 83 and let four survive: **six gaps, each a test now** (a reader made on a ring that has frames in it begins at the end of them; a block larger than 384 frames waits for two of its own; the backlog at the limit is not skipped; a change of device is opened at once and does not wait for the retry time; a balance that is turned after the first block is gone to; the place of the device is rounded and not cut) and **one in the tests' own harness** (a guard that fails a thread that does not end, out of the list); the six again are caught, and so are the page's four and the selector's five (a first run of those five was void: four golden tests failed at baseline, which made every mutant "caught"; the baseline passes now).
- **A silent input, and what the system does** (the user's first try, 2026-10-28: "hmm I cant get the microphone to work yet"). The stream opened, 48,000 frames a second arrived, **and every sample was exactly 0**: that is how macOS answers a program that may not use the microphone (no error, no prompt once a refusal is on record), and it is the permission of the *responsible* app, the terminal that runs Tang, and not Tang's own (the same from the test program, which has no embedded `Info.plist`, so the plist is not it). The system's record of the request (`log show --last 3m --predicate 'subsystem == "com.apple.TCC" AND eventMessage CONTAINS "AUTHREQ_"'`: `AUTHREQ_ATTRIBUTION` names the responsible app and `AUTHREQ_RESULT` the answer) said `responsible=dev.warp.Warp-Stable` and `authValue=0, authReason=2`: denied, by a user's choice. The fix is the user's (System Settings > Privacy & Security > Microphone, or `tccutil reset Microphone <id>` and Allow): **Tang does not change a person's privacy settings**. What Tang does: `tang listen` says it in words, and an open input that gives exact zeros for 3 s says so once in the log (`SilenceWatch`, `silence_advice`: `Audio input: "…" has given nothing but silence (exact zeros) for 3 s: on macOS, the terminal that runs Tang (…) may not be allowed to use the microphone …`; the terminal's own logs are off while it runs: `tang 2> log`). Held by `an_input_that_gives_only_zeros_for_the_limit_is_said_so_once_and_the_first_sound_sets_the_watch_again`, `the_advice_for_a_silent_input_names_the_terminal_and_the_command_that_asks_again`, `a_level_is_said_in_decibels_below_full_scale_and_none_is_silence` and, in `tests/cli.rs`, `listen_to_an_input_that_is_not_there_says_so_and_ends_with_status_1_having_opened_nothing`; 12 mutants of this logic, all caught (one only by the build). **It works** (2026-10-28, after the user had allowed Warp in System Settings and restarted it: "ok we restared", "restarted"): `tang listen` read the MacBook Pro Microphone at 48,128 frames a second with a peak of about -39 dB (a quiet room), and the user's word was "ok now it works!".
- **Decisions** (D99 of `docs/web-ui.md`; the user may overrule them). (1) *A plugin in a lane that starts bare, and not a node of a new kind*: it is the whole of the feature with the engine, the model and the file as they are; a node of its own comes with P5c. (2) *Heard only through a cable*: the answer to the feedback of a microphone that is heard through the speakers that it listens to. (3) *The system's default input, or one chosen by its place in the list of the system's inputs* (a session that is opened on another machine has what is at that place there: a name in the file would be portable); no flag beside `--audio-device`. (4) *One channel is both sides, the engine's rate with a linear resampler, a tone on the null backend.* (5) *The microphone opens when the first block is rendered and closes when blocks stop; it stays open for a lane that is muted or bare* (the lane's plugin renders either way).
TODO: a device by its name in the file, so that a session names the microphone and not its place in a list; the page and the terminal say nothing of an input that cannot be opened or that gives only silence (no device, no permission: the log has it, and `tang listen` says it) or of the device it is on (the page has the level of the lane since step 100: a level that stays at the floor says that nothing arrives; the terminal has none); the microphone is open while the lane exists and blocks are rendered, whatever the lane's mute or cut way out (the same TODO as for a muted lane that still renders: one that has been silent for a while could close it); the input's own latency (the ring's backlog, about 16 ms at 48 kHz, and the device's buffer) is not added to the paths by the delay compensation (`latency()` is 0); the resampler is linear (it images at 16 kHz) and a clock that runs faster or slower than the engine's skips or drops out now and then (an adaptive resampler would not); the terminal's `n` and then a choice puts an input in a lane that has its way out; a node of its own for the input (its card has no keys, pattern or input of events since step 99c, and still the lane's group cell) is P5c; Tang as a plugin (a DAW's buffer as the source) is still the idea of "Future ideas".
TODO: the terminal has nothing of it to hide (a lane shows a range and a pattern only when it has them), but `R` and `r` still work on the lane of an audio input and its action bar still says so, and the ear's menu (`♫`, `h`) still lists the lane as one whose notes a modulator can hear (the controller takes it): they could refuse or leave it out.

### MIDI routing

- Note-on/note-off: filtered by instrument's key range (inclusive)
- Instrument with no range: receives all notes (full range)
- Overlapping ranges: notes go to all matching instruments
- **Per-note expression** (CC, pitch bend, channel pressure, poly aftertouch):
  routed by MIDI channel following the MPE convention. **Channel 1** (the MPE
  master / non-MPE global channel) is duplicated to all instruments, so an
  ordinary mod wheel / sustain / pitch wheel affects everything as before.
  **Member channels 2–16** carry per-note MPE expression and reach only the
  lane currently sounding a note on that channel — so on an MPE controller
  (e.g. a ROLI Seaboard) the pressure/glide of a note played in one
  instrument's zone can't bleed into another instrument's modulators. Each lane
  tracks its active note channels (`InstrumentLane.active_note_channels`) from
  range-filtered note on/off; the tally resets when the lane's range changes.
  **Limitation:** modulators are block-rate and lane-global, not per-voice, so
  a *chord* on one MPE instrument still collapses its notes' per-note expression
  to a single value (last channel wins) — true per-voice expression would need
  voice-level modulation.
- **What the built-in oscillator does with them** (see "Built-in plugins"): the pitch wheel of the master channel bends every note and that of a member channel the
  notes that began on it (so its per-note expression is per voice, which the limitation above is not), the sustain pedal holds notes, controllers 123 and 120 let go of
  them, and the velocity of a note is its level as far as `velocity` says. The other plugins do what they do.
- **Sources** (P4a of `docs/patch-graph.md` §11.6, step 81): every message that reaches the audio thread has a *source*, the input it came from (`Source`, `src/source.rs`, a number below 64).
  **0 is the keys** (the computer's keys, the page's piano and keys and every browser's: one `NoteInput` path, which `NoteInput::new` is), **each MIDI device** has a number of its own
  (`NoteInput::for_source`, made when its port is opened), handed out in the order the devices are first seen and kept for the run (`midi::SourceTable`: a device that is unplugged and plugged in
  again is the source it was; the 63rd and the ones after it share the last, `Source::LAST`), and **`Source::BROADCAST`** is no source's: what a panic is sent as, so that every lane hears it whatever it
  hears. The channel carries `MidiEvent { source, frame, bytes }`, and the audio callback (`audio::Renderer`, which has room for as many as the channel holds, `MIDI_CHANNEL_CAPACITY`, 1,024, so that a
  burst asks the allocator for nothing) hands the engine the block's events as `Events`: the frame and the bytes of each, as the plugins have them, and beside them the source of each (none at all says
  that every event is from the keys, which is what `AudioGraph::process` gives for a list of notes, so that the plugin trait and the tests that play a chord are as they were; the engine's own is
  `process_events`). A lane has **`sources`**, the set of sources it hears (`SourceSet`, a `u64`: every source until `GraphCommand::SetLaneSources { inst, sources }` says another), and the first step
  of `InstrumentLane::filter_midi` drops the events of the others, before the range; a modulator that hears a group hears the events whose source some member hears (`build_group_midi`: a note also by the member's
  range, and a group with no member hears what is no note, as it did). **No hanging notes:** a lane keeps, for each source, what that source has sounding in it as far as it has passed it on
  (`Sounding`: the notes that are held by channel, the channels whose sustain pedal is down, the channels whose pitch wheel is away from its centre), and when it stops hearing a source
  (`SetLaneSources` without it) or is told that the source is gone (`GraphCommand::ReleaseSource`: a device that was unplugged, whose keys would be held for ever) it lets go of what it holds: the
  note-offs of its notes, then the pedal going up and the wheel going back to its centre, first among the events that the remap, the pattern, the transpose and the modulators see (so that a pattern that
  a held key plays stops, and a control that learned a key lets go), at most 32 a block (`RELEASES_PER_BLOCK`; what is left goes in the next, and nothing allocates); a source that is heard again before
  it is let go of keeps what it holds, and a lane goes on hearing a source that is gone, so that the device is heard when it is plugged in again. `ReleaseSource` is sent by the controller for a device that the MIDI manager says has gone (see the paragraph on hot-pluggable MIDI
  devices under "Architecture"), and so is `SetLaneSources`, from the cables of notes that a session has (P4b, step 82: see "Sources of notes and cables of notes"): a session that has none sends nothing, so that every lane hears every source and the sound is what it was, bit for bit (the oracle's corpus renders the same).
TODO: the held-notes lights (`HeldNotes`, the Piano tab's) are one set of bits for every source: the notes that a device that has gone was holding are let go of by the lanes and stay lit until the panic (a bit
cannot say which source set it).

## Virtual piano

The Piano tab turns the computer keyboard into a MIDI controller using Amiga tracker key layout.
Default base octave is 4 (lower row starts at C3, MIDI note 48). Fixed velocity: 100.

Lower row (base octave):
```
Key:  Z  S  X  D  C  V  G  B  H  N  J  M  ,  L  .  ;  /
Note: C  C# D  D# E  F  F# G  G# A  A# B  C  C# D  D# E
```

Upper row (base octave + 1):
```
Key:  Q  2  W  3  E  R  5  T  6  Y  7  U  I  9  O  0  P
Note: C  C# D  D# E  F  F# G  G# A  A# B  C  C# D  D# E
```

Notes sound on key press and stop on key release. A key ends the note it began, at the octave
it began in, even if `[` or `]` was pressed while it was held; a key that is not down (no
press, or one that Locked mode refused) ends nothing when it goes up.

The browser page plays the same layout (`tang serve`, `tang --web`): `web/src/client/keyboard.ts`,
which `web/src/app/useKeyboardPiano.ts` feeds with the window's keys, so a key pressed with the
focus anywhere but in a field that takes text is the piano's. Keys are told by where they are
(`KeyboardEvent.code`), so a layout that is not QWERTY keeps the shape of the piano; `[` and `]`
change the octave (0 to 8) and the page's header says `Keys: octave 4`; a key with Control,
Command or Alt is the browser's. A note ends with the key that began it, at the octave it began
in, and what is held is let go when the window loses the focus or the page is hidden (the server
ends a closed connection's notes too). The notes go to the server as `noteOn` and `noteOff`: the
one note path, so the Piano tab's Locked mode applies to them. The page also draws a piano under its
canvas (step 24; `web/src/app/piano/`): its keys are played with the pointer (`client/pointerPiano.ts`:
a glissando, a chord for several pointers; the lower on a key the pointer is, the harder the note, a velocity
from 20 at the key's top edge to 127 at its front edge (`velocityAt`; step 52), where the keys of the computer
play at 100: what a velocity control follows), a note that a key and the pointer both hold ends when both
let go (`client/holds.ts`, the provider's `useNotes`), and the keys light for the notes the server says
are held, whoever holds them (the `held` telemetry channel). A **Panic** button at the right end of the bar above the keys
(`PianoBar`, step 65) lets go of every note that sounds, the server's and not only the page's (`Client.panic`, the message `panic`; see "Built-in plugins").

## Audio host selection

Tang uses cpal for audio output.

| Platform | Default | Opt-in | Notes |
|----------|---------|--------|-------|
| Linux | ALSA (served by PipeWire) | JACK (`--jack`, via PipeWire) | See below |
| macOS | CoreAudio | — | Native |
| Windows | WASAPI | — | Native |

On Linux both hosts go through PipeWire; they differ in how Tang joins its
graph:

- **ALSA (default).** ALSA's `default` device is PipeWire's ALSA plugin, so
  Tang is an ordinary stream with its own buffer, like Spotify or YouTube. If
  Tang misses a deadline (heavy plugin, CPU spike), only Tang glitches, and
  WirePlumber moves it to the new default device when headphones connect or
  disconnect. The cost is that buffer: roughly one extra `--buffer-size` of
  latency.
- **JACK (`--jack`).** Tang runs inside PipeWire's graph cycle, so there is
  no extra buffer, but PipeWire waits for Tang every cycle: a late Tang cycle
  glitches every app (YouTube, Teams, …). Requires PipeWire's JACK bridge,
  either via `pw-jack tang` or the system-wide `ld.so.conf` redirect;
  `--jack` errors out if it is missing:

  ```bash
  sudo cp /usr/share/doc/pipewire/examples/ld.so.conf.d/pipewire-jack-x86_64-linux-gnu.conf /etc/ld.so.conf.d/
  sudo ldconfig
  ```

Both set `PIPEWIRE_LATENCY=<buffer-size>/<sample-rate>`, a per-client hint
(PipeWire runs the global quantum at the smallest latency any client asks
for). Without it, the ALSA route requests cpal's period size, a quarter of
the buffer (cpal splits an ALSA buffer into four periods), which would drag
the whole system down to a 256-frame quantum at the default buffer size.

**Following the default sink (JACK).** cpal links Tang's output ports to
`system:playback_*` once at stream start. PipeWire's JACK bridge resolves that
alias to the current default sink, but the links are fixed, and WirePlumber
does not move JACK clients to a new default. So with `--jack` and no
`--audio-device`, a watcher thread (`follow_default_sink` in `src/audio.rs`)
re-resolves `system:playback_*` about once a second through its own JACK
client and relinks Tang's ports when the default changes. It only removes the
links it made, so extra manual links (qpwgraph) survive.

TODO: follow the default device on macOS/Windows (cpal binds the stream to one
device, so this would mean rebuilding the stream).

The audio input (step 99) opens its stream on `audio::input_host()`: the output's host, which is the system's default host (ALSA on Linux) unless `--jack` was given (`audio::use_jack`, which
`run_session` calls once at startup, before any plugin is made: the input is a plugin, made far from the command line), and then JACK if it has an input device. Unlike `select_audio_host` it sets nothing in
the environment. `tang enumerate audio` lists the inputs of that host and the outputs of the default host, which are the same host (`enumerate` takes no `--jack`).

On Linux, the audio callback thread is promoted to `SCHED_FIFO` real-time
scheduling (priority 50) on its first invocation. This requires the user to
have `rtprio` privileges (typically via the `audio` group). Fails silently
if privileges are missing. macOS and Windows handle real-time audio thread
scheduling natively.

**Denormals.** `audio::Renderer::render` has the floating point unit flush denormals to
zero for the length of each block (`NoDenormals`, a guard: FTZ and DAZ in the MXCSR on
x86_64, FZ in the FPCR on aarch64, nothing on another processor; the state it found is put
back when the block is done): a number nearer to zero than the smallest normal 32-bit float
(1.2e-38), in what a filter, a reverb or an envelope computes or what it computes with, is
zero. The tails of these run down through the denormals (more than half the samples of the
two seconds after a note through a filter that is shut were denormal, f149) and arithmetic
on them takes many times as long on some processors (a hundred times on x86; not measured
here). It covers the plugins' own code as well as the built-in ones, and it is a guard of
the block and not a setting made once on a thread, since a host's callback is not always on a
thread of its own and a plugin may change the unit. The null backend and the sound card's
callback render through the same `Renderer::render`, so both have it.

The flush does not end a tail, and a tail that does not end is not silence. A state that moves
by a product of its own distance to something (`s += g * (x - s)`, the integrators of the
filter) stops where that product is flushed, near the smallest normal number over `g`, and
stays there: after a note the built-in filter put out a constant of 2e-38 to 5e-35 (by the
type and the cutoff; the bandpass and the notch, and the ladder and the MS-20 at cutoff 0.9,
ran down to zero), the same for the 70 seconds it was watched, so that a scope never read
exactly zero (f160: the browser test of the scope failed four runs of twelve on it, a side
effect of f149's flush: through the tracer's filter, with the flush 12 of 15 places of the LFO
at which a key is let go left a tail that did not end in 8 seconds, and without it none did;
a filter at a constant cutoff had stalled without it too, at the denormal numbers, 1e-45 to
3e-42, for the 30 seconds it was watched). The filter lets go of each of its states that is
under `TAIL_FLOOR` (1e-20, −400 dB) at the end of every block, so that a tail runs down by its
own arithmetic, far above the numbers it stops at, and then is zero
(`a_note_through_a_filter_of_each_type_and_cutoff_ends_in_silence_after_it_is_let_go`). The
built-in delay and reverb, at their defaults, ran down to zero within 100 seconds (watched
for ten minutes); a plugin of another make has what it has.

**Null backend.** `--audio-device none` (`audio::NULL_DEVICE`) opens no host and
no device: a thread named `tang-null-audio` renders a block every
`buffer / sample rate` seconds (5.3 ms at 256 frames and 48 kHz) with the same
code the real callback runs (`audio::Renderer`) and throws the samples away. MIDI,
the meters and the telemetry behave as with a sound card, so CI, headless runs
(`tang serve`, later) and tests can drive and measure the engine, and a loud
session can be tried without making a sound. The cadence is kept without drift,
and a stall is not caught up with a burst of blocks. `stop` hands the plugins back
to the stopping thread to drop, which the real stream does not promise. It runs
at normal priority: no real-time promotion.
