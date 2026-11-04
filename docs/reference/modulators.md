# Modulators

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): the sources and their laws, cables by node and port, what a modulator drives besides plugin parameters. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

### Modulator fields

(Since step 94 the program moves the modulators of a lane and of a group that a file has into the session's own rack when it loads the file, each hearing the lane or the group it was in, and saves them
as `[[modulator]]` with `hears`: see "Free modulators", "Every modulator is global"; since step 96 there is no other rack. What follows is **what the older file's `[[instrument.modulator]]` says**, which is read for ever, and what every modulator does.)

Modulators are block-rate sources. A lane's modulator, in the older file form, belonged to an
instrument and drove parameters anywhere in that instrument's chain — the
instrument itself or any of its effects — plus sibling modulators; the loader makes it a modulator
of the session's own that hears the lane, with the cables it had, and **a modulator of the session's own can drive any plugin of the session**, and since step 97 the volume, the mix and the pan that the host keeps for a lane, an effect and a group (see "The volume, the mix and the pan that a modulator drives"). Each target in the older form
names the chain `slot` it lands on: `0` (default) = the instrument, `1..N` =
the effects in order (the new form names the plugin by its place in the session: `to = "instrument[0].effect[1]"`). In the TUI, a modulator is listed under the lane or the group
it hears (magenta; the ones that hear every note come first), and the target selector lists every parameter of every plugin of the session, each labelled by the lane or the group it is in
(e.g. `L1 Filter: cutoff`). Five source types are supported
(LFO, ADSR envelope, MIDI learn, macro, step sequencer):

(Groups' modulators, in the older form, had the same source types and cross-mod, with targets across the whole group: see "Group modulators" above. Every modulator now has the whole session for its reach: see
"Free modulators".)

**LFO modulator** (default):

```toml
[[instrument.modulator]]
type = "lfo"            # optional, "lfo" is the default
waveform = "sine"       # sine, triangle, saw, square, random, smooth (default: sine)
rate = 0.5              # Hz (default: 1.0; not below 0: a file with a negative rate is refused)
sync = "1/8"            # follow the tempo: a cycle is this note length, and `rate` is kept and not used (default: free; see "Tempo sync")
retrigger = true        # a note starts it again at the start of its wave (default: false: it runs free)
fade = 0.5              # seconds it takes to come in from nothing after a note, 0 to 5 (default: 0: none)

[[instrument.modulator.target]]
param = "cutoff"        # parameter name (resolved within `slot`'s plugin)
slot = 1               # chain slot: 0 = instrument (default), 1..N = effect
depth = 0.5             # fraction of param range, −1.0–1.0 (default: 0.5).
                        # Negative depth inverts the direction (LFO in
                        # anti-phase; a control that lowers instead of raises).
```

The shapes are `sine`, `triangle`, `saw` (rising), `square` and two that have a number of their own for every
cycle of the LFO: **`random`** (also `s&h` or `sh`: sample and hold) puts out the number of the cycle it is in, from
−1 up to but not including 1, and holds it until the next cycle begins; **`smooth`** glides from the number of
each cycle to the number of the next (a smoothstep, so that it eases at both ends and the cycles join without a
corner). Like the others they are bipolar, so the law of an LFO below is the same for them. The numbers are
not drawn from a generator that has state: a number is the top 24 bits of a hash of the modulator's *seed* and of how
many times it has gone round (`plugin::chain::noise`), so the same cycle of the same seed always has the same number, and
a session that is loaded again plays the same numbers. The seed is the order in which modulators came into the audio graph
(one counter, `AudioGraph::next_modulator_seed`: the lanes' modulators and the groups' came from two racks until step 96): a session loads
its modulators in the order of its file, so each has the seed it had the last time, and one that is added later, or put
back by undo, has the next one and so draws other numbers than it did before (the seed is not in the session file). The
cycles are counted as the phase wraps (`Modulator::tick`), so that turning the rate does not start the numbers again, and a rate
that goes round more than once in a buffer counts every cycle. The page's mock engine draws the same numbers from the same hash
(`web/src/client/noise.ts`, pinned to the engine's by a table that the engine writes, `fixtures/noise.json`), with the node's
id for the seed.

**An LFO's key** (step 66, `docs/web-ui.md` D66): two settings that follow the notes, both off in a file that has neither (a session that was written before them is the sound it was, to the last bit: `a_note_changes_nothing_of_an_lfo_that_neither_retriggers_nor_fades`). With `retrigger = true` a note (a note-on with a velocity, on any channel: the notes of the lane after its range, its remap, its pattern and its transpose, or those of any member for a group's LFO, as a step sequencer's retrigger and an envelope hear them) puts the LFO at the start of its wave again: in the buffer that has the note it puts out what the wave is at its start, and it goes on from there at its rate (`a_note_starts_an_lfo_that_retriggers_again_at_the_start_of_its_wave`), and a `random` or `smooth` wave takes the next number, since a note is the start of a new cycle (`a_random_lfo_that_retriggers_takes_the_next_number_with_every_note`). With `fade = N` (seconds, 0 to 5, held to it when a file is loaded) what it puts out is multiplied by the part of N that has gone by since the last note (`age / fade`, at most 1), so that it comes in from nothing: it is at full until the first note, every note starts the fade again from nothing whether or not the wave starts again, and what is published (the node's trace, the dots of the knobs it drives) is the faded output (`an_lfo_with_a_fade_rests_at_full_until_a_note_and_then_comes_in_over_the_fade`, `a_note_starts_the_fade_again_whether_or_not_the_lfo_retriggers_and_the_wave_of_one_that_does_not_goes_on`). Both are applied once a buffer, as every modulator is: a note is heard at the start of the buffer that has it. In the engine they are `ModSource::Lfo { retrigger, fade, age }` (`ModSource::lfo` and `with_key` make one, and `GraphCommand::SetModulatorKey` sets both of a lane's or a group's LFO and leaves it where it is in its wave), in the model `ModSourceSlot::Lfo { retrigger, fade }`, and in the intents `ModulatorField::Retrigger` (the switch a step sequencer has too) and `ModulatorField::Fade` (`SetModulatorField`: held to 0 to 5, the switch rounded, a step of the history). No cable ends at either (they are a person's, as a step sequencer's glide and length are): no port, no telemetry slot, no cross-mod field. In the file they are `retrigger = true` and `fade = 0.5` on an `lfo` modulator, written only when they are not what they are without them; the key `retrigger` is a step sequencer's too and its default is the kind's (a note starts a step sequencer again unless the file says it does not, and does not start an LFO unless it says it does: `ModulatorConfig.retrigger` is an `Option<bool>` that the loader reads by kind). The LFO's pane has them after its rate and its note length (Type, Waveform, Rate, Sync, Retrigger, Fade, then the targets). The page has a choice `Retrigger` and a knob `Fade` (`none` at 0) after the rate and the wave on its node, and the mock engine puts out the same (the wave from the note that started it again, the fade from the last note).
TODO: a retriggering LFO starts at the start of its wave and nowhere else (a start phase would do it), and its fade is a straight ramp. The mock engine's random LFO that retriggers takes the same number at every note where the engine's takes the next (by its code, `shape` of the cycles since the note: not tested).

**Tempo sync** (step 67, `docs/web-ui.md` D67): an LFO and a step sequencer can go by the session's tempo in place of a rate in hertz. With `sync = "1/8"` a cycle of the LFO (a step, for a
step sequencer) is an eighth note long, so its rate is what the tempo makes of it, `bpm / (60 * beats)` cycles a second for a note length of `beats` beats (`plugin::chain::rate_at_tempo`: an
eighth note is half a beat, 4 Hz at 120 beats a minute), and it goes as fast as the tempo does when the tempo is set again (`an_lfo_that_follows_the_tempo_goes_by_it_as_it_changes_and_one_that_does_not_goes_on`).
The sixteen note lengths are `SYNC_DIVISIONS` (`src/app/model.rs`), grouped by note from the slowest to the fastest and, within a note, dotted (×1.5), plain and triplet (×2/3): `4 bars`, `2 bars`,
`1 bar`, `1/2 dotted`, `1/2`, `1/2 triplet`, … `1/16 triplet`, `1/32` (so not strictly by length: a dotted quarter, 1.5 beats, is longer than a half-note triplet, 1.33); a bar is four beats, since a session
has no time signature. In the model `sync` is a place in that list from 1, and 0 is none (`ModSourceSlot::Lfo` and `Steps`, `sync_name`, `sync_beats`); in the engine it is the beats (`ModSource::Lfo { sync, .. }`
and `Steps { sync, .. }`, 0 for none; `GraphCommand::SetModulatorSync` sets a modulator and leaves an envelope, a macro and a MIDI control alone), and every modulator is ticked with the tempo
that the graph has (`AudioGraph.bpm`: 120 until a `SetGlobalBpm` says another, which takes a finite number above 0 only, see "Tempo"), so that a tempo that is set is heard at the start of the next buffer.
The rate that the modulator has is **kept, untouched, while it follows the tempo**: `sync` overrides it in the one place that reads it (`effective_rate`), the model has no second copy of it to keep in step,
a change of the tempo needs no event to any modulator, and taking the sync off, or undoing it (`a_sync_is_a_step_in_the_words_of_the_setting_and_is_put_back_with_the_rate_the_lfo_keeps`), gives the rate it
was. A cable to the rate of a modulator that follows the tempo drives nothing (the engine goes by the tempo; the page's knob has no ring and no dot then). The setting is `ModulatorField::Sync` (0 to 16, a
whole number, `SetModulatorField`: a step of the history, "set sync"; `ModulatorRow::Sync` in the panes) and no cable ends at it (it is a person's, as the retrigger is: no port, no telemetry slot, no
cross-mod field). In the file it is `sync = "1/8 triplet"` on an `lfo` or `steps` modulator: the name is read without regard to case and to the spaces in it and round it, `free`, `off`, `none` or nothing
at all is free, a name that is not one is free and logged (as a waveform that is not one is), and a save writes the key only when the modulator is not free, so that a session that was written before it is
the sound it was and is saved again without it (`a_modulator_that_follows_the_tempo_is_written_with_the_name_of_its_note_length_and_read_back_the_same_and_one_that_is_free_has_no_sync_key`). Both kinds
are labelled by the note length and not the rate they keep (`LFO 1/8 sine`, `Steps 16 1/16`). In the protocol it is `sync: number` on the `lfo` and `steps` sources, and the generated constants have
`SYNC_DIVISIONS` (the names and the beats) and `SYNC_FREE`.
The terminal's pane has a `Sync` row after `Rate` for both (a list: `Left` and `Right` go a note length along it and stop at its ends, free first) and dims the `Rate` row while the modulator is not free
(`RowLook::Unused`, as a step past the length is). The page has a choice `Sync` in the header of the node, in a line with the name and the button that takes the node out (`SyncControl`, `Choice inline`): not
a cell of the grid, which has room for the four controls of a row and for names of a few letters (`1/16 triplet` was cut in a cell of one column, and a fifth control made the LFO's and the step sequencer's
nodes a row taller than the layout assumes, which the browser suite's checks of the sizes caught: `describe.settingsOf` has no `sync`). The knob of the rate in the node shows the rate that the tempo makes of
the note length (`rateAt`, `web/src/client/tempo.ts`, pinned to `rate_at_tempo` by `fixtures/sync.json`, which the engine's own numbers write), moves when the tempo does, and cannot be turned until the
modulator is free again, when it is the rate it kept. The mock engine's LFO and step sequencer go by the same law.
Three examples go by the tempo (`arp.toml`, `wobble.toml` and `gate.toml`, see "Example sessions"), each held by a test that renders it at two tempos.
TODO: the speed follows the tempo and the phase does not: the engine has no transport (no playhead, no bar to count from), so an LFO of a bar is as long as a bar and starts wherever it was (with `retrigger`,
at the note), not on the beat; locking it would take a position in the song. A cable to the rate of a modulator that follows the tempo does nothing (it would have to move the note length, which is a choice).
Not here: a delay, an arpeggio or a pattern that follows the tempo (the delay's `time` is in seconds; the pattern is Open question 10 of `docs/web-ui.md`), other note lengths (a 64th, a swing) and a time signature.

Modulators serialize under `[[instrument.modulator]]` (the instrument
level), never under an effect, when the model has one in a lane (a session that is loaded by the program has none since step 94, which writes them
as the session's own, `[[modulator]]` with `hears`: see "Free modulators"). Older sessions that nested `[[...effect.
modulator]]` still load: those modulators migrate into the lane with their
targets pinned to that effect's slot. They belong to the lane, not to its
instrument plugin: a lane with no `plugin` (what `d` on the instrument of a lane that
has effects leaves) keeps its effects and its modulators through a save and a load. A
target on slot 0 of such a lane has no parameter to land on, and a load skips it with a
log line.

**ADSR envelope modulator** (triggered by note-on/off):

```toml
[[instrument.modulator]]
type = "envelope"
attack = 0.01           # seconds (default: 0.01)
decay = 0.3             # seconds (default: 0.3)
sustain = 0.7           # level 0.0–1.0 (default: 0.7)
release = 0.5           # seconds (default: 0.5)

[[instrument.modulator.target]]
param = "cutoff"
depth = 0.5
```

**MIDI-learn modulator** (a hardware CC, the pitch-bend wheel, or key
pressure / aftertouch drives the target; or the velocity or the pitch of the
notes the lane plays):

```toml
[[instrument.modulator]]
type = "midi"
midi_cc = 74            # bound CC number …
# midi_pitch_bend = true  # … or the pitch-bend wheel …
# midi_aftertouch = true  # … or key pressure (channel pressure / poly
                        # aftertouch — the "press harder" dimension on MPE
                        # controllers) …
# midi_velocity = true  # … or the velocity of the last note the lane plays …
# midi_key = true       # … or its number (the pitch). The five are mutually
                        # exclusive (the first of them, in this order, wins);
                        # omit all = unbound (re-arms for learning on load).

[[instrument.modulator.target]]
param = "cutoff"
depth = 1.0             # full deflection = ±depth·range around your set value
```

Create one from the TUI with **`l` (learn)** on a selected parameter (in the
param pane): it adds an armed MIDI modulator bound to that parameter, and the
next incoming CC / pitch-bend / aftertouch on any channel is captured and bound
(the audio thread notifies the main thread, which fills in and persists the
binding). It targets anything the param pane can select — plugin params
(instrument, effect, group-bus effect) and modulator fields (bind a knob to an
LFO's rate, an envelope stage, or a target's depth). In the tree it shows as
`CC 74 → cutoff` / `Pitch Bend → …` / `Aftertouch → …` / `Velocity → …` /
`Key → …` (or `MIDI (learning…)` while armed). `t` and `d` work on it like any modulator. A learned control
*combines* with your set value (see the apply math below) rather than
overwriting it. The whole path (a controller, the MIDI thread, the audio
thread's capture, the notification, the controller's binding, the browsers told,
the parameter following the knob, the saved `midi_cc`) is tested with a virtual
MIDI port in `tests/serve.rs` (`a_control_that_learns_…`: `#[ignore]`d, since it
needs CoreMIDI on a Mac or ALSA's sequencer on Linux: `cargo test --features web
-- --ignored`).

**Macro modulator** (a knob that a person turns, which nothing else moves):

```toml
[[instrument.modulator]]
type = "macro"
value = 0.5             # 0 … 1 (held to it when the session is loaded; default 0: at rest)
name = "Brightness"     # what it is called (default none: the node is called `Macro`); the control characters and the spaces round it go, and it is cut at 64 characters, when the session is loaded
expose = false          # whether the box of a module that holds it shows its knob (default true; written only when false): step 110

[[instrument.modulator.target]]
param = "cutoff"
depth = 0.5             # `value` times this, of the range, above your set value
```

A macro is one number, `value`, from 0 to 1, that is set from the TUI (the first row of its pane: `Left` / `Right`, or `Enter` and a number), from the page (a knob
on its node) or in the file, and it drives every target that has a cable from it as a CC does: at rest (0) it adds nothing, at full (1) it adds `depth` of the
range above the value the user set (a negative depth takes it below). A new one is at rest, so it moves nothing until it is turned. One knob drives many parameters,
each scaled by the depth of its own cable; a macro can drive the rate, a stage or the depth of a cable of another modulator too (cross-mod). The audio thread has
nothing of a macro but its value, so a change of it is a macro of the new value given to the engine in place of the old one (`SetModulatorSource`, `controller::macro_command`; until step 96 `SetGroupModulatorSource` was a group's). It is made in the TUI from a parameter (`m`: `New macro → cutoff`; hearing the group for a bus effect's
parameter), in the page from the menu of a knob (`~`: "New macro") or from the "+" of a lane or a group ("Macro"), by `Modulate` / `AddFreeModulator { kind: macro, hears }`, and
is `~ Macro 0.50 → Filter: cutoff (50%)` in the tree (`Macro Brightness 0.50` for one that is called something) and `Macro` (its value, the "Targets" separator and the depth of each cable) in the pane.
A macro can be called something and can be told not to show its knob on the box of a module that it is in (`RenameMacro`, `ExposeMacro`, step 110: see "A macro that has a name and a knob that a box shows" in `docs/reference/modules.md`); neither is the engine's, and both are in the file (written only when they are not what a macro starts as) and in the library of modules.
TODO: no cable drives a macro's value (`ModulatorField::Value` has no port, and the pane's `m` on its row opens nothing): a MIDI control bound to a macro would turn it
from a hardware knob, which is what a macro is for on a stage.

**Step sequencer modulator** (sixteen bars that go by at a rate, and drive what they have cables to by the value of the bar they are at):

```toml
[[instrument.modulator]]
type = "steps"
steps = [0.0, 0.5, -0.25, 1.0]  # 1 to 16 values, each -1 … 1 (held to it when loaded); a step that is not given is 0; no list at all is a staircase
length = 4                      # how many steps are in use, 1 … 16 (default: as many as `steps` has values, sixteen if none)
rate = 8.0                      # steps a second (default 8: an LFO's default is 1, so `rate` is an option in the file's types and the loader says which)
sync = "1/16"                   # follow the tempo: a step is this note length, and `rate` is kept and not used (default: free; see "Tempo sync")
glide = 0.0                     # 0 … 1: the part of each step, at its end, that goes to the value of the next (default 0: hard steps)
retrigger = true                # whether a note starts it again at its first step (default true)

[[instrument.modulator.target]]
param = "cutoff"
depth = 0.5
```

A step sequencer goes through its first `length` steps at `rate` steps a second, and round again, and is the value of the step it is at (or, in the last `glide` of a
step, on its way to the next one's; the last goes to the first, and not to a step that is not in use): `steps_output` in `plugin::chain`, which the page's `stepsOutput`
(`client/steps.ts`) is pinned to by a table the engine writes (`fixtures/steps.json`). It drives as an LFO does, bipolar: `base + value * depth * range`. A new one
(TUI `m`: "New step sequencer"; the page's "+" menu on a lane or a group and the `~` menu of a knob; `AddFreeModulator` / `Modulate` with the kind `steps`) is a staircase from
-1 to 0.875 in sixteen steps, at 8 Hz, with no glide, restarted by a note and free of the tempo (see "Tempo sync"). Its rate can be driven by a cable (the rate has the port an LFO's has: `ModulatorField::Rate`);
the glide, the length and the retrigger are a person's, and no cable ends at them. The steps past the length keep their values, so that a sequence that is shortened and
lengthened again is as it was. With `retrigger` on any note-on with a velocity, on any channel, puts it at its first step (a lane's: the notes the lane takes; a group's: the
notes of any of its members). Where it is, the step as the whole part of the number and how far into it as the fraction, is published for a playhead in a telemetry slot
of its own after its output's (`ModulatorSlot::position`, `SlotKind::ModPosition`, `SetModulatorPosition`: `None` if the slots had run out, and then neither the page nor the terminal has a playhead). The step it is at is `step_at(length, position)` (`plugin::chain`:
the whole part of the position brought round at the length, which `steps_output` is made from too), which the page's `playheadStep` is pinned to by the same fixture (`fixtures/steps.json`, its `steps` list).
All sixteen values are one intent, `SetSteps { node, values }`, told as `StepsSet` (each held to -1..1; a list that is not sixteen long, a value that is not a number and a node
that is no step sequencer are refused, with nothing changed), so that a stroke across the bars is one step of the history ("set steps", `Key::Steps`); the engine is given
them with the length, the glide and the retrigger in one command (`SetModulatorSteps`, `controller::steps_command`) and the rate by the LFO's (`SetModulatorRate`). In the tree
it is `~ Steps 16 8.0Hz → Filter: cutoff (50%)` (the steps in use, and the rate, or the note length when it follows the tempo: `Steps 16 1/16`), and its pane, "Step sequencer", has the rate, the note length (`Sync`), the glide, the length, a retrigger row (off and on, a list), a row for each of the sixteen steps
(`Left` / `Right` move one by 5% of its range, and `Enter` takes a number, held to -1 … 1) and the targets. The page draws it as a node with four controls (and the choice of a note length in its header) and sixteen bars with a playhead
(`ui/StepBars.tsx`, see "Web UI").
The terminal has both too: the step that the sequencer is at is marked in its pane (`Step 3 ▶`, magenta) and the steps past the length are dimmed (`RowLook` in `src/tui/mod.rs`;
`State::selected_playhead` reads the position from the telemetry on every frame that is drawn, about ten a second, so a sequencer at 8 Hz is seen to hop about a step a frame). The row under the
cursor keeps its look and is bold (a step past the length bold and dim, the step it is at magenta and bold); a sequencer whose telemetry slot ran out has no marker.
TODO: a step is held to a hundredth when it is painted on the page, which is a hundredth of the range of what it drives at depth 1 (a detune of ±24 semitones is 0.48 of a
semitone, and 0.12 at depth 0.25: out of tune by twelve cents; the terminal's `Enter` takes a number to as many places as a person types): a sequencer for a melody wants a
grid of semitones for what it drives (its rate can follow the tempo since step 67, see "Tempo sync"; the oscillator's `detune` has the grid since step 69, its `quantize`, see "Built-in plugins": chromatic
is a grid of semitones, the others hold it to a scale; a pitch parameter of another plugin has none).

Modulators are applied once per audio buffer; the base value tracks the
user's set value automatically. Target `depth` runs **−1.0 … 1.0**: negative
depth flips the sign of the offset below, inverting the direction (an LFO in
anti-phase, a control that lowers the parameter instead of raising it, an
envelope that dips below the base). The source types apply differently:

- **LFO** (bipolar output -1..1): `base + output * depth * range` — wobbles
  ±depth around the user's value.
- **Envelope** (unipolar output 0..1): `base - depth * (1 - output) *
  (base - min)` — the user's value is the envelope's **peak**. The parameter
  rises from below up to the base as the envelope opens; with depth 1.0 it
  spans the full min→base distance. This is what makes an amp envelope work:
  volume at 1.0 with an envelope at depth 1.0 gives silence→full→sustain→
  silence. (A purely additive envelope could only push values *above* the
  base, which on an already-maxed parameter clamps to no effect at all.)
- **MIDI learn** (additive around the base, like the LFO): the control
  *offsets* the parameter around the user's set value rather than overwriting
  it, so it combines with the base and any other modulators (an unbound /
  still-learning control contributes nothing — the base shows through; nor
  does a bound one that has sent nothing yet: it starts at rest, a CC or
  pressure at 0 and the pitch-bend wheel at its centre, `MidiBindSource::rest`).
  The rest-aware laws by source:
  - **Pitch bend** (bipolar): `base + (output − 0.5) * 2 * depth * range` —
    centre = no change, full bend = ±depth·range around the base. This makes
    the wheel behave like a pitch/mod wheel and is the natural mapping for
    expressive (MPE) controllers.
  - **CC** (unipolar): `base + output * depth * range` — rest (0) = no change,
    full = +depth·range above the base.
  - **Velocity** (unipolar, as a CC): the velocity of the last note-on the lane
    plays, over 127; `base + velocity/127 * depth * range`. Before a note it is
    at rest (0), and it keeps the last note's velocity when the note ends (a
    release goes by its own note's velocity). A note-on with no velocity is a
    note-off and does not count.
  - **Key** (bipolar around middle C): the number of the last note-on over 127,
    `base + clamp((note − 60) / 60, −1, 1) * depth * range` — note 60 (middle C)
    adds nothing and five octaves either way are the whole depth (`KEY_CENTRE`,
    `KEY_SPAN`; the page reads both). It rests at note 60 before any note.

  Velocity and key are not learned (no controller sends them as a control): they
  are bound when they are made: `m` on a parameter in the TUI ("New velocity" /
  "New key"), the page's "+" menu on a lane or group and the `~` menu of a knob
  ("Velocity", "Key"; the intents `AddFreeModulator` / `Modulate` with the kinds
  `velocity` and `key`), or `midi_velocity` / `midi_key` in a file. Like
  every modulator they are block-rate and lane-global: a chord has its last
  note's. The notes they hear are the ones the lane's instrument is given: after
  the lane's range, remap and transpose, and a pattern's own notes. A group's
  hear the notes of its members' ranges.
TODO: the keys of the computer play every note at 100 (a key event has no
velocity), in the page and in `play`; only a MIDI keyboard and the page's piano
(by where the pointer is on a key) give a velocity control more than one value.
TODO: a key control is centred on middle C and spans five octaves for everyone
(`KEY_CENTRE`, `KEY_SPAN`): to track over fewer octaves a person scales the depth
of the cable, and the centre cannot be moved.
- **Macro** (unipolar, additive around the base, as a CC): `base + value * depth *
  range` — rest (0) = no change, full = +depth·range above the base.
- **Step sequencer** (bipolar, additive around the base, as an LFO): `base + value *
  depth * range`, where `value` is the step it is at, -1 to 1 (see "Step sequencer modulator").

Envelope behavior: note-on retriggers from Attack phase (continuing from the
current level — a quick retrigger does not restart from silence). Release
begins when all held notes are released. Linear ramps for A/D/R phases.

**Taking a cable or a modulator out** (TUI `d`, the browser's "×"; both through
`Controller::remove_edge` / `remove_modulator`) gives what it drove the value the
user set: a cable writes into its target on every buffer, and nothing else would
write the user's value back, so the parameter (or a modulator's rate, a stage of
its envelope, the depth of a cable) would stay where the cable last put it. The
values are the model's, sent as ordinary `SetParameter` / `SetModulatorRate` /
`SetModTargetDepth`… commands after the removal; a target that another cable still
drives is left to that cable, and one that went with the modulator is given
nothing. What drives what is named by node and edge ids (`Driven`), which survive
the removals around it, not by places in a rack.

**Taking an effect, a lane or a group out** (TUI `d`, through `Controller::remove_effect` /
`remove_bus_effect` / `remove_lane` / `remove_group`) takes the cables that point at what
goes out first, each by the same path as above (with the cables that drive their depths),
and then the plugins, which the audio thread hands back. For an effect or a bus effect that
is the cables that drive its parameters, whichever modulator they start at; for a lane, the cables that drive its instrument's and its effects' parameters (a modulator that
hears the lane stays, hearing every note: step 94's D3, and step 96 has no modulator that goes with the lane); for a group, the cables that drive its bus effects' parameters (the modulators that hear the group stay, hearing every note, and what their
cables drove in the members is untouched, since a cable names its plugin). What they drove goes with the plugin or the lane, so nothing is
given back. (Until step 94 a group's own cables took the value the user set back to what they drove in the lanes that stayed.) The cables to the
effects after a removed one name their plugins by node, and the audio thread finds them there, so
there is nothing to tell them. A
session keeps at least one lane: the last one is not taken out. `d` on an instrument that has
a plugin takes the instrument out of its lane (the intent `ClearInstrument`, a step of the history: the cables that drove the plugin's parameters go first, and so does the
lane's note remap, which the audio thread drops with the plugin; the lane stays, empty, with its effects, which go on running, the modulators that hear it and the rest) and a second `d` takes the lane out.

**Limitation:** a modulator envelope shapes *parameters*, not voices — it
cannot make a note ring past the instrument's own release. Once the plugin
stops its voice (the built-in oscillators hold notes ~12 ms past note-off by
default), modulating a volume parameter has nothing left to attenuate. For
amplitude envelopes on the built-in synths use their native
`attack`/`decay`/`sustain`/`release` parameters; envelope modulators are for
sweeping parameters (cutoff etc.) while notes are sounding.

**Cross-modulation** — a modulator can target sibling modulators' parameters
instead of (or in addition to) plugin parameters:

```toml
# Target a sibling modulator's LFO rate:
[[instrument.modulator.target]]
mod_rate = 0            # index of sibling modulator
depth = 0.3

# Target a sibling modulator's envelope parameters:
[[instrument.modulator.target]]
mod_attack = 1          # index of sibling modulator
depth = 0.5

# Target a sibling modulator's target depth:
[[instrument.modulator.target]]
mod_depth = [0, 0]      # [mod_index, target_index]
depth = 0.2
```

Cross-mod target fields (`mod_rate`, `mod_depth`, `mod_attack`, `mod_decay`,
`mod_sustain`, `mod_release`) are mutually exclusive with `param`. Self-
modulation (targeting own index) is prevented.

A cross-mod cable works from the value the user set the field to (an LFO's rate,
a stage of an envelope, the depth of a cable) and writes it every buffer, the
last cable to write it standing: when the user sets the field the engine moves
what the cables that drive it work from (`AudioGraph::centre`, which replaced `update_cross_mod_base` and its three siblings at step 90: for a depth,
since f133, which had been left out, so that the cross-mod wrote the depth back
to what it was around the old value every buffer, and the slider did nothing).

A field that is turned on a log scale (the knob of an LFO's rate, 0.01 to 50 Hz,
and of an envelope's attack, decay and release, 0.001 to 10 s, runs over decades:
`ModulatorField::taper`) is moved on that scale, so the cable's `depth` is a
fraction of the knob's travel and not of the range in seconds or hertz. The
user's value is put at its place on the knob (`ln(value / min) / ln(max / min)`),
the source offsets that place by the laws above (an LFO plus or minus `depth`, an
envelope from `depth` of the way down to the value the user set, a MIDI control
as before) with the knob's travel, 0 to 1, as the range, and the result goes back
to a value (`Modulator::modulated_value`, which `apply_cross_mod` and a group's cable to
a member's modulator use). A swing is as far up the knob as down it: an LFO at
depth 0.5 on an attack of 10 ms goes from 1 ms to 1 s (it went from 1 ms to 5 s,
and sat at 1 ms for half of each turn). A sustain and the depth of a cable are
levels and go by the range, as does a range that starts at nothing (there are no
decades in it). A saved session's cross-mod cable to a rate, an attack, a decay or a
release means a fraction of the knob now, and no longer of the range.

### Cables by node and port (`ModTargetSlot::aim` and `ModTarget::aim`, steps 89 and 90, P5a and P5b-1)

A modulation cable of the model names what it drives by the node and the port (`Driven`: a plugin's parameter by the node of the plugin and the parameter's own index, a modulator's
rate or a stage of its envelope by the node of the modulator, the depth of a cable by its edge) and not by a place in a rack (a slot of a chain, a member's ordinal in a group, a sibling's
index, a cable's place in its modulator): it stays on what it drives while the plugins, the lanes, the members of a group and the modulators around it come and go. P5a of
`docs/patch-graph.md` §11.6 made it so, and took out the twenty functions that kept a place in step with the structure in the model (`shift_*`, `fixup_*`, `remap_*`: 304 lines of
`model.rs`, with the six lookups by place that went with them and fourteen of their tests): a cable is told no new place, because it has none. P5b-1 (step 90) did the same for the audio thread (below): its
cables name the node and the port too, and its 32 place-keeping functions are gone.

(Step 96 took the lanes' and the groups' racks out of the model, the engine, the controller, the terminal and the page: there is one rack, the session's own, and `ModTargetKind` has only `FreeParam` and the siblings' kinds. What this section says of a lane's rack and a
group's (`PluginParam`, `GroupMember`, `GroupBus`, `GroupMemberMod`, `locate_in_group`, `member_modulator_field`, `group_cables_into_lane`, `load_lane_modulators`, `load_group_modulators`, `plan_lane_target`, `plan_group_target`) is how steps 89 and 90 built it and is kept as their record; "Free modulators", "The racks go (step 96)" says what is there now.)

- **The model.** `ModTargetSlot { edge, param_name, aim, depth, param_min, param_max, effective }`: `param_name` is the label the tree shows, bound when the cable was drawn, and `aim` is
  the node and the port. `SessionModel::kind_of(at, source, aim)` says the place that the file and the terminal name it by (`ModTargetKind`: `PluginParam { slot, .. }` for a lane's rack,
  `GroupMember`, `GroupBus` and `GroupMemberMod` for a group's, `FreeParam { plugin, .. }` for a free modulator's (step 91), the siblings' kinds for any), from the structure there is. It is the one place that turns a node into a place: it is how the controller
  tells whether a rack reaches what a cable would drive (`add_target` refuses a cable that it has no place for, and an undone cable or modulator is put back without one), and the file's
  (`save::modulators_to_save`, where the places are still what the file says); the audio thread is told the node, and no longer the place (P5b-1, below). It is `None` for what is not in
  reach of the rack (a lane's rack drives its own chain and its own modulators; a group's, its members' chains and modulators, its bus and its own modulators; a free one's, any plugin of the session and the other free modulators), for a node that is not there, and for
  the modulator's own settings (a cable like that does nothing). `kind_of_target(at, modulator, target)` says it for the cable at a place, `centre_of(aim)` what the cable is centred on, and
  `cables_into`, `cables_into_plugin`, `group_cables_into_lane` and `depth_cables_of` are searches for the cables whose `aim` is at a node. The lists of what a modulator can be drawn to
  (`param_targets`, `field_targets`) still say each by its place (`kind`) as well as by its `aim()`: the terminal's popups name a choice by it. The tree labels what a cable drives by the plugin in the
  slot that the model says now. The graph a client gets and the telemetry's layout are made from the `aim`s (`endpoint_of_aim`), for the cables that `kind_of` has a place for.
- **What no longer exists, because it cannot.** A cable that drives nothing that is in reach. `SessionModel::prune_dangling_cables` takes out what is left of them when a plugin, a modulator, a lane
  or a group goes out of the model without the controller (which takes the cables out first, each by `take_out`, written down for the history), and when a lane leaves a group, and those that drive
  the depth of a cable that goes, until nothing points at nothing (`check_ids` holds the model to it). `Controller::add_target` refuses a cable that `kind_of` has no place for, using no slot and
  no id.
- **The loader.** A cable of a file is made against the nodes there are (`load_lane_modulators`, `load_group_modulators`: `plan_lane_target`, `plan_group_target`, `resolve_rack`, `make_cables`):
  the nodes of a rack's modulators are all there before any cable of the rack is made, so that a cable can name a modulator, or a cable's depth, that comes after it. The commands that the audio
  thread is sent are the same, in the same order, with the same slots and ids, for every file that has no cable that drives nothing. One that has is **loaded without it**, and the log says so: a
  cable to a modulator that the rack has not or to the modulator itself, to the depth of a cable that is not made (and the cables that drive the depth of that one), or to a member's modulator or cable
  that is not there (`a_cable_to_a_modulator_the_file_does_not_have_is_left_out`, `a_cable_of_a_group_to_what_a_member_does_not_have_is_left_out` and the one for a cable to its own modulator).
  A save does not write them. (`mod_depth = [m, c]` names the cable that the file has at place `c` of modulator `m`, and a group's names that of a member's modulator by the place it has in the model.)
- **The audio thread names them too** (step 90, P5b-1). The engine's `ModTarget` is `{ edge, aim, depth, base_value, param_min, param_max, effective }`: the model's id of the cable and the model's `Driven`
  (it and `ModSetting` are in `src/ids.rs` now, which `app/model.rs` re-exports), and its `Modulator` has the model's `node` (`InsertModulator` and `InsertGroupModulator` carry it, as `AddInstrument` carries a lane's
  since P2; `AddModTarget` and `InsertModTarget` carry the cable's id). The audio thread finds what a cable drives when it applies a rack, by the node: a lane's rack looks for the plugin among the lane's own
  (`slot_in_chain`: the lane's node is its instrument's, `effect_nodes` are its effects'), a group's among its bus effects and the plugins of the lanes that are its members now (`locate_in_group`), the setting of a
  modulator or the depth of a cable among the modulators of the rack and, for a group's, those of its members (`sibling_field`, `member_modulator_field`): a few searches by id for each cable of each block, none
  of them allocating. **A cable whose plugin, modulator or cable is not there drives nothing and publishes nothing, and does not go to what comes to its place**: a plugin that is taken out, a lane that leaves the
  group (and comes back: the cable drives it again), a modulator that is taken out with another put in its place. The controller takes out the cables that drive what it takes out first, as it did, and the history puts
  them back, so such a cable is there between two commands at most. The order of the work is what it was (a group's writes before the lanes render, a member's before the bus's, a lane's own after them, so that it wins),
  and so are the bits: the whole corpus of the oracle is unchanged. `AudioGraph::centre(aim, value)` is what the user's value is told by: when a parameter, a rate, a stage of an envelope or the depth of a cable is set
  (`SetParameter`, `SetGroupParameter`, `LoadPreset`, `SetModulatorRate`, `SetModulatorEnvelopeParam`, `SetModTargetDepth` and the group's), every cable that drives it, whichever rack it is in, is centred on it.
  **What is gone** from `chain.rs`: 32 functions and 531 lines (`shift_*`, `fixup_*`, `remap_*`, `adjust_*`, `update_*_base`, `member_moves`, `fix_group_member_targets`, the engine's copies of `lane_member_ordinal` and
  `group_member_lane`, `cross_mod_index`), the 54 tests written around them (17 are new: 11 that say that a cable is on what it was on, and 6 that the mutants asked for, for the user's values), and the `kind` of a cable; ten came (`slot_in_chain`,
  `locate_in_group`, `member_modulator_field`, `sibling_field`, `centre`, `centre_stages`, `params_driven_on`, `targets_mut`, `InstrumentLane::plugin_node` and `Modulator::with_node`): what runs of `chain.rs` is
  673 lines shorter (5,889 to 5,216) and its tests 1,054 (10,325 to 9,271). The oracle's facts (`Facts::racks`, `Facts::bases`) are the model's `Driven`s and edges **exactly** (`RackCables`: each modulator's node,
  and each cable's edge, what it drives and its depth), with no place to translate, the depth only where no cable drives it (the engine's depth of a cable whose depth another cable drives is what that cable
  has made of it this block, and the model's is what the user set). Timing: in the benchmark (`oracle::bench`, release, Apple M4 Pro) the stress session with a chord of six takes 819 µs a block of 256 frames (15.4% of the 5.33 ms) where the loop that the plan replaced takes 820 µs, 822 µs with every lane muted and 371 µs idle, and no block asks the allocator for memory: the searches by node cost nothing that the difference between two runs does not hide (817 µs and 814 µs at step 74).
- **Held by mutants** (step 89): 85 mutants of `kind_of` and the searches by node, the pruning, the loader's plans, the controller's `add_target` and removals, the history's restores, the saver, the graph, the layout and the terminal's tree: 78 were caught by the tests that were
  there, six were gaps (a group's rack reaching the bus of another group, a cable to the modulator after the last, the offset of a depth cable in a modulator that an older file nested in an effect, a cable that depends on one that is left out later in the file, the place the audio thread
  names a depth by, a modulator that comes back from undo without two of its cables: five new tests and one extended, and the six mutants are caught by them) and one is no fault (the saver's slot for a member's cable is never read: the file's writer takes it from the cable's kind).
- **Held by mutants** (step 90): 53 mutants in two runs: 47 of what the engine finds by node and what it is told to centre (`slot_in_chain`, `locate_in_group`, `member_modulator_field`, `sibling_field`, the cross-mod and what it publishes, the lane's and the group's applying of a rack, `centre` and `centre_stages`, the plugin that a preset centres on, the commands that centre, the node that a modulator is inserted with) and of what the controller, the journal and the loader send it (`add_target`, `add_modulator`, the restores, the loader's cables and modulators): 39 were caught by the tests that were there, 7 were gaps and 1 was no fault. The gaps were a cable to its modulator's own setting that was not skipped, the four stages of an envelope each centred on its own value (two mutants), the parameters that a preset centres cables on, the node of an effect in a lane (a parameter of the first effect that moved the cable of the second), the centre of a bus effect's parameter, and the node that `add_modulator` gives a group's modulator (the controller's tests read the commands as text, which leaves the node out; they read it from the commands now). Six tests close them, and six mutants more for the calls of `centre` that the first run had none for (a preset twice, a lane's and a group's rate, a lane's and a group's envelope) run with the seven: all 14 are caught. The no-fault was a guard in `publish_group_member_mod_effective` that skipped a group's cable to a sibling, which `member_modulator_field` does not find among the members' anyway: it is out of the code.
TODO: the commands that address a rack still name places (`inst`, `mod_index`, `target_index`, `slot`: the order of a rack is the layout, and the oracle holds the engine's racks to the model's), and `ModTargetKind`
and `CrossModField` stay in `chain.rs` as the places that the file and the terminal's popups name a choice by (`kind_of`, `ParamTarget::kind`, `FieldTarget::kind`): P5c, which takes the lane out, makes the commands
by node and takes them out.
TODO: a cable of a file that drives nothing is loaded without and only the log says so (a page or a terminal that opens the session is not told, and the next save leaves it out); a notice at load would say it.

### The volume, the mix and the pan that a modulator drives (`ModGain`, `gain = "mix"`, step 97)

A modulator drives a plugin's parameters and, since step 97, **the values that the host keeps** for a node as well: the user's "yes, works well I think. Some things, 1. I cannot modulate everything, for example mix and some volume, is that fixable?" (2026-10-28, after step 96).
They are the **volume** of a lane that has an instrument (`volume = …` on the `[[instrument]]` table: the host's output gain, applied after the instrument and before its effects, which is not the plugin's own `volume` parameter, so that a list that has both calls it `output volume`),
the **mix** of an effect (`mix = …`: the dry/wet blend of a lane's effect and of a bus's) and the **volume** and the **pan** of a group. A lane that has no instrument has no volume to drive (nothing is in front of the gain), and nothing else is a gain: the tempo, a lane's transpose and its key range, a mute,
a solo and the switches of a link are choices and not values. What the page's knobs and the terminal's `v` and `x` set is the **user's value**, and a cable moves what is heard around it as a cable moves a plugin's parameter.

- **The model.** `ModGain { Volume, Mix, Pan }` (`src/ids.rs`: `range()` is 0 to 4, 0 to 1 and -1 to 1, as the intents' `GAIN_RANGES` say them; `name()` is the word of the file and of the tree; `label()` the word of a list that has the plugins' parameters in it: `output volume`, `dry/wet mix`, `pan`; `parse`)
  and `Driven::Gain { node, gain }`: what a cable drives, by the node whose value it is (a lane's, which is its instrument's node, an effect's or a group's), as a parameter is named by its plugin's node. `ModTargetKind::Gain { node, gain }` is the place that the file and the terminal's popups name it by (`kind_of`),
  `SessionModel::gain_of(node, gain)` the value the user set (`centre_of`: what the cable is centred on), `SessionModel::gain_targets(at)` what a modulator can be drawn to (`GainTarget { node, gain, kind, owner_name, value }`: lane by lane the volume of one that has an instrument and the mix of each effect of its chain,
  then group by group its volume, its pan and the mix of each of its bus effects), `cables_into_gains_of(node)` the cables that drive the values of a node, and `free_cables_into` has them too, so that they go with a lane or a group that is taken out.
- **The graph.** The ports `volume`, `mix` and `pan` (`Port::{Volume, Mix, Pan}`, `{"type":"volume"}`, …; `Port::of_gain` and `Port::gain`; `assert_closed` knows them): a cable is the edge from a modulator's `modOut` to the node's port, and its slot of the telemetry, for the value that the cable has made of the user's, is named by the node and the
  port as a parameter's is by the node and the index (`a_cable_to_a_volume_a_mix_or_a_pan_names_its_slot_by_the_node_and_the_port_that_it_drives`). `NodeKind` is as it was: a node already says its volume, its mix and its pan.
- **The intents.** `AddModEdge { modulator, to }` and `Modulate { to, kind, depth }` take the three ports (`Controller::gain_cable`, over `gain_targets`): a node that has none of the value is refused, with nothing changed and no id used (`that node has no volume`, `… no mix`, `… no pan`: the volume of an effect, the pan of a lane,
  the volume of a lane that has no instrument), and so is a modulator that drives the value already (`that modulator drives that mix already`); the other refusals have new words (`a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth`, `a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them`).
  `Modulate` makes a modulator that hears the lane of the plugin whose value it is (a lane's, an effect's) or the group (a group's volume and pan), as it does for a parameter. **A cable is a step of the history** in the words of the control (`modulate volume`, `modulate mix`, `modulate pan`: undo takes the modulator out with its cable, redo makes both again under their ids)
  and of `AddModEdge` (`draw a cable`). Taking a cable out gives the user's value back (`Controller::value_command`, and the audio thread ramps to it), unless another cable drives it. Taking an effect or a bus effect out takes the cables to its mix; `ClearInstrument` takes the cables to the lane's volume with those that drove the plugin's parameters
  (a lane that has no instrument has no volume to drive), where `ReplaceInstrument` leaves them (the lane still has one); a lane that is taken out takes the cables to its volume and to the mixes of its effects, and a group those to its volume, its pan and the mixes of its bus effects (`free_cables_into`), each written down and put back by undo
  (`the_cables_to_a_host_side_value_go_with_what_has_it_and_undo_puts_them_back_in_the_model_the_file_and_the_audio_thread`, `replacing_the_instrument_of_a_lane_leaves_the_cable_to_its_volume_and_taking_it_out_takes_the_cable_and_undo_gives_it_back`). A lane that goes into a group or leaves one keeps its cables (a cable names the node, as every cable does).
- **The file.** `gain = "volume"` (or `"mix"` or `"pan"`) on a `[[modulator.target]]`, in place of `param` and beside `to`, which is the place of whose value it is (`instrument[0]` for a lane's volume, `instrument[0].effect[1]` or `group[0].effect[0]` for the mix of an effect, `group[0]` for a group's volume and pan):

  ```toml
  [[modulator.target]]
  to = "instrument[0].effect[0]"
  gain = "mix"
  depth = 0.5
  ```

  (`ModTargetConfig::gain`, `ModTargetOut::gain`, `FreeSession::gain_at`, `plan_gain_target`, `CablePlace` for the place). The loader **leaves out a cable that cannot be made, and logs why**: a word that is none of the three, a `to` that is none or names a node that has no such value (the volume of a lane that has no instrument, the pan of an effect).
  The cable is centred on the value that the loaded session gives the node and its range is the value's own (a volume above 4, which only a file can say, is held to 4 while a cable drives it: TODO below). A save writes what a load reads, and the same file again (`a_session_with_cables_to_volumes_mixes_and_pans_is_saved_as_the_same_cables_and_the_same_file`).
- **The engine keeps the user's value** (`InstrumentLane::volume`, `mix_values`, `Group::volume` and `pan` in `chain.rs`: they are what the model has, `AudioGraph::centre` moves the cables to them when `SetVolume`, `SetMix`, `SetGroupVolume`, `SetGroupPan` or `SetGroupMix` sets them, and the oracle compares them after every block)
  **and carries what the cables make of it as a ramp for one buffer**. `apply_free_modulators` collects, from the cables that end at such a value (`Driven::Gain`), a `GainDrive` for each value: the user's value, its range and the sum of the offsets (**added, as for a parameter: no modulator wins**, and each by the law of its kind);
  `apply_gain_drives` works out the goal (the user's value and the sum, held to the range), finds the value by its node (`GainOwner::{LaneVolume, LaneMix, GroupVolume, GroupPan, GroupMix}`: a value whose node is nowhere drives nothing and publishes nothing, as a parameter's cable does) and hands the lane or the group a ramp from where the value was when the last buffer ended
  (`GainRamp`) to the goal (`volume_ramp`, `mix_ramps`, `pan_ramp`), which `apply_volume`, `run_effect` and `apply_pan` follow linearly over the buffer, as the fade of a mute does (a mix, a volume or a pan that a modulator moves a buffer at a time would zipper). The goal is the cable's `effective` value in the telemetry.
  **When the last cable on a value goes**, the value ramps back to the user's over one buffer and then nothing is remembered (`GainRamp::left`). A session in which no cable ends at such a value has no `GainDrive` and no ramp, and every path is the code it was: **the oracle's whole corpus of 1017 renders is the same bits**, and no block asks the allocator for memory
  (`cables_to_volumes_mixes_and_pans_ask_the_allocator_for_nothing`). Held by `a_cable_to_a_lanes_volume_ramps_it_over_the_buffer_to_what_the_cable_makes_of_it_and_holds_it_there`, `a_cable_to_an_effects_mix_ramps_the_blend_and_what_the_user_sets_the_mix_to_is_what_it_is_centred_on`,
  `cables_to_a_groups_volume_and_pan_ramp_them_and_the_pan_leaves_the_two_sides_apart`, `a_volume_that_a_fast_lfo_moves_is_ramped_so_that_the_step_across_a_border_is_one_step_of_the_ramp_and_not_a_jump`, `taking_the_cable_out_ramps_the_volume_back_to_what_the_user_set_and_then_leaves_it_alone`,
  `two_cables_on_one_mix_add_up_and_are_held_to_the_range_of_a_mix_and_a_cable_to_a_node_that_is_nowhere_drives_nothing` and `a_lane_whose_effect_is_taken_out_while_a_cable_drives_its_mix_is_left_with_nothing_remembered`.
- **The terminal.** The target selector (`t` on a modulator) lists them after the plugins' parameters, lane by lane and then group by group, in the cells `[owner, value]` (`L1 Oscillator` and `output volume`, `L1 Filter` and `dry/wet mix`, `G1 Pad` and `volume` and `pan`: `ModGain::label`), and the tree says what a cable drives as
  `→ L1 Oscillator volume (50%)`, `→ L1 Filter mix (30%)`, `→ G1 Reverb (bus) mix (30%)` or `→ G1 pan (50%)` (`gain_target_label`, `free_target_label`; a plugin's own parameter of the same name is told with a colon, `L1 Oscillator: volume`). `m` (the modulate popup) and `l` (learn) on a group's **Volume** and **Pan** rows make a modulator that hears the group and bind it (`group_row_gain`).
  TODO below: an instrument's volume and an effect's mix are not rows of the parameter pane (they are edited with `v` and `x`), so that they are modulated from the selector and not started from.
- **The page.** The cell of a host-side value has a port on its top edge, where a cable ends, and the `~` button that modulates it, **when the node has the value**: the volume of a lane that has an instrument, the mix of an effect, a group's volume and its pan (`GainCell`; a lane that has no instrument shows its volume knob and no port, and `gainPorts` in
  `client/reach.ts` is the controller's rule). The knob (`GainControl`) has **the ring and the dot of a parameter's** (`modulationExtent` over the value's own range, the dot the engine's `effective` value read from the telemetry by the node and the port). A cable is dragged to the port (`portAt`, `handleOf`: the handles are named `volume`, `mix` and `pan`),
  or chosen from the list at the foot of a modulator (`Drivable.fields`: `output volume`, `mix`, `volume` and `pan` after the node's parameters; `drivablesOf`, `driversOf`), or from the `~` menu of the knob; its words are `LFO → Filter: mix`, `ADSR → Oscillator: output volume`, `LFO → Pad: pan` (`gainName`, `portName`, `endLabel`; the knob itself is still called Volume, Mix or Pan).
  **The mock engine** does what the controller does (`addGainCable` with its refusals in its order and its words, `modulateOutcome` for a group, `replaceInstrumentOutcome` leaving the cables that are not the plugin's, the effective value of a driven value), pinned by three new scripts of `history.json` (cables drawn to every value and refused; `Modulate` for the three ports;
  every removal, undo, the replacement and the clearing of an instrument) and by a new list of `fixtures/freeReach.json`, `gains` (what the controller let each of its 8 modulators be drawn to: 13 values for each, in a session of two groups, four lanes and an empty lane, which has none). The browser specs are `e2e/gains.spec.ts`
  (on the mock: a cable to a mix and to a volume with their rings, and Undo; the `~` menu of the mix and the modulator it makes, which hears the lane; a group's pan and volume. On the real program: the knobs move on the engine's own values, the file names what each cable drives, Undo, Redo and a page opened again).
- **The oracle** has a session (`gains`: a group with a reverb on its bus, a lane in it with a filter, and a lane that has a delay, with three modulators, an LFO, an envelope that hears every note and a step sequencer, whose seven cables drive the volume of both lanes, the mix of the filter, of the delay and of the reverb on the bus, and the group's pan and, downwards, its volume) and a scenario (`edited:gains`, `GAIN_EDITS`, 24 edits with `Edit::GainCable { modulator, value }`:
  cables drawn to every kind of value while a chord is held, the values set by the user, the effect, the modulator, the lane and the group that a cable drives or starts at taken out and put back by Undo, the instrument cleared and put back): six renders more, **1023 in all, and the 1017 that were there are the same bits** (the stored files have six lines and two lines more, and nothing else changed).
- **Held by tests.** Rust 2447 tests (2633 with the `web` feature, 2441 without the default plugin formats, 2627 with `web` and without them), the page's 2803 tests in 120 files (33 new), the browser suite 199 (4 new, and three that count the ports and buttons of a node updated); mutants 68 of the Rust (60 caught by the tests that were there and 8 survived, seven gaps that are six tests now and one a filter of the run that left out the test that catches it) and 27 of the page (23 caught and 4 survived: three gaps that are three tests now and one no fault).
- **Decisions** (D97 of `docs/web-ui.md`; the user may overrule them). (1) Which values: a lane's volume (with an instrument), an effect's mix, a group's volume and pan; not the tempo, a transpose or a key range (choices), and not the switches. (2) The user's value is what the model and the engine keep and the knob sets; what is heard is the user's value and the sum of the offsets, held to the value's range
  (0 to 4, 0 to 1, -1 to 1), and a cable that is taken out gives the user's value back over one buffer. (3) A lane's volume is called `output volume` wherever it stands among the plugin's parameters; the knob on its node is still Volume. (4) A cable to a lane's volume goes when the instrument is taken out and stays when it is replaced.
- TODO: a volume above 4, which only a file can say, is held to 4 while a cable drives it (the range of the value is the controller's, 0 to 4, and the loader gives the cable that range): the range could be widened to the value that the file says.
- TODO: the terminal's parameter pane has no row for an instrument's volume or an effect's mix (the group's pane has its Volume and Pan), so `m` and `l` cannot start from them; the selector (`t` on a modulator) is the way, and the action bar says nothing of it.
- TODO: a modulated mix, volume or pan is a ramp of a buffer (5.3 ms at 256 frames): a modulator faster than half the buffer rate is smoothed, as a plugin's parameter driven by it is a step per buffer; and the `effective` value that the telemetry carries is the goal at the end of the buffer.
- TODO: the engine's ramp is linear in the value, so that a volume driven through 0 to 4 is as linear as the user's own (a decibel law would sound more even at the quiet end): the knob's scale is linear too.
