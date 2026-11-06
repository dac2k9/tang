# The web page

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): the browser front-end: the protocol, the server, every step of the page. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

## Web UI

A browser front-end to the same Tang process (local or LAN). Audio, plugins
and MIDI stay native — the browser is a control surface, never an audio
engine — and the TUI stays first-class. Full design, protocol sketch and the
phase 0 step list: [`docs/web-ui.md`](docs/web-ui.md). Summary:

- **Controller** (`src/app/`): a plain struct that owns the live session
  model, the engine handles and the plugin catalog. Front-ends call
  `apply(Intent)` and receive `Event`s. The TUI is migrated onto it feature by
  feature, so no feature ever has two implementations. Built so far: `SetParam`,
  `SetGain`, `SetLimiter`, `LoadPreset`, `SetModulatorField`, `SetSteps`, `SetEdgeDepth`, `SetRange`, `SetMute`, `SetSolo`, `SetRoute`, `SetModulatorEar`, `SetFeed`, `RemoveEdge`,
  `AddFreeModulator`, `AddModEdge`, `Modulate`, `AddCable`, `AddSource`, `SetCableGain`, `AddEffect`, `AddFreeEffect`, `AddInstrument`, `ReplaceInstrument`, `ClearInstrument`,
  `SetNodeUi`, `SetPinned`, `SetBpm`, `SetRecording`, `ClearPattern`, `SetPatternField`, `MoveEffect`, `AddGroup`, `SetLaneGroup`, `RenameGroup`, `RenameMacro`, `ExposeMacro`, `RemoveNode` (a modulator, an effect, a lane
  or a group), `Save`, `SaveAs`, `SaveModule`, `SaveSelectionAsModule`, `CollapseIntoModule`, `UngroupModule`, `Undo` and `Redo` (see "Session state"). Effects are
  added by `Controller::add_effect` / `add_bus_effect` (loaded here, from the plugin
  catalog only, at the end of a lane's chain or a group's bus; the TUI's selector asks
  them), which the intent `AddEffect` wraps (browsers choose from the catalog the server
  tells them; the mock engine's nodes for the built-in effects are a fixture Rust writes,
  `web/src/client/fixtures/builtins.effects.json`). Instruments are put in by
  `Controller::put_instrument` (a lane's, which the TUI's `i` asks) and `add_instrument_lane`
  (a new lane), from the catalog only, which `ReplaceInstrument` and `AddInstrument` wrap (`ClearInstrument` takes one out again)
  (`builtins.instruments.json` is the mock engine's node for each built-in instrument).
  A preset is loaded by `LoadPreset` (the instrument of a lane, or an effect of a lane's chain; a
  bus effect is refused, since the audio thread's command addresses a lane's slot), which also
  centres the cables that drive the plugin on the preset's values; the mock engine answers with
  what the controller says each built-in preset does (`builtins.presets.json`, which Rust writes).
- **Model**: graph-shaped (nodes, ports, edges, layout) with stable `NodeId`s
  (and `EdgeId`s for modulation edges); today's lane/group/modulator structure
  is projected into it: `SessionModel::graph()` (`src/app/graph.rs`) gives the
  nodes (instrument, effect, modulator, group, the master as `NodeId::MASTER`),
  their `parent`s and the edges between ports (`audio`, `member`, `modulation`
  with the target's depth as `amount`), and `Controller::snapshot()`
  (`src/app/snapshot.rs`) is that plus the revision, the telemetry layout and
  what belongs to the session as a whole (dirty, file name, tempo, piano). A lane's
  pattern is on its node (`NodeKind::Instrument.pattern`, a `PatternInfo`, or null while it has none). Node
  layout is stored in the session TOML as optional `ui = { x, y }` tables that the
  engine and the TUI ignore (`Node.ui` in the graph; see "Node positions (`ui`)"), and so is which of a plugin's parameters its node
  shows as knobs (`Plugin.pinned` in the graph; see "Pinned parameters (`pinned`)").
- **Telemetry**: the audio thread publishes modulator outputs, effective
  (modulated) parameter values and meters through lock-free slots; the web
  runtime streams them to the browser as binary frames. The slots, the engine
  side and the layout that says what each slot means
  (`SessionModel::telemetry_layout()`, `src/app/layout.rs`) are built (see
  "Session state"); `web::pusher` sends them to the browsers as binary frames (see "Server").
- **Protocol**: the types that cross the socket (`Intent`, `Event`,
  `StampedEvent`, `Graph`, `TelemetryLayout`, `Snapshot`) derive serde — camel
  case, enums as objects tagged by `type` or `kind` — and, under the `web`
  feature, ts-rs. The TypeScript in `web/src/protocol/generated/` is committed;
  a test (`app::protocol::typescript`, run by the CI job `typescript`) fails
  when it is not what the types generate. After changing a wire type:
  `TANG_UPDATE_TYPES=1 cargo test --features web typescript`, and commit the
  result. The tracer scene (`web/src/client/fixtures/tracer.toml`: the tests' own
  copy of what `examples/tracer.toml` was, since a person who runs the program on
  the example and saves writes over it) has a golden JSON snapshot
  (`src/app/snapshots/`); review the diff when it changes
  (`INSTA_UPDATE=always cargo test the_tracer`).
- **Server** (`src/web/`, built; step 10): `axum` on its own thread behind the
  `web` Cargo feature, started by `tang serve` or by `tang --web`. Routes `/` (the
  embedded interface) and `/ws`; one WebSocket per client with a
  bounded outgoing queue (a client that falls behind is told and closed, and
  reconnects from a snapshot); loopback by default, `Origin`/`Host` checked on every
  request, a token for non-loopback binds, limits on message size, request rate,
  connections and write time. The audio, MIDI and controller threads stay
  synchronous. Messages: `app::protocol` (`ClientMessage`, `ServerMessage`); the
  plugin catalog (what can be added; step 19) follows a client's snapshot and is told again
  as the scan finds more (`catalog { rev, scanning, entries }`: ids and names, never where a
  plugin is installed; `tang serve` starts the scan, the TUI its own); what Undo and Redo would do
  follows the catalog (`history { undo, redo }`, step 32: the words of a button or null, told again when they change, before the
  answer to the request that changed them); which MIDI inputs are open follows the history (`inputs { names }`, step 83: the names in the order they were opened, told again when they change); the
  telemetry frames are made by `web::pusher` at 30 Hz for the channels a client
  asked for (`meters`, `modulation`, `held`: a value for each of the 128 notes, 1 for a note that
  is held, from the same bits the Piano tab reads, and `scope`: the latest 2048 frames of the output mix, the left side and then the right, 16 KB a frame, made only while a client asks for it; and, from step 46, the signal at a node:
  a client says which nodes it wants with `probes { nodes }`, the whole list each time and at most 64, is answered with `probes { nodes }` (those that have a signal and a ring) and is sent a frame for each of them on channel 4, with the node's id where
  the layout's revision is, 2 × 2048 values, D47). `NoteInput` (`src/note_input.rs`) is the one note path of hardware
  MIDI, the virtual piano and the browser: the Piano tab's filter, the held-notes
  bits and the channel to the audio thread, with the notes a WebSocket holds ended
  when it closes, and the panic (`NoteInput::panic`, the message `panic`: step 65). Tests: `web::` (in-process, with a real WebSocket client) and
  `tests/serve.rs` (the real binary on the null backend). See `docs/web-ui.md`
  §3.2, §7 and §8 for the as-built notes.
- **Frontend** (`web/`, built; steps 11 to 16 and 18 to 44): TypeScript (strictest settings) +
  React + Vite, a framework-agnostic client layer (`web/src/client/`: reducers,
  telemetry buffers and slot readers, the connection with its reconnection and token, a
  vanilla zustand store, the frame loop, `Drive` (what a control shows while the server
  catches up, one intent a frame), the engine's modulation laws for the knobs' rings, and
  the mock engine, which speaks the protocol in the page), generated protocol types,
  design tokens as CSS variables (`web/src/ui/tokens.css`), the page's own rules (`web/src/ui/global.css`: the page is not selectable text, in both spellings of `user-select`, except what takes text and what is shown to be copied: see "A drag on a knob painted a gray box" in `docs/reference/plugin-host.md`), the controls
  (`web/src/ui/`: `Knob`, `Choice`, `LiveBar`, `LiveTrace`, `LiveEnvelope`, `Meter`, which draw live values by
  writing to the DOM or a canvas, never through React state) and the patch canvas
  (`web/src/app/patch/`: `toFlow`, a pure layout of the graph, drawn with React Flow:
  a strip for each instrument, its modulators above it, cables, a node's knobs bound to
  the session; step 14 added the controls of a modulator: an LFO's rate and waveform and an
  envelope's stages on the node (`Knob scale="log"` for a rate and a time, which run over
  decades), and a slider for the depth of each cable at its middle; step 15 added building:
  a "+" menu on every lane and group (`MenuButton`) that adds an LFO, an envelope or a MIDI
  control, a "×" on every modulator, every effect and every cable, a list at the foot of a modulator that draws
  a cable to any parameter it can drive (for a keyboard: a `MenuButton` with the plugins as
  headings, which finds an item by its first letters), and a cable dragged from a
  modulator's output to a parameter's port, the ports that take it marked while it is
  dragged; `client/editor.ts` is where the page asks for them and `Notices` /
  `NoticeBoard` is where it says what the server refused; where a cable may end is the
  controller's rule, read from the graph by `client/reach.ts` and pinned to the controller's by
  a fixture it writes, `web/src/client/fixtures/reach.json`). Step 16 made the page's keys a
  piano (`client/keyboard.ts`, `app/useKeyboardPiano.ts`; see "Virtual piano"). Step 18 made the
  page save the session (`app/SessionBar.tsx`: the file's name, "Saved" or "Unsaved changes", a Save
  button and Control+S / Command+S, which keep the browser from offering to save the page; `Editor.save()`). Step 49 put the
  tempo in that bar (`app/Tempo.tsx`, a field of beats a minute that is a `spinbutton`: what is typed is asked for as `SetBpm` with Enter or on leaving the field,
  Escape gives it up, and the arrow keys turn it by one, by ten with Shift, each step asked as it is made, so that a held key is one step of the history; the server
  holds it to 20–300 and the field shows what it was held to; `Editor.setBpm`; the field is the page's `ui/NumberField`, which the length of a pattern uses too). Step 51 put
  the pattern of a lane in the page: a block under the controls of every lane, an empty lane too (`app/patch/PatternCell.tsx`: a button that records, `Editor.setRecording`,
  red and "Stop" while a recording goes on; a menu that clears the pattern, `Editor.clearPattern`, which asks first, in words that say it is for good; the length, a
  `NumberField` of 1 to 32 beats, `Editor.setPatternField`; the switches Play, Loop and In key; and a line, `client/patternWords.ts`: "4 beats · 3 notes · 90 BPM", the tempo
  it was recorded at, or "Counting in, then 4 beats at 120 BPM: play now" while it records). The block is as tall whatever it shows (`PATTERN_HEIGHT`, 97 px measured in a
  browser, which the layout adds to every lane's node), and a lane that has no pattern has it too with what has no use dimmed (its length is the one its first recording
  takes, `PATTERN_DEFAULT_LENGTH`). The mock engine does what the engine does with a recording (`MockServer.settle`, a timer that looks at the recordings every 50 ms while
  there are any: it counts in, takes the length of the pattern at the tempo it began with, counts the notes of the lane's keys that are played, and when it is done the lane has
  the pattern and the clients are given the session, as the controller does when it polls). Step 52 added the controls that follow the notes a lane plays (see "MIDI-learn
  modulator"): a velocity and a key, made from a lane's or a group's "+" menu and from a knob's "~" menu (`client/modulatorKinds.ts` has the words of every kind and tells a source's
  kind back), shown as MIDI controls (`velocity of the last note`, `pitch of the last note`), their rings by the laws (a key is bipolar around middle C: `KEY_CENTRE`, `KEY_SPAN` are
  generated constants), and followed by the mock engine as the engine follows the notes (`MockServer.#listened`: a lane's hear the notes in its keys as its transpose makes them, a
  group's the notes of its members' keys as they are played; no remap, no patterns). The piano under the canvas plays a pointer harder lower on a key (20 to 127, `velocityAt`).
  Step 53 gave every lane and group a mute and a solo (see "Mute and solo"): a "Mute / Solo" cell last in the node's grid (`MuteSoloCell`: two buttons, `M` and `S`, `aria-pressed`, named
  `Mute Oscillator` and `Solo Pad`; a press asks `Editor.setMute` or `setSolo` for the other way and the buttons show what the session has, nothing is changed before the answer, and the title
  says what a press does), a node that cannot be heard drawn dimmed (`tang-node--unheard`, `opacity: 0.55` and not brighter under the pointer: a lane or a group that is muted, or not soloed
  while something is, and the effects in the chain of one; `audibleAt` over `audibleOf` in `client/audible.ts`, which is the engine's law written again and pinned to a run of the engine over every
  combination of the flags of two groups and four lanes by `fixtures/audible.json`, 4,096 cases), and the mock engine's meters and scope silent for what is not heard (`MockServer.frame`; no fade).
  The client's store also keeps the server's plugin catalog (`ClientState.catalog`, step 19), and
  every lane and group has an "fx" menu that puts an installed effect in its chain (`client/catalog.ts`,
  `AddEffectMenu`; `Editor.addEffect`). Step 20 added the instruments: every lane has a "♪" menu that
  puts another instrument in it (`ChangeInstrumentMenu`; `Editor.replaceInstrument`) and the bar a
  "+ Instrument" menu that adds a lane with one (`app/AddInstrument.tsx`; `Editor.addInstrument`); the
  canvas moves to a node the page has added when it is not all in the view (`client/reveals.ts`,
  `app/patch/view.ts`), and not for a node another front-end adds. Step 21 added the presets: every
  instrument and every effect of a lane whose plugin has some has a "≡" menu that loads one
  (`PresetMenu`, `presetsOf`; `Editor.loadPreset`), the one loaded last marked and the menu opening on it
  (`MenuItem.current`). Step 22 added all the parameters of a plugin (a node shows the first eight as knobs):
  a "⚙" button on every instrument and effect docks a panel beside the canvas with a filter by name and a
  page of forty rows at a time, turned like the knobs (`ParamPanel`, `ParamRow`, `client/paramSearch.ts`;
  Escape or the button again closes it). Step 23 added the keys of a lane: every lane has a "Keys" cell
  with a lowest and a highest key (or `any` at an open end) that asks `SetRange` (`RangeCell`; the notes are
  named as in the TUI, `client/notes.ts`). Step 24 added the piano: under the canvas, the keys of a
  keyboard in the scale of the session (the root gold, the other notes teal), lit while a note is held
  and played with the pointer, with the scale as two choices and a "Lock to scale" toggle
  (`app/piano/`: `Piano`, `PianoBar`, `PianoKeys`; `client/pianoLayout.ts`; the intents `SetScale` and
  `SetLocked`). Step 25 made the nodes movable: a node is dragged by its header (the controls are
  `nodrag`), the page sends `SetNodeUi` once, when it is put down, every browser draws the node there,
  the session saves it, and a button on the canvas, Arrange, arranges the nodes again (`Editor.moveNode`,
  `resetLayout`; `useMovableNodes` and `app/patch/moving.ts` keep a node in hand while the session is laid out again; a node that the layout would put
  on one that was placed by hand, or within 16 pixels of it, is moved clear of it, below it and above it for a modulator: `clearOfPlaced` in `toFlow.ts`). Step 26 drew
  the cables between modulators: an LFO's rate and the stages of an envelope have a port on the top edge of their knob as a plugin's
  parameters have, a cable is dragged to one from another modulator's output (or chosen from the list at the foot of the
  modulator, which lists those settings after the parameters, under a heading for each modulator), and a modulator that
  drives the setting of another is drawn a row above it in a layout from nothing, so that a cable runs down (`app/patch/toFlow.ts`: `tiersOf`; a cable that is drawn leaves the nodes where they are, step 44;
  `client/reach.ts`: `fieldsOf`, pinned to the controller's rule by `fixtures/fields.json`; `Editor.addCable` takes a port). Step 27 made
  the modulation visible: a modulator's node draws what it has put out for the last four seconds as a line that scrolls (`ui/LiveTrace.tsx`,
  from the page's own samples of the telemetry, `client/trace.ts`: a rate that another modulator drives is seen to wander), and a cable is as
  bright as the output of its modulator is high (`app/patch/cableLight.ts`). Step 28 put the effects in order: every effect has
  two buttons that move it one place earlier or later in its chain, a lane's and a bus's alike, the one that cannot go that way
  dimmed (`MoveEffectButtons` in `app/patch/nodes.tsx`, `chainPlaceOf`; `Editor.moveEffect` asks the intent `MoveEffect`, which the
  mock engine does as the controller does, pinned to it by `fixtures/moves.json`), and the canvas lays the chain out in the new
  order; a browser lets go of the focus of an element that the page moves in the DOM, so the button that was pressed is given it
  again (`keepFocus` in `app/focus.ts`). Step 29 drew the shape of an envelope on its node: the line its four stages make, with a dot where
  the envelope is while notes are played (`ui/LiveEnvelope.tsx`, `client/envelope.ts`: a stage's width is the logarithm of its time, and the
  dot's stage is told from the level and which way it was last going, since the telemetry carries no phase: `courseAfter`). Step 30 let a person
  choose the knobs of a node: each row of a plugin's panel has a pin (`Show cutoff on the node`, pressed when the node shows it) that adds
  the parameter to the node or takes it off (`client/pinning.ts`: a node shows what is pinned to its plugin, or the first eight, and the
  target of a cable while the cable is there; `Editor.pinParam` keeps the other knobs as they were, `SetPinned` replaces the list), and a
  button under the count gives the node back to the first eight; the pins are the session's, so every page has them and the saved file
  has them by name. Step 31 brought the groups into the page: every lane has a "Group" cell, a choice of none, each group by its name or
  a new group with the lane in it (`GroupCell`; `Editor.setLaneGroup` and `addGroup`, which show the canvas the group, since the layout
  moves it to where its members are), the bar has "+ Group" (`app/AddGroup.tsx`), the name of a group is changed in place (a ✎ button
  turns it into a field: Enter or leaving the field asks `RenameGroup`, Escape asks nothing; `GroupNameField`), and a lane or a group is
  taken out by a "×" that opens a menu that asks first, in words that say what goes with it and what stays (`RemoveLaneOrGroupMenu`,
  `removalOf`; the only lane of a session has none, and there was no undo for it then). The mock engine does all of it as the controller does
  (`fixtures/groups.json`). Step 32 gave the page Undo and Redo: two buttons in the bar of the session that say what they would put
  back or do again (`app/UndoRedo.tsx`, from `ClientState.history`; `Editor.undo` and `redo`) and the keys Control or Command+Z,
  Control or Command+Shift+Z and Control+Y (not in a field that takes text, which has its own undo, and not when there is nothing to
  do); the mock engine keeps the controller's history (`client/history.ts`, `fixtures/history.json`). Step 33 put what is added in it
  (effects, instruments, modulators, cables, groups and the group of a lane: the page's "Undo add Reverb" takes the effect out and "Redo add
  Reverb" makes it again under the id it had), with the mock's ids and slots pinned to the controller's after every undo and redo by the shape
  of the session that the fixture records for each change of structure (`client/counters.ts`, `client/membership.ts`). Step 34 put what is
  taken out in it, as it was: a cable, a modulator and an effect (a lane's or a bus's, with the very plugin that the audio thread hands back) are put
  back by "Undo remove an LFO" or "Undo remove Reverb" with their cables, at their places and under their ids, and so are a lane and a group, whole ("Undo remove the
  lane of Oscillator", "Undo remove Pad": the group with its bus effects, its modulators and its members); their "×" asks first all the same, and says "for good" (see the
  TODO on its words). Step 35 put what is replaced in it: a preset that is loaded ("Undo load preset Small Room": the preset and the values the plugin had) and an instrument that another replaces
  ("Undo change the instrument to Oscillator": the very plugin that the audio thread handed back, with the cables that drove it and the lane's note remap); what still emptied the history was a group made of a lane that
  is in another and a lane put in a group or out of one whose modulators drive it (the second since step 96 is a step: see "The racks go (step 96)"). Step 36 drew the cables to the depth of a cable: every cable that starts at a
  modulator is a row at the foot of its node (`CableRows`, `PatchNodeData.cables`; the layout counts them, `nodeHeight(node, knobs, cables)`), with a port on the node's left edge named after the cable
  (`depth-<id>`: `DepthPort`, `depthHandle`) where a cable from another modulator ends to drive how deep that one goes: it is dragged to the port (the whole row takes it while one is dragged) or picked
  from the list at the foot of a modulator, under the modulator whose cables they are (`depth → Filter: cutoff`); the modulator that drives the depth of another's cable is a row above that one, as for a setting;
  the cable is drawn round the node it ends at, with its slider on the run under what drives it (`cableRoute`); and a cable that ends at a depth is told by the cable whose depth it is (`ADSR → depth of LFO → Filter: cutoff`:
  `endLabel`, `cableWords` in `client/names.ts`, which the editor uses as well for what it says of a refusal). Step 37 put the TUI's `m` and `l` on the page: every knob (a parameter of a plugin that is
  shown as a knob, and the rate and the stages of a modulator), every row of the ⚙ panel and every row of a modulator's cables has a `~` button (`ModulateMenu`, `app/patch/ModulateMenu.tsx`: quiet until the pointer or the
  focus is on the control, in the corner of its knob) with a menu of a new LFO, a new envelope, a new macro, a new MIDI control, a new velocity and a new key (step 52), under "New", and of the modulators that are there and can still drive it, under "Attach". A new
  one is made for the control with its cable in one intent, `Modulate` (`Editor.modulate`: a MIDI control is armed to learn and its cable is at the whole range, as the TUI's `l` has it, the others (a velocity and a key too) at half depth; the canvas is
  shown the node it made), and goes in the rack that reaches the control (the lane's for a lane's plugin, the group's for a bus effect, the modulator's own for its rate, a stage or the depth of its cables, where it is a
  sibling that drives it); one that is there gets a cable (`Editor.addCable`, the intent `AddModEdge`). What could drive a control is `driversOf` (`client/reach.ts`: `drivablesOf` read from the other side, tested against it
  over two fixtures), and the menu looks it up when it is opened (`MenuButton`'s `onOpen`), not when anything changes: there is a button on every knob. A control that is chosen from a list (a waveform, a parameter whose
  values have names) has no button, as the TUI's `m` leaves such a row out (a cable can still be dragged to its port). Step 38 let the page save the session
  under another name: a "Save as…" button after Save in the bar (`app/SaveAs.tsx`) opens a field in its place with the name of the file the session has,
  selected; Enter or OK asks `Editor.saveAs` (the intent `SaveAs`) for the name as it was typed, and the server says what a name may be and where the
  file goes (`session::plain_file_name`: the page has no rule of its own; the mock engine has it written out, `client/filenames.ts`, pinned to the
  controller's name by name by `fixtures/filenames.json`, 67 names); a refusal is said among the notices (the name in quotes, cut after 24 characters) and the field stays, Escape and Cancel leave, and
  `Control+Shift+S` and `Command+Shift+S` open it (Control+S is still the save). The bar takes a second row in a window too narrow for all of it (`flex-wrap`).
  Step 39 drew what a cable to a modulator's own setting does to it: the knob of an LFO's rate or of a stage of an envelope has the ring and the dot that a
  parameter's knob has (`ModulatorFieldControl`: the dot is the telemetry's value of the setting, which the engine publishes and the layout names at the port the
  cable ends at, and the ring is `settingExtent`, the extent of the last cable alone, since the engine's writes to a setting are last-wins and not summed as a
  parameter's are), and the slider of a cable whose depth another cable drives has a bar under it, from the middle, for the depth the engine makes of it
  (`CableDepthControl`, a `LiveBar`; the slider stays what the user set). The controller gives every cable a slot for that value (`Controller::add_target` and
  the loader), and the mock engine does as it does. Step 40 made the page say when the server is slow to answer: a request that has waited more than 400 ms
  (`SLOW_AFTER`) for its answer (a plugin that is slow to load: the program does it on the thread that answers, so everything else waits behind it) is
  counted by the client (`Client.slow`, a store of its own: the client's state is told to every control at every change, and a request goes at every frame of
  a drag), and a "Working…" light shows over the top of the canvas while any is (`app/BusyLight.tsx`: a progress bar with no value, since the page's one `status`
  is the connection's); nothing is blocked by it, and it does not say what is being done. Step 41 made the ring of a parameter that a lane's modulators and its
  group's both drive the lane's alone (`modulationsOn`, from the engine's order of writes: see "Application order" under "Group modulators"; every driver adds up since step 96), made the mock engine's value
  of a parameter that several cables drive the engine's own (each cable's offset made from the value the user set and added, not chained), and had its MIDI control (the mock
  has no MIDI) rest where the engine's does. Step 42 gave the master node a scope under its meter (`ui/LiveScope`, drawn on a canvas and not by React: the wave of the output from where it goes up through the middle, so that a wave that goes
  round again and again stands still, with the right side under it where it is not the same, or its spectrum, whose levels fall slowly; two choices under it, the view and for the wave a time base of 5 to 43 ms, which are the viewer's own and
  not the session's). The page asks the server for the `scope` telemetry channel while the master node is on it (`Client.watchTelemetry`: counted, and asked again on every connection), works the spectrum out itself
  (`client/fft.ts`: the server sends samples, and a spectrum of them is a drawing too) and is told what the frequencies are by `Meta.sampleRate`. The mock engine's scope sounds like the notes that are held. The terminal's
  Scope tab draws the same (see "Scope tab").
  Step 43 made the ring and the dot of a rate, an attack, a decay and a release follow the engine's law for such a setting, a fraction of the knob's travel (see "Cross-modulation"):
  `settingExtent` takes the scale of the setting, the mock engine does as the engine does, and how a range is spread over a knob is in `client/travel.ts`, which `ui/knob-geometry.ts` exports too.
  Step 44 stopped the canvas from being rearranged under a person's hand: `toFlow(graph, remembered)` is given where it put the nodes last (`Flow.places`) and leaves a node where it was while its slot is the same
  (`slotsOf`: an effect is in the place of its chain that it is at, a modulator in its rack, a lane in the session, a group and the master in the column that the layout gives them, and what is in a group's strip in the slot of the group),
  so that a cable that is drawn or taken out, and a node that is added or taken out, moves nothing but what needs room: a node that has grown, or a modulator that is added to a strip that has no room above it, moves the rows below it down,
  strips whole (`makeRoom`), and a group's strip and the master go along with a lane that grows past them (a lane that is new goes below the ones that stayed, `clearOfKept`; a node on one that a person placed goes clear of it, last, `clearOfPlaced`).
  `useLayout` keeps the places and lays the nodes out from nothing when "Arrange" is pressed (a button in the corner of the canvas, "Arrange the nodes again" to a screen reader; lit while a node has a place or one is not where the layout has it, `Flow.arranged`),
  which gives the places back (`resetLayout`) and then fits the view to every node (`FitWhenArranged`). The view moves to show a node that is added where it cannot be seen.
  Step 46 gave every instrument that has a plugin, every effect and every group a button in its header (the sine wave, "Scope of X") that shows a scope of what comes out of it under its controls, with the view and the time
  base of the master's (`app/patch/NodeScope.tsx`: `ScopeButton`, `NodeScope` and `ScopePanel`, which the master is drawn with too): the page asks the server for the signal while the scope is there (`Client.watchProbe`, counted, and
  asked again on every connection: D47), which nodes show one is the viewer's own (`PatchView` holds the set, `scopes.ts` hands it to the nodes; it is not saved, not sent and not in the history), and the layout makes room for it
  (`toFlow(graph, remembered, scopes)`: `canScope`, `PatchNodeData.scope`, `NODE_SCOPE_HEIGHT`, 157 as a browser draws it; `useLayout(graph, scopes)`). The mock engine has eight rings and sounds like the notes that are held. A scope that is opened where it cannot be seen moves the canvas to the node, as a node that the page added does (`Reveals`, once the layout has the scope). The badge of a node
  (what kind of plugin it is) is under its name in the header, in small type, so that the name has what the buttons leave.
  Step 57 added the step sequencer (see "Step sequencer modulator"): a node with its rate (a knob with a port, as an LFO's), its glide and its length (knobs with none: `ModulatorValueControl`), its
  retrigger (a choice of off and on: `RetriggerControl`) and, above the trace of what it puts out, sixteen bars (`ui/StepBars.tsx`: a `role="slider"` for each step, a bar up from the middle for a value above nothing and
  down for one below it, the steps past the length dimmed, a playhead on the step the engine says it is at that is written to the DOM on the frames and never rendered). A stroke across them paints them
  (`client/stepbars.ts`: `columnAt`, `stepValueAt`, `paintTo`, `playheadStep`; a pointer that jumps bars sets the ones between along the line, a bar has the value at its height to a hundredth and nothing near the
  middle), a key moves one (up and down by a fiftieth, the page keys by a tenth, Home and End to the ends; the arrows that go across move the focus from bar to bar, and one bar is in the tab order) and a double
  click puts one back to nothing; a stroke is all the sixteen values a frame at a time (`Drive.steps`, a `Control` of a list: `Control` is generic, and takes which values it will send), shows what it has painted until the
  server has answered and is one step of the history. It is made from the "+" menu of a lane or a group ("Step sequencer") and the `~` menu of a knob ("New step sequencer"), and its node is
  `STEPS_NODE_HEIGHT` tall (an LFO's and the bars and the gap above them: 321 against 249 as a browser draws them). The mock engine has the same sequencer (`stepsPlace`, the engine's wrap at the length, which
  puts out the position the playhead follows; a note starts it again when it is set to). `e2e/steps.spec.ts` runs the node, a stroke that is one Undo and its steps in the saved file, and a note that puts the playhead on the first step.
  Step 76 drew the audio cables (P3d; see "Audio cables (`[[cable]]`)" for the whole of it): `AudioPort` handles on the nodes, `SendEdge` with its level slider and its button that takes it out, the inputs that take a dragged cable grown to a strip,
  `SendMenu` in the headers (on the badge's line: a lane's row of buttons has no room for one more), the mock engine that does what the controller does with them, and `e2e/sends.spec.ts` (a cable dragged, its level, the keyboard's menu, undo and the
  saved `[[cable]]` on the real program).
  Step 77 put the way out of a lane and of a group on the page (P3c-2a; see "The way out (`route`)"): a button under the mute and the solo in the "Mute / Solo" cell of every lane and group (`WayOutButton`: the node's headers are
  full and the cell has the room, so that no node is taller), saying where the audio goes (`→ Master`, `→ Pad`) or, pressed and in the colour of the audio cables, that there is no way out (`no out`), asking `Editor.setRoute`; the mock engine
  does what the controller does with it, and `e2e/routes.spec.ts` runs it on the mock (the link to the master goes from the canvas, a lane in a group, the history's words) and on the real program (silent when it is cut, heard through a
  cable drawn from the instrument and only through it, the saved `route = "none"`, undo and redo, a page opened again). The button is gone since step 88b (see "Links are cables"): the link is a cable now.
  Step 78 gave the page the law of mute and solo for what cables carry (P3c-2b; see "Mute and solo"): `client/audible.ts` has it as the engine does (`heardOf`, `audibleOf`, `ownAudibleOf`, `fedOf`), `audibleAt` dims the instrument of a lane that is heard only for what a cable sends it and not its effects, the mock engine's meters follow it, and `e2e/solocables.spec.ts` runs it on the mock (the instrument dimmed and the filter not, until the cable has no level or the sender is muted) and on the real program (a lane with no way out, heard through the filter of a lane that is not soloed, whose own notes are not).
  Step 82 gave the controller sources of notes and cables of notes (P4b; see "Sources of notes and cables of notes"): the graph a page gets has their nodes and edges (`source` nodes, `events` and `hears` edges, the ports `eventsOut` and `eventsIn`).
  Step 83 put them on the page (P4c; see "The page" under "Sources of notes and cables of notes"): the message `inputs` and the page's rule for which input a device stands for (`client/inputs.ts`, pinned by `fixtures/inputs.json`), the mock engine's sources and cables of notes (`client/noteWiring.ts`, pinned by `fixtures/sources.json`), the canvas's source nodes in a column of their own before the lanes, `NotesPort` on a source and on a lane, the cables
  of notes and the links by which a lane hears every source (`NotesEdge`, `HearsEdge`), the menus for the keyboard (`NotesMenus`), the bar's "+ Source" (`AddSource`), `Editor.addSource` and `addNotesCable`, and `e2e/sources.spec.ts`.
  Step 87 let an effect stand by itself (see "An effect that stands by itself"): the bar's "+ Effect" (`AddFreeEffectMenu`, `Editor.addFreeEffect`, the intent `AddFreeEffect`) adds an effect in a lane of its own that has no instrument, and the canvas draws no card for a lane that only holds effects (`holdersOf` in `client/holders.ts`; `toFlow` leaves it, and the links at it, out): the effect is a module with the lane's mute, solo, way out and "+" (`PatchNodeData.heldIn`), whose "×" takes the lane with it when it is on its own. A lane whose instrument is taken out alone (step 85) is drawn so when it has nothing else; a lane that has a key range, a pattern, a group or a cable keeps its card.
  Step 88a (P5.0: see "Links are cables") made every audio link a cable to a client at the controller's level (a cable drawn between the ends of a link that is cut gives it back, one between the ends of a link that is there is refused, and a module that stands by itself is bare) and gave the page the logic of it (`linkOrCable` and `cableTakers` in `client/wiring.ts`, the mock engine pinned by the `cutRows` of `fixtures/audioCables.json`) and not the picture, which step 88b made.
  Step 88b (P5.0: see "Links are cables") drew every link as a cable: amber, no slider, with a × named `Remove cable A → B` that cuts it (`RemoveLinkButton` in `AudioEdge`) and puts the focus on the send menu of the node the link started at, which draws it again; `WayOutButton`, `FeedButton`, `wayOutOf`, `inputOf`, `.tang-way-out` and `.tang-feed` are gone, and the "Mute / Solo" cell is the mute and the solo.
  Step 84 gave the link that every lane has by itself, the grey one from the end of its chain to its group or the master, the button that disconnects it (see "The way out (`route`)"): a × at its middle that cuts the way out, in `AudioEdge` (since step 88b it says `Remove cable Filter → Master`).
  Step 86 made every link of a chain one that can be cut (see "The input of an effect"): a × on every link into an effect (`DisconnectFeedButton` in `AudioEdge`: `Disconnect Oscillator → Filter`), which asks `SetFeed none` and puts the focus on the effect's own button, `FeedButton`, on the badge's line of its header (`← Oscillator`, or `no in` pressed when it is cut), which gives the link back; `Editor.setFeed`, `inputOf`, `feedLoops`, and the mock engine's `setFeedOutcome` and `relinked` (its chain operations make the links again from the nodes). Since step 88b the × says `Remove cable Oscillator → Filter` and gives the focus to the send menu of the node the link started at, and `FeedButton` and `inputOf` are gone.
  Step 85 let the × of a lane take the instrument out alone (`RemoveLaneOrGroupMenu` in `app/patch/nodes.tsx`): the menu of a lane that has an instrument is "Keep Oscillator 1", "Remove Oscillator 1 only: its 4 effects and 2 modulators stay in the lane" (the intent `ClearInstrument`, `Editor.clearInstrument`: the lane stays on the canvas as `(empty)`, with its effects, modulators and the rest, and the focus goes to its button that puts an instrument in) and "Remove the lane of Oscillator 1 for good: …" (the lane's removal, as before; not for the only lane of a session, which has the first two); a lane that has none is offered the removal of the lane only. `removalOf` says whether a lane has an instrument (`Removal.instrument`); the mock engine does what the controller does (`clearInstrumentOutcome`, the history's words `take Oscillator out of its lane`), pinned by five scripts of `history.json`.
  Step 92 (P5b-3) let the page make and aim a modulator that is in no lane and no group (see "Free modulators"): the bar's "+ Modulator" (`AddFreeModulatorMenu`, `Editor.addFreeModulator`), the canvas's row of them above the first lane (`toFlow`), `reachFrom` and `modulatorsInReach` for a modulator that has no parent (`client/reach.ts`: every plugin, and the other free modulators), the `~` menu's "Attach" of any knob,
  the knob's ring by the nearest rack (`modulationsOn`; the sum of every driver since step 96), and the mock engine pinned by `history.json` and `freeReach.json`.
  Step 93 (P5c-1) let a modulator that is in no lane and no group hear one lane's or one group's notes (see "Free modulators", "The ear"): the ♫ button in its header (`HearsMenu`, `client/hears.ts`), `Editor.setModulatorEar`, `hears` in the graph's modulator node, the mock engine pinned by a script of `history.json`, and `e2e/freemodulators.spec.ts`.
  Step 94 (P5c-2) made every modulator the session's own (see "Free modulators", "Every modulator is global"): `Editor.addFreeModulator(kind, hears)` for the "+" of a lane and of a group, the mock engine's `Modulate` making one that hears the knob's lane or group, `ownersOf` in `toFlow.ts` (a modulator is laid out above the strip of the lane or group it hears), `client/moved.ts` and the mock engine's scenes as the program has them once loaded, `Removal.hearing` in the "×" words, and the specs.
  Step 95 (the small half of P3g) gave an effect a second input of audio (see "An effect with a second input"): a node that has one has two input handles on its left edge, A and B (`AudioPort`'s `pair`), the page has the controller's rule for a cable at either (`audioTakers`, `cableIsBetween`), the keyboard's menu has `Math (A)` and `Math (B)`, the words of a cable say the input (`Oscillator → Math: B`),
  `Editor.addAudioCable` takes `side`, the mock engine is pinned by `fixtures/sideCables.json` and a script of `history.json`, and `e2e/math.spec.ts` runs it on the mock and on the program. The fixture found a discrepancy that was older than the step (see `docs/web-ui.md`): the mock put the node of a lane that was added after the free modulators, where the controller lists it before them.
  Step 95b made a knob that has no effect now say so (see "A parameter that has an effect only sometimes"): `Param.usedWhen` in the graph, `client/uses.ts` (the rule, pinned by `fixtures/uses.json`), `paramUnusedBecause`, and `data-unused` and a title on the cell of a node's grid and on the row of the ⚙ panel, which the stylesheet dims; the Math's `pitch` and `wave` while a cable is at B, and `amount`, `bits`, `attack` and `release` in the modes that do not read them.
  Step 96 (P5c-3) took the lanes' and the groups' racks out of the page (see "The racks go (step 96)"): every modulator is the session's own and has `parent: null` and, if it has an ear, `hears`; `reachFrom`, `modulatorsInReach`, `modulationsOn` and the mock engine's value of a parameter
  have no rack to tell apart (`ownerOf`, `NEARNESS`, `Removal.modulators`, `Editor.addModulator`, `client/membership.ts` and `client/moved.ts` are gone), `Modulate` and "+" make a modulator that hears the knob's or the lane's or the group's node, and the mock engine's fixtures are written by Rust from the sessions as the program has them once loaded.
  Step 97 let a modulator drive the volume, the mix and the pan that the host keeps (see "The volume, the mix and the pan that a modulator drives"): the cell of such a value (`GainCell`) has a port on its top edge and the `~` button when the node has the value, the knob (`GainControl`) has the ring and the dot of a parameter's, `gainPorts`, `isGainPort`, `Drivable.fields`, `driversOf`, `gainName`, `portName`, `portAt` and `handleOf` know the three ports, and the mock engine does what the controller does (`addGainCable`), pinned by `fixtures/freeReach.json`'s `gains` and three scripts of `history.json`; `e2e/gains.spec.ts`.
  Step 98 made the Arrange button put the nodes where the cables between them are short (see "Arranging the nodes"): a pure function of the boxes and the cables (`app/patch/arrange.ts`), asked for by `toFlow(graph, remembered, scopes, { arrange: true })` and `useLayout` (`optimized`), with the node that has no cable drawn to the middle of the others, a modulator tied to what it hears and a new effect put by the one it follows; the button is bright when a session is opened, dim once it is arranged.
  Step 98b made the two oscillators of `bell-2` one over the other (the user's "do you think its possible to arrange from this [Image #2], to this? [Image #3] just looking at the 2 osc?"): a cable of modulation ends at the column of its knob (`Wire.toX`), the nodes that feed one node are drawn into a column (`ALIGN`) and the cables at its two inputs are not crossed (`INVERT`), the work is the best of up to twelve starts (`STARTS`, `TOTAL_WORK`, `shuffled`), and a node that a cable to the depth of a cable ends at keeps `SIDE_ROOM` at its left.
  Step 100 gave every lane and every group a level on its card (the user's first try of the microphone, "hmm I cant get the microphone to work yet", and the light for a silent input that it was offered): `LevelStrip` (`app/patch/LevelStrip.tsx`), the master's `Meter` without its CLIP light over the peak that the engine publishes for the node (`slotReader(client.telemetry, "meters", node, { type: "audioOut" }, "peak")`), named `Level of <name>`, drawn over the 3 px stripe of the node's kind at the top of its card (`.tang-node__level`, positioned at `top: -3px`, so that no card is taller): the stripe turns green from the left as the level rises. The lane's meter is after its chain, its volume and the fade of a mute (a muted lane has none; the scope button has the raw wave), and a lane that has no way out has it too, so that the audio input, which is bare until a cable is drawn from it, reads its tone (-12 dB on the null backend) the moment it is added: a level that stays at the floor says that nothing arrives. An effect, a modulator, a source and the master have none of it (the master has its own). Held by `PatchView.levels.test.tsx`, `web/e2e/input.spec.ts` and 8 mutants of the page, 7 caught and 1 no fault (`useMemo`'s dependency on the node).
TODO: the terminal has no level (the Session tab's tree could end a lane's line with a small bar, from the telemetry that it reads for the playhead and the CLIP light), a lane has no clip colour, and an effect has no level (the engine has no meter for one).
  Step 101 let go of a drag whose release the page never saw (the user: "sometimes the mouse pointer gets "attached" to a knob or window and wont release even when I release my mouse"; not reproduced, a net over the ways to lose a release): `ui/dragEnd.ts` (`releaseLost`: a mouse that moves with no button down; `startsDrag`: the main button, not with Control held, which on a Mac is the secondary button and opens a menu that takes the release; `useDragEnd`: the pointer up or cancel anywhere, the blur, a context menu, the page hidden) is used by the knob and by the step bars (which end on a lost pointer capture too), as the piano has had since step 24, and `app/patch/stuckDrag.ts` (`useStuckDragGuard`, in the canvas) sends the mouse up that a card of the canvas was waiting for (React Flow moves a card with d3-drag, which ends only on a mouse up in the window) when a mouse moves with no button down or the window loses the focus while a card is in hand: with the window as the event's `view`, which d3-drag takes its listeners off. Held by `dragEnd.test.tsx`, `stuckDrag.test.tsx`, the knob's and the bars' tests, `web/e2e/stuckdrag.spec.ts` and 34 mutants, all caught.
TODO: a cable that is being drawn can still stay in hand if its release is lost (ending it where the pointer is could draw it to whatever it is over), and the piano's net has no context menu.
  Step 102 gave the master a volume, a limiter and a readout of how far the limiter works (the user's "we often reach the clip-lvl and its hard to control"; see "The master limiter and volume"): the master's card has a row of three cells under its meter and its scope, the `Master volume` knob, the `Ceiling` knob (read as `-3.0 dB`) and the `Limiter` button (`On` or `Off`, `aria-pressed`: `LimiterCell`, which asks `Editor.setLimiter`), and under them the `Limiter reduction` bar with its number (`app/patch/ReductionMeter.tsx`, from the telemetry cell `reduction` of the master, written to the DOM on the frames); the knobs are `GainControl`s with no port and no `~` button (`GAIN_PORTS` has none for them: no modulator drives the master), `NodeKind::Master` has `volume`, `limiter` and `ceiling`, the reducers and selectors know the master's gains and `limiterOf`, and the mock engine does what the controller does (`setLimiter`, `masterGainOutcome`, the master's level held under the ceiling and the reduction in its telemetry, the history's words), pinned by a script of `history.json`; `e2e/master.spec.ts` runs it on the mock and on the real program (a hot session held at the ceiling, the CLIP light dark with the limiter in and lit without it, the file's `[master]`, Undo, a page opened again).
  Step 103 hid the groups from the page (the user's "I think we should remove the groups (at least wihin the UI)" and, to the proposal of hiding them, "2. yes hide groups"; see "Group format"): the bar has no "+ Group" (`AddGroup.tsx` is gone), a lane's choice of group (`GroupCell`) offers none and each group of the session and no "new group", and a lane has the choice only while the session has a group (`PatchNodeData.groupCell`, which `toFlow` sets from the graph and which `controlCount`, `nodeHeight`, `blocksOf` and `rectOf` count: a card in a session with no group has one control fewer, a row shorter where the count passes a multiple of four). A group that a session has (an old file's, or one that the terminal's `g` made under `--web`) is drawn as before, a lane can leave it from the cell, and the focus after a group is taken out goes to "+ Instrument"; `Editor.addGroup` stays and no control calls it. The tests that made a group from the page start from one now: the mock's scene `?scene=groups` (the tracer and an empty group) and `testWithGroups` in `web/e2e/own.ts` (the real program, on the tracer's file with groups added). `examples/shared-reverb.toml`, the one example with a group, has its reverb in a lane that has no instrument (the same bits, every sample). Web 2,932 tests in 126 files, the whole browser suite (209), Rust 2,532 tests, 18 mutants of the page, all caught in the end (one survived at first: `rectOf`'s height of a lane that has the cell, a test now).
TODO: the terminal still makes groups (`g`) and shows them, the Patch tab has their rows and columns, and the groups are in the model, the file and the engine until P5c, which loads an old file's group as a lane that holds its bus effects with a module for its volume and pan (a gain-and-pan built-in that does not exist yet).
  Step 104 let a person keep the chain of effects of a lane as a module and add it again (the user's "In the future, I want to be able to design a set of modules, save them as a stand-alone module that you can then load inside another instrument"; see "Modules: effects and instruments, with the cables between them, saved to a file"): a button ⤓ in the "Mute / Solo" cell of a lane that has effects (`SaveModuleButton`, `app/patch/SaveModule.tsx`: a small field for the name under it, drawn in the page and not in the node; `Editor.saveModule`, which says among the notices that the module is saved or why it was not; `chainLengthOf`), the bar's "+ Effect" lists the modules first under a heading `Module` (`freeEffectItems`; the lane's and the bus's "fx" menus do not), and the mock engine has a library of its own in memory (`client/modules.ts`, `MockServer`'s `#library`), held to the controller's by `fixtures/modules.json` and its `odd` scenario (a module that a hand wrote). Web 2,970 tests in 128 files, the browser suite 211 (`e2e/modules.spec.ts`), 119 mutants of the page (100 caught by the tests that were there, 3 of them only by the type check, and 19 that survived: 22 gaps, tests now).
  Step 105 made a module any plugins that are chosen with the cables between them (the user, shown step 104 and a picture of three effects in lanes of their own joined by cables: "the goal is to save the graph of fx or instruments, like this [Image #4], into its own module? is that possible?"; see "Modules: effects and instruments, with the cables between them, saved to a file"): the canvas's **Select** button turns choosing on (a click, Shift-click or a box chooses nodes: `elementsSelectable` and `selectionOnDrag` of React Flow, with `nodesDraggable` off while it is on), the bar says how many are chosen and has **Save as module…** (`SaveSelectionButton`, which shares its field for the name with the cell's ⤓: `ModuleNameButton` in `SaveModule.tsx`; `modulePluginsOf` in `app/patch/selection.ts` picks the plugins out of what is chosen), `Editor.saveSelectionAsModule`, and the mock engine's library keeps lanes with an instrument or none and cables between any of their plugins (`client/modules.ts`: `Module`, `ModuleLane`, `moduleOfNodes`, `pluginCount`; `mock.ts`: `addModuleOutcome` for several lanes and instruments, `saveSelectionOutcome`), held to the controller's by `fixtures/moduleGraphs.json`. Web 2,995 tests in 130 files, the browser suite 214 (`e2e/modules.spec.ts`: a box round three effects that cables join, saved and added as one step on the mock; on the real program the file of the library and the session's own).
  Step 106 let a module hold the modulators that are chosen with its plugins (the user's "yes agree LFOs should be next!"; see "Modules: effects and instruments, with the cables between them, saved to a file"): `modulePluginsOf` holds the modulators and counts the plugins (`Held.plugins`), the bar's **Save as module…** is out of reach while no plugin is held and says why (`, and a module needs an effect or an instrument`), and the mock engine's library keeps modulators with their ears and cables (`client/modules.ts`: `ModuleModulator`, `ModuleTarget`, `ModuleDrives`, `modulatorsOfModule`; `mock.ts`: `withModuleModulators`, `moduleCable`, `heldSource`, `nameOfSource`; `takesOut` takes the modulators out first), held to the controller's by the second scenario of `fixtures/moduleGraphs.json` (`modulatorSteps`). Web 3,008 tests in 130 files, the browser suite 216 (`e2e/modules.spec.ts`: a modulator chosen with the plugins that it drives and saved, on the mock and on the real program).
  Step 107 gave the canvas a right-click menu system and a box drawn with Shift (the user's "can we add a right click menu system? Also lets have shift+mouse drag to select many things?"; see "Right-click menus, choosing with Shift and moving what is chosen"): `ui/ContextMenu.tsx` (the menu at the pointer: the keys, type-ahead, menus in menus, the room to the pixel), `app/patch/contextMenu.tsx` (the host: `ContextMenuHost`, `useContextMenu`), `app/patch/contextMenus.ts` (what the canvas, a lane, an effect, a modulator, a source, a group, the master, a cable and what is chosen offer, made from the lists that the buttons have; `nodeSpec` and `edgeSpec`), the places that `Editor.addInstrument`, `addFreeEffect`, `addFreeModulator` and `addSource` take (`#placed`: an add and then a move), a box drawn with Shift held and a click with Shift held that choose with Select off (`elementsSelectable` is `choosing || adding || boxing`), nodes that are chosen moved together and by the arrow keys (`useMovableNodes`, `keepHeld`), the field for a module's name where the pointer was (`ModuleNameDialog`), and the helpers of the browser specs that wait for a quiet master before a key goes down (`e2e/sound.ts`). Web 3,130 tests in 135 files, the browser suite 227 (`e2e/menus.spec.ts` 11).
  Step 108 drew a module that is added as one box (the user's "the goal when we re-add a module is that it gets rendered like just a single box"; see "A module that is added is one box"): `app/patch/boxes.ts` (`collapse(graph, names, open)`: the page's view of the session, with the nodes of the modules that are closed left out and a box for each, the cables that cross its edge at handles that say which port of which node they are, `memberHandle` in `handles.ts`), `ModuleBox.tsx` (the box: its ports as rows, the Open button, the × that asks before it takes the module out with all that is in it), `ModuleTag.tsx` (the tag over a lane of an open module, which closes it), `openModules.ts` (which modules are open: the viewer's own, not saved), `connection.ts` (a cable dragged to or from a box's handle is the cable to or from the node inside), the box's strip and height in `toFlow.ts`, the box's menu and a chosen box's contents in `contextMenus.ts` and `selection.ts`, and the mock engine's record (`withModuleRecord`, `removeModuleOutcome`, pinned by `fixtures/moduleBoxes.json`). Web 3,214 tests in 141 files, the browser suite 231 (`e2e/boxes.spec.ts` 4).
  Step 109 let a person collapse what is chosen into one module where it is (the user's "compact/collapse them into a module in place ... to clean up the ui a bit"; see "Collapsing what is chosen into one module where it is"): `collapsibleOf`, `collapseWords`, `collapseWhyNot` and `collapsePlace` in `app/patch/selection.ts` (what collapsing takes, as the controller works it out, and where the box is drawn: the top left of what it hides), the bar's **Collapse…** (`CollapseSelectionButton` in `SaveModule.tsx`, which shares the field for a name with the save: `NameOptions`), the canvas's right-click item (`collapse-chosen` in `contextMenus.ts`), **Ungroup** in the × menu of a box (`ModuleBox.tsx`), in its right-click menu and in the menu of a lane of an open module, `Editor.collapseIntoModule` and `ungroupModule`, `toFlow`'s memory of the places of the nodes that a closed module hides, and the mock engine's `collapseIntoModuleOutcome` and `ungroupModuleOutcome` (pinned by `fixtures/moduleCollapse.json`). Web 3,248 tests in 141 files, the browser suite 235 (`e2e/collapse.spec.ts` 4).
  Step 110 gave a macro a name and a choice of whether the box of its module shows its knob (the user's "expose a macro-knob as well, maybe thats the most common usecase", "let a Macro have a name and an expose option/flip", "the expose can be default on"; see "A macro that has a name and a knob that a box shows"): `Box.knobs` (`boxes.ts`: a `BoxKnob` for each macro in the module that is not told to hide its knob, called by the name the canvas gives the node), the group of knobs that `ModuleBox.tsx` draws under the ports in the grid that a plugin's controls are in (each a `MacroValueControl` with the name for its label, the macro's own value turned by its node), `nodeHeight` of a box with the grid and `heightOfData`, the one height that `blocksOf` and `rectOf` share, the flag in `sameNodeStructure`, the ✎ and the field that name a macro in its node (`NameField`, the group's field made general), `ExposeControl` (the choice beside the macro's value), `Editor.renameMacro` and `exposeMacro`, `client/macros.ts` (what an absent name and an absent flag are), and the mock engine's `renameMacroOutcome`, `exposeMacroOutcome` and the history's plans (pinned by `fixtures/macros.json`). Web 3,283 tests in 143 files, the browser suite 238 (`e2e/macrobox.spec.ts` 3).
  Step 111 gave modules a category of their own and the knob of a macro on a box a port (the user's "now we need to be able to attach cables to the macro on the box as well. Also move modules to its own category as they are now under effects"; see "Modules have a category of their own, and the knob of a macro on a box has a port"): the bar's `+ Module` (`AddModuleMenu`, `moduleItems`, `Editor.addModule`) and the canvas's "Add a module" list the modules, `+ Effect` and "Add an effect" the plugins only (`freeEffectItems` is gone); `BoxKnob.targets` (what the macro can be drawn to, `drivablesOf`, which `drawableFrom` finds among the knobs of the boxes when it finds no node), `KnobPort` (the handle `modOut@<macro>` in the knob's cell, where a cable is started; a macro that shows its knob has no port on the box's bottom edge as well) and `DrivePicker`'s `cell` form (a → in the corner of the knob, for a keyboard). No intent is new. Web 3,311 tests in 143 files, the browser suite 241 (`e2e/macrobox.spec.ts` 6).
  Step 112 (part B) put a button on the node of a plugin that has a window of its own (the user's "for a VST, is it possible to add a button to open the official UI?" and "lets do the plugins own window next"; see "A plugin's own window" in `plugins.md`): `hasWindowOf(state, node)` in `client/selectors.ts` (the hello's `windows` feature, which the server says when its machine can show a window, and the plugin's `hasEditor`, for an instrument or an effect and no other node), `Editor.openEditor(node)` (the intent `OpenEditor`; a refusal is a notice, `Could not open the window of Filter: Filter has no window of its own`), the **"UI" button** (`WindowButton` in `nodes.tsx`, `windowButtonId`, `window-<node>`) in the header of the node of an instrument or an effect, by the presets and the parameters, and **"Open its own window"** in the node's right-click menu (`viewItems` in `contextMenus.ts`, before "Parameters…"); the mock engine takes the option `windows` and checks in the controller's order (`openEditorOutcome`); `editorOpened` changes nothing in the page (`reducers.ts`) and an `openEditor` is no step of the history (`planOf` in `history.ts`). The window is not in the browser: it opens on the machine that runs Tang, where it takes the keyboard from the terminal, and what is done in it comes back as changes of the plugin's parameters, which the knobs show. Held by `PatchView.windows.test.tsx` (the button: where it is and where it is not, the request it asks, the menu item and its place) and seen in the story `WithPluginsThatHaveTheirOwnWindow`.
  What the controller
  clamps to (the ranges, what a new modulator starts as, whether a setting is turned by distances or by ratios, the order of the waveforms) is
  generated into `web/src/protocol/generated/constants.ts` with the types and read by the mock
  engine and the controls, not copied. The page is the connection state, the canvas and the session as JSON
  (closed). `cd web && npm ci`, then `npm run dev` (Vite on 5173, forwarding `/ws`
  to `tang serve --web-origin http://localhost:5173`; `?engine=mock` in the address
  needs no server, `&scene=stress` serves eight lanes with 32 LFOs and 64 driven
  parameters, `&scene=lanes` two lanes with an LFO each and `&scene=groups` the tracer with an empty group, which the page has no way to make), `npm test` (Vitest), `npm run lint`, `npm run typecheck`,
  `npm run storybook`, `npm run build` (→ `web/dist`), `npm run e2e` (Playwright,
  one test at a time, since they share the real program's session, which is a copy of the tracer in
  `web/.e2e-session/` so that a test that saves does not write the example; needs
  `cargo build --features web`; `npm run e2e:install` once; they include the
  performance tests of docs/web-ui.md §9.4, which need the Vite dev server for the
  count of React's commits). The program serves
  `web/dist` itself (`src/web/assets.rs`, `rust-embed`: read from the disk in a debug
  build, inside the binary in a release build, so **run `npm run build` before
  `cargo build --release --features web`**; without it the program serves a placeholder
  that says so). `web/dist/` keeps a `.gitkeep` so that the program builds without
  Node. The tracer's snapshot in `web/src/client/fixtures/` is generated:
  `TANG_UPDATE_TYPES=1 cargo test --features web` rewrites it along with the types (and
  `reach.json`, next to it, which says where the controller let a cable be drawn, `fields.json`, which says
  which settings and cable depths of other modulators it let a cable be drawn to, `moves.json`, which says what
  it makes of moving each effect of a session to each of some places, `groups.json`, which says what it makes of adding a group,
  putting a lane in one, naming one and taking a lane or a group out, `history.json`, which says what it makes of scripts of intents and of time going by
  (the events, what Undo and Redo would do after each, the words of a refusal), and
  `builtins.catalog.json`, the built-in plugins as the catalog lists them, which the mock serves). The fixtures say numbers as the wire does
  (`app::testutil::wire`, not `serde_json::to_value`, which widens a 32-bit number to a 64-bit one).
- **TUI and web together** (built; step 13, `docs/web-ui.md` §3.4): `tang --web`
  runs the TUI and the web interface on one controller, on the main thread. The loop
  applies a browser's requests and reacts to what they changed (a value a browser set
  is on the TUI's screen with the next frame, and a step the TUI takes starts from
  it); what the TUI changes through intents reaches the browsers as events, and the
  rest of what it does by a snapshot (`Controller::resync`, see "Session state").
  The tree is addressed by position and a browser can add or take out a node above
  the cursor, so the TUI keeps track of nodes and not of places: its cursor stays on
  the node it was on (found by its id, with the kind of entry: a lane's pattern is
  an entry of the lane's node, as its instrument is), and a popup that holds a place
  in the tree (a lane's range, a group's name, a selector of plugins, of targets or
  of presets, the modulate popup, the group popup) is closed when the entries of the
  tree have changed, as it would act on whatever is at its place now. A value being
  typed goes to the node under the cursor, which follows its node, and is closed
  only if that node is gone; a cable that is added or taken out moves no place and
  closes nothing.
  Tested by running the real loop with a real server and a WebSocket client that
  presses keys by sending them to the loop (`tui::golden_tests::with_web`), and by
  hand with the real program in a pty and a real browser (18 checks, both ways).
- **Scope**: track A first — a "rack-as-patch" canvas over the existing engine
  (serial audio chain, block-rate lane-global modulation, cables = modulation
  targets). A patchable synth engine (`builtin:patch`, track B) comes later.

Phase 0 (foundations) is built: the 13 steps of `docs/web-ui.md` §12. Steps 14 and 15 did the
first of phase 3 ahead of phase 1 (D18): a modulator's settings and a cable's depth are turned,
modulators and cables are added and taken out, in the browser and in the TUI. Step 16 made the keys
of the computer a piano in the page, so that what is built can be tried by ear. Phase 1 (session
parity) has begun, §11: the controller takes effects and modulators out of the chains (step 17) and the
page saves the session (18), puts effects (19) and instruments (20) in the chains, loads presets (21), reaches every
parameter of a plugin (22), sets a lane's keys (23), has the piano (24), moves the nodes of the canvas, keeping where
they are in the session (25), draws cables between modulators (26), shows what a modulator puts out (27), moves
effects along their chains (28), draws the shape of an envelope (29) and lets a person choose the knobs of a node (30);
steps 31 to 41 followed (groups from the page (31), undo in four steps (32 to 35), cables to the depth of a cable (36), a modulator made for a
knob from the knob (37), Save as (38), the ring and the dot of a setting that a cable drives (39), the light for a request that is slow (40) and the ring of a
parameter that a lane's and its group's modulators both drive (41): see "Frontend" above and `docs/web-ui.md` §12).
TODO: snapping to a grid. (The arrow keys move what is chosen since step 107.)
TODO: make the Type row (LFO to envelope) an intent: until then a browser is given a snapshot
for it (`Controller::resync`).
TODO: the canvas numbers the nodes of one name in the order of the graph (`displayNames`), so an effect that is moved past one
of its name changes name with its place (Filter 3 becomes Filter 4, and the one it passed Filter 3): a number that stays with
the node, by its id, would keep it, but would number a loaded session's nodes out of the order they are drawn in.
TODO: a trace of an LFO faster than 15 Hz aliases: the telemetry is 30 values a second (the engine's blocks are much shorter); a telemetry
channel of the lowest and highest output of each block, or a faster one, would draw it.
TODO: the light for a slow request says that the server is slow and not what it is doing (which plugin it loads). The terminal says it for a plugin chosen in its
selector ("Loading Pianoteq 9…") but not for one that a browser asks for, nor for an undo that loads one again: its loop is blocked then and draws nothing for
as long as the load takes. Moving the loads to a worker (`docs/web-ui.md` §3.1) would end it.
TODO: the bar under the slider of a cable whose depth is driven says where the engine has the depth, but not how far the cables that drive it can swing it (a knob's ring); a
slider has no room for one, and the bar's scale is the slider's whole width while the thumb's travel is inset by half a thumb at each end.
TODO: replacing an instrument takes out every cable that drove the old one; keep the ones whose
parameter the new plugin has under the same name (a vibrato on `detune` could survive a change of
oscillator): a cable would have to be drawn again with the new plugin's index.
TODO: telemetry slots are never reused (4,096 of them): a session that adds modulators and cables
for long enough runs out, and later cables have no live dot.
TODO: pinning from a knob on the node itself (a pin is set in the panel only), ordering the pins by dragging (they are shown in the order they
were pinned), and a node with no knobs at all (an empty `pinned` list is the front-end's own choice, the first eight; docs/web-ui.md D33).
TODO: the panel's sliders ignore what a parameter says of its steps (`stepCount`) and of its group (`group`): a stepped
parameter is turned as if it were continuous, and the filter looks at names only. (The VST3 adapter makes a parameter of 1 to 64 steps a list
of names, an enum, so that it is not turned as a number; what is left is one with more steps, and the groups, which no plugin of this machine reports.)
TODO: a lane added from the browser is full range and in no group (its own choice of group puts it in one; drawing its audio to a group is
not built), and an instrument put in from it starts at its own defaults (or at a preset the page then loads): taking a lane or a group out
asks first (see the TODO on the words of its "×").
TODO: the slider of a cable's depth is at the middle of the cable, which can be over a node's
edge, and two cables with the same middle put theirs over each other; sliders that move along the
cable would settle it (nodes can be placed since step 25, which helps but does not settle it; a cable that ends at a depth has its slider on the
run under what drives it, since step 36).
TODO: patterns as graph nodes (a lane's pattern is on the lane's node, as a `PatternInfo`, but has no `NodeId` of its own).
TODO: the page does not draw the notes of a pattern (a snapshot has how many there are), nor says where a recording is: it knows that one goes on, and not whether it is counting
in or recording, nor how far it is (the engine reports its end and nothing before it); a telemetry channel or an event for the phase of a recording would give a progress bar.
TODO: events of their own for what the audio thread reports to the controller (a
finished pattern recording, a captured MIDI-learn control) and for what the TUI
changes without an intent: until then connected browsers are given a snapshot
(`Controller::resync`).
TODO: the events of one intent share its revision, which a client takes for a repeat
if there is more than one (every intent makes one event today).
TODO: undo of what moves between groups: a group made of a lane that is in another empties
the history until its step exists (`Plan::Barrier` in `src/app/history.rs`, `planOf` in `web/src/client/history.ts`; a lane put in a group or out of one has been a step since step 96).
TODO: the TUI's edits that have no intent (the Type row) are not in the history, and do not empty it; the order of the lanes with
Shift+Up / Shift+Down on a lane does (a step that took something out puts it back by the places it had).
TODO: a cable that a file has to a cable that is not there (`mod_depth = [0, 0]` to a modulator with no cable) is centred on something else after undo of the
removal of its modulator than the loader gave it; it drives nothing, so it makes no difference to what is heard, and the tests leave it out.
TODO: the "×" of a lane and of a group asks first, in words that say it goes "for good" (`RemoveLaneOrGroupMenu`), from when a removal could not be undone: both can be
now; whether to keep the question is the user's to say, and the words are to be changed either way. (For a lane that has an instrument it offers taking the instrument out alone first: step 85.)
TODO: an all-notes-off for a lane that comes back by undo (and for the members of a group that does, and for an instrument that is swapped in or out): a note that was held when it was taken out stays held in a plugin that
was not sent the note-off. (The panic, `Ctrl+P` or the page's button, ends such a note by hand.)
TODO: the controller's layout of the telemetry lists a cable by the place of its modulator in the rack, and the mock engine's lists one that is drawn after
the others: a cable of an earlier modulator is told in another order (no reader depends on it; the fixture's scripts draw from the last modulator of a rack).
TODO: the TUI's `n` and then a choice from its selector is two steps, an add and a replacement: it takes two Ctrl+Z.
TODO: keep-alive pings for WebSockets.
- **Step 115, part A: the bar's `Open…` (a session file picked in the page, loaded in place of the session).** *The user's "also we need to be able to load a sound from within the UI, i.e. load a toml".* A button after `Save as…` (`web/src/app/OpenSession.tsx`), there only when the hello says `openSession` (`canOpenSession` in `client/selectors.ts`: a plugin in a host; a program that is started with its file has none, and says so if the intent is sent). It opens the browser's own file chooser (the web view of a plugin's window has one: wry's `runOpenPanel` on macOS, WebView2's own on Windows) for `.toml` files, reads the file **in the page** and sends its text as one intent (`Editor.openSession(text, name)`, `Editor.openSessionFile(file)`: a file that cannot be read, and one over 4 MiB of text, are said among the notices and not sent; the server reads a message of up to 8 MiB from a connection that has been let in, and the text goes as JSON, where a quote and a line's end take two bytes). The server's answer is a notice if it was refused (`Could not open "pluck.toml": that is not a session: <why>`); if it is taken, the plugin makes its engine again with the session **and the page that sent it is served by the new engine, at another address**: a plugin's window follows it (`refresh_gui`), a browser tab that was opened on the old address is left with a connection that does not come back (open the page again from the device's `Open the page`). The button is disabled while the file is being read and sent. No confirmation is asked, and nothing is kept of the session that is replaced (no Undo). The mock engine takes the intent when it is made with `openSession: true` and is as it was. Held by Vitest (the editor's five tests, the selector's three, the button's six), and for real by the plugin tests (`tang-clap/tests/window.rs`: through the real socket) and by a run of the debug plugin in `tang window` (see `docs/reference/plugin-host.md`).
TODO: the places that the layout remembers are the page's: a page that is opened again, and a second browser, lay the nodes that no person has placed out from nothing, so that the canvas is not what it was when it was left (docs/web-ui.md D45, Open question 6).
TODO: until the nodes are arranged a cable between two modulators that are in one row runs across what is between them, and its slider can be over a node (the cables' routes are made for the layout from nothing).
TODO: the view moves to show a node that is added where it cannot be seen, and may leave the others out of it.
