# Plugins

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): plugin formats, the built-in plugins, how plugin I/O is handled, a parameter that has an effect only sometimes. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

### A parameter that has an effect only sometimes (`used_when`, step 95b)

A parameter can say **when it has an effect at all**: the Math's `bits` only in its three logic modes, its `pitch` and `wave` only while nothing is patched at its second input. The user's question of 2026-10-28, with `examples/ring-bell.toml` open (a cable is patched at B there): "I am not sure
the knobs on the math does anything now, like the pitch and the wave? is that true?". It was, and nothing said so. A front-end shows such a parameter **dimmed** while it has none, and says why; it can still be turned, and keeps its value for when it has one.

- **The plugin says the rule, and the host works it out.** `ParameterInfo::used_when: Option<UsedWhen>` (`src/plugin/mod.rs`; none for a parameter that always has an effect, and for every plugin that does not say: the three formats' adapters and the other built-ins) is `UsedWhen { condition, why }`: a
  `UsedCondition::Choice { param, values }` (the choice `param`, a `ParameterInfo::index` of the same plugin, is set to one of `values`, places in its list of labels) or `UsedCondition::SideUnpatched` (nothing is patched at the plugin's second input), and `why`, the rule in words (`only while nothing is patched at B`,
  `only in the modes xor, and, or`). `UsedCondition::holds(value_of, side_patched)` is the one rule: a choice holds when the value that was set rounds (as Rust rounds: a half away from nothing) to one of its places, never for a place below the list, and a rule that names a parameter the plugin has not is no rule
  (it holds). The plugin is not asked while it runs: what is set is read where the values are, so that the rule is data that any front-end reads.
- **The Math's.** `amount` in the modes that read it (am, fold, warp, compare, hold, follow), `bits` in xor, and, or, `attack` and `release` in follow, `pitch` and `wave` while nothing is patched at B; `mode` and `level` always. `a_parameter_that_says_it_has_no_effect_has_none_and_one_that_says_it_has_one_has_one` holds the table to the
  arithmetic in each of the twelve modes, with a cable at B and with none: where a parameter says it has no effect two settings of it give the same bits, and where it says it has one they give sounds that differ (`its_parameters_say_when_they_have_an_effect` has the table itself, in words).
- **The model and the graph.** `ParamSlot::used_when` (copied from the plugin's info when it is loaded: `plugin_slot`), `PluginSlot::unused_because(param, cables) -> Option<&str>` (the words of the rule when it does not hold of the values the user set and the cables: a cable of `cables` that ends at `CableTo::EffectSide(self.node)` is a cable at *this*
  effect's second input) and on the wire `Param::used_when` (`usedWhen`, left out for a parameter that has none, so that every other plugin's node is the JSON it was; `UsedWhen` and `UsedCondition` are generated TypeScript). It is the value that the user set that counts: a choice that a cable drives is read where it is set.
- **The terminal.** On the Session tab the row of a plugin's parameter that has no effect is dimmed (`RowLook::Unused`, as a step past a sequencer's length is; told by the parameter and not by the row's place, which the filter changes; a bus effect's too), and while the cursor is in the pane and on such a row the line under the pane
  says why: `pitch has no effect now: only while nothing is patched at B`. `tang describe` says the rule after each such parameter (`[3] pitch (min=-48, max=120, default=57): only while nothing is patched at B`).
- **The page.** `client/uses.ts` has the rule again (`sideIsPatched`: a cable edge that ends at the node's `audioInSide`; `conditionHolds`; `unusedBecause`), pinned to the model's by `fixtures/uses.json` (48 rows: the two math effects of the second input's session, with their cables as they are and taken out, in each of the twelve modes, and for each of
  the eight parameters why it has no effect then, or null; the graph is the snapshot of `sideCables.json`). `paramUnusedBecause` (`client/selectors.ts`) is the selector, and `ParamCell` (a knob or a choice on a node) and `ParamRow` (a row of the ⚙ panel) put `data-unused` and a title on their element, `No effect now: only while nothing
  is patched at B`, which the stylesheet dims (`.tang-cell[data-unused]` to 0.45, `.tang-row[data-unused]` to 0.5). A dimmed control turns as any does. The mock engine needs nothing (the graph that it serves has the rule: `builtins.effects.json` is written by the controller), and `e2e/math.spec.ts` runs it on the mock engine and on the program.
- **Held by mutants** (step 95b): 63 mutants of the logic of step 95b, one at a time against the unit tests that should catch them: 39 of the Rust (the condition 7, the model's question 6, the loader's copy and the graph's two 3, the Math's table 14, the terminal's dimming and its line 8, and `tang describe` 1, by hand against `tests/cli.rs`), all caught by the tests that were there and none by the build only; 24 of the page (`uses.ts` 15, the selector 2, the controls 7), 21 caught and 3 survived: **one gap, a test now** (the rule reads the parameter that it names by its index and not the first of the plugin's: in the Math the mode is the first, so that the table could not tell), **one no fault** (a guard against a place below the list: Rust's cast to a whole number needs it, JavaScript's `includes` does not, and it is out of the code) and **one that only the type check catches** (the guard on the kind of node that gives `plugin` its type: `tsc` fails without it, Vitest does not).
TODO: only the Math says when its parameters have an effect. A CLAP, VST3 or LV2 plugin says nothing of it (none of the three formats has a flag for it), and there are two kinds of condition only (a choice, and a second input that has no cable): a range of a parameter, or two conditions at once, would be variants of `UsedCondition`.
TODO: the dimming follows the value that the user set, and a choice that a cable drives (a modulator on the Math's `mode`) is read where it is set, so that a knob can be dimmed for a mode that the engine is not in; the effective value that the telemetry has would say it.
TODO: a dimmed control says why to the pointer (its title) and in the terminal under the pane, and a screen reader is not told (the control has no description of it); the Help tab does not say what a dimmed row is; and the panes of a modulator, a group and a pattern have no rule of this kind (a modulator's pane has its own looks).
TODO: the terminal's pane shows a value to two places, so that the Math's `attack` of 5 ms, and the oscillator's of 4 ms, read `0.00` (found while looking at the pane for this step): a unit, or more places for a range that small, would say it.

## Plugin compatibility

Plugin loading is behind a trait. Three formats supported:

- **LV2** — via livi.
- **CLAP** — via clack-host.
- **VST3** — via vst3-rs (coupler-rs/vst3-rs) with libloading.

### Built-in plugins

- **Oscillator** (`builtin:osc`) — one polyphonic oscillator with a
  selectable waveform, per-voice ADSR amplitude envelope. Parameters:
  `waveform` (enum: sine/triangle/square/saw/noise), `detune` (±24 semitones / ±2
  octaves — wide so a modulator can sweep pitch dramatically; use small values
  or low modulator depth for subtle detuning), `volume`
  (0–1, smoothed ~5 ms), `attack`/`decay`/`release` (0–10 s), `sustain`
  (0–1), `pan` (−1 left … 0 center … +1 right, balance-style so center leaves
  both channels at full; smoothed ~5 ms). Envelope defaults (A=4 ms, S=1.0, R=12 ms) reproduce the original
  click-guard ramps; envelope times are floored to 2 ms so extreme settings
  stay click-free. Voices ring until their release completes. The voices are in a map with room for every key, made with the oscillator, so a note-on
  does not allocate on the audio thread (`notes_that_start_sound_and_end_do_not_allocate` counts).
  Four parameters were added after `pan` (so the others keep their places), and what they start at changes nothing: a chord of each waveform is held
  to what it rendered before them by `the_oscillator_renders_what_it_did_before_it_had_a_unison_and_a_pulse_width`.
  `unison` (a choice of 1 to 7 oscillators for each note, named by how many: the value is the place in the list, 0 is one; default 1) with `spread`
  (0–1 semitone, default 0.15: how far the outer oscillators are from the pitch of the note, each way, the others evenly between) and `width` (0–1,
  default 0.5: how far across the channels they are put, the lowest on the left and the highest on the right, by a law of equal power, so that a
  channel keeps the level of one oscillator whatever the width is, and not by a balance, which would take them out of the other side); both do
  nothing for a unison of one, and a single channel has no width. The oscillators of a note start in different places of their cycles (the first at
  its beginning, as a note has always begun), and the sum is brought down by the root of how many there are. A change of the unison while a note
  sounds adds or takes away an oscillator at once (a click: it is a choice, and not meant to be swept). `pulse` (0.05–0.95, default 0.5) is how much
  of the cycle a square is high: the offset of a pulse that is not a half (its mean) is taken out and it is scaled so that its higher side is at most 1,
  so that a half is the square it was and a narrow pulse does not carry a hum; the other waveforms have no use for it. A pulse is as naive as the
  waveforms are (no band-limiting: it aliases at high pitches, a narrow one more than a half, as the square and the saw do).
  The `waveform` has a fifth value, `noise` (4, after the saw, so that the numbers that sessions saved for the four before it are what they were): white noise, a number from -1 to 1 for
  every sample, all equally likely, with the level of a saw (the same RMS) and no pitch, so that `detune`, `pulse` and `spread` do nothing to it, and the envelope, `volume`, `pan`,
  `unison` and `width` do what they do to a wave (the oscillators of a unison are noises of their own, spread across the channels, and the sum is brought down by the root of how many
  there are). Each oscillator of each voice makes its numbers from a state of its own (`Voice::noise`: xorshift32, started from a hash of how many voices have begun and the
  oscillator's place in the unison, `noise_seed`, which is never 0), and not from a stream that the voices share: the sum of a chord would then be dealt out in the order that the voices are walked in (the order of
  their keys), and what a note got would depend on the chord that it is in (`the_noise_of_a_chord_is_the_same_whatever_the_order_that_its_voices_are_summed_in`: its notes begin at frames of their
  own, so that the voices are at different places of their attacks and a shared stream would show). Each key that is hit has noise of its own (a roll on one key is not the same burst
  again; a key that is hit again while it sounds goes on with its voice), and a run that plays the same notes has the same numbers. Not here: a colour (pink and brown noise; a low-pass after
  it does most of what they are for), and noise in the same lane as a wave (a second lane and a group sum them).
  Two parameters more come after `pulse` (the others keep their places; what they start at changes nothing, which `with_no_fm_whatever_the_ratio_the_oscillator_renders_what_it_did_before_it_had_one` holds a chord of each waveform to): `fm` (0–1, default 0) and `fm_ratio` (a choice, default ×1), the **FM**. The phase of the wave is swung
  by a sine of its own, so that a sine becomes the sine with the sidebands of the Bessel functions (the partial `k` of `sin(w t + b sin(r w t))` is a sine of `(1 + k r) w` with the amplitude `J_k(b)`: `a_sine_that_is_swung_by_another_has_the_partials_of_the_bessel_functions_at_the_index` holds it for 13 ratios at four
  indexes), and the other waveforms are read where their phase is swung to (phase distortion). It is the sound that FM synths make, as phase modulation, which has the same partials. `fm` is how far the phase is swung, up to 10 radians (1.59 cycles) each way at the top, smoothed over about 5 ms as the volume is, so that a
  modulator that moves it a block at a time does not click (`the_index_is_gone_to_and_not_jumped_to` and `the_index_is_gone_down_from_and_not_dropped_from`); a swing that has gone back to nothing is set to 0 under 1e-5, so that a sound with no FM is the sound that there always was and the number does not run down into the denormals.
  `fm_ratio` is a place in `FM_RATIOS`, how many times the pitch of the oscillator the swinging sine goes: ×0.5 ×1 ×1.4 ×1.5 ×2 ×2.5 ×2.8 ×3 ×3.5 ×4 ×5 ×5.6 ×6 ×7 ×8 ×12 (the whole numbers make harmonic sounds, the multiples of 1.4 bells and metal); a choice, like `unison`, so that a cable to it steps from place to place. The
  swinging sine begins at 0 with each note (the first frame of a note is the wave's), goes round at the pitch of each oscillator of a unison, spread included, and is kept to one cycle. The noise has no phase, and FM does nothing to it. Not band-limited: at a high pitch and a high index the sidebands fold over the Nyquist frequency, as the
  naive waveforms do. In the page the two are in the panel, under a section of their own, "FM" (pin them to put them on the node).
  Three parameters more come after `fm_ratio` (the others keep their places; what they start at changes nothing, which the tests that hold a chord of each waveform to what it was, and `a_sync_of_one_is_no_sync_whatever_the_waveform`, say): `sub`, `sync` and `drift`, under a section of their own in the page's panel, "Voice" (pin them to put them on the node).
  `sub` (0–1, default 0) is a **sub oscillator**: a sine one octave under the note, in the middle of both channels, at `sub` times the level of the oscillator's own voice (a sine at a `sub` of 1 is as loud as the note itself: `the_sub_is_a_sine_an_octave_below_the_note_at_the_level_that_is_set`). It is the note's and not one of a unison's: the unison does not bring it down by the root of how many there are, and `spread` and `width` do not move it (`the_sub_goes_where_the_note_goes_and_not_where_a_unison_spreads_it`); it goes where `detune` takes the note, and it does not drift (`the_sub_does_not_drift_with_the_oscillators`). It is gone to as the volume and the FM's index are, over about 5 ms, so that a modulator that moves it a block at a time does not click, and set to 0 under 1e-5, so that a sound with none is the sound there always was and the number does not run down into the denormals (`the_sub_is_gone_to_and_not_jumped_to`, `a_sub_that_is_turned_off_goes_to_nothing_and_not_to_a_number_that_never_gets_there`). A sine and an octave: a square sub and a sub of two octaves are not here.
  `sync` (1–8, default 1) is a **hard sync**: the oscillator that sounds (the slave) goes round `sync` times as fast as the note's own cycle (the master) and is started over at the beginning of each cycle of the note, so that the pitch is the note's whatever the ratio is, and the ratio is the colour: a saw at 3.3 has three ramps in a cycle and a third of a fourth that is cut off (`a_saw_that_is_synced_goes_through_as_many_ramps_in_a_cycle_of_the_note_as_the_ratio_says`: 1, 2.2, 3.3, 3.7, 5.6 and 8 make 1, 2, 3, 4, 6 and 8 ramps). The slave starts again as far into its own cycle as the master is past the end of its own, so that a cycle that ends between two frames does not move the sync by a frame (`the_synced_oscillator_is_as_far_into_its_cycle_as_the_note_is_into_its_own_times_the_ratio`). A ratio of 1 is no sync, bit for bit (the slave is the master's place: `a_sync_of_one_is_no_sync_whatever_the_waveform`), so that a modulator that sweeps the ratio up from 1 starts from the plain wave. It is a ratio and not an interval, so that an envelope on it adds a steady number of ramps; its knob is linear. The noise has no cycle to sync (`the_noise_has_no_cycle_to_sync`); the FM's swinging sine goes at the master's pitch and the wave that it swings is the slave's (`a_synced_saw_that_is_swung_a_little_goes_through_the_ramps_of_the_sync_all_the_same`). A modulator sets the ratio a block at a time, so it moves in steps of a block (5.3 ms at 256 frames). Naive, as the waveforms are, and so it aliases: the restart of the slave is a step, and no band-limiting softens it.
  `drift` (0–1, default 0) is a slow **wander of the pitch**: each oscillator of each voice has a place to go to, a number from -1 to 1 that is drawn every 0.8 s (`DRIFT_HOLD_SEC`), and its pitch goes to it with a time constant of 0.4 s (`DRIFT_TAU_SEC`); at the top of the knob the places are 12 cents each way (`DRIFT_CENTS`: one oscillator over twenty seconds went from -11.4 to +9.9 cents, and half of the knob goes half as far: `a_drift_wanders_the_pitch_a_few_cents_and_none_is_none`, `the_drift_is_given_a_new_place_every_eight_tenths_of_a_second_and_goes_to_it_slowly`). The numbers are the voice's own (`Voice::drift_place`: xorshift32 from a hash of how many voices have begun, as the noise's are), so that each note and each oscillator of a unison wanders on its own, whatever order the voices are walked in, and the same notes wander the same way in every run (`the_same_notes_wander_the_same_way_in_two_runs`, `the_notes_of_a_chord_and_the_oscillators_of_a_unison_each_wander_on_their_own`): it is what keeps a unison alive, and what the oscillators of an analog synth do (a wander of cents, and not a vibrato). It moves once a block, before the voices are rendered, so that a note begins with no drift and starts to go at its second block; a drift that is turned to none gives the pitch back at once (`a_drift_that_is_turned_to_none_leaves_the_pitch_of_the_note`: a step of at most 12 cents in the frequency of a wave whose phase goes on, which does not click).
  TODO: the sync is naive (the slave's restart is a step, so it aliases at high pitches and ratios): a band-limited one (PolyBLEP, or an oversampled one) is not here, nor are a soft sync (the slave turns round instead of starting over), a sub of another waveform or two octaves down, and a drift that the sub follows.
  Two parameters more come after `drift` (the others keep their places), under a section of their own in the page's panel, "Performance": `velocity` (0–1, default 0) and `bend` (a choice, default ±2), and with them the oscillator answers to a keyboard: what it starts at changes nothing that a person played before, and a pitch wheel, a pedal and all notes off, which it ignored, have a say now.
  `velocity` is how much of the level of a note is how hard the key was hit: the level is `1 - velocity + velocity * hit / 127`, so that 0 is none of it (every note as loud as the next, which is what the oscillator was, bit for bit: `velocity_does_nothing_at_its_default`) and 1 is all of it (a note hit at 64 is 64/127 as loud as one at 127: `a_velocity_of_all_makes_the_level_the_velocity_over_127`; a half is half way between: `half_a_velocity_is_half_way_between_none_of_it_and_all_of_it`). It is the level of the whole note, the sub's included, and a key that is hit again while it sounds goes to the new velocity over about 5 ms, as the volume does, and not at once (`a_key_that_is_hit_again_goes_to_the_new_velocity_and_does_not_jump_to_it`). The keys of the computer and of the page hit at 100.
  `bend` is how far the pitch wheel bends a note, a choice (off, ±1, ±2, ±3, ±5, ±7, ±12, ±24 and ±48 semitones: `BEND_RANGES`) so that the ranges that are asked for are a step apart; ±2, a keyboard's usual, is where it starts. A wheel message (`0xE0`, 14 bits, 8192 the middle) is heard at its frame of the block, as a note is (`a_wheel_message_is_heard_at_its_frame_of_the_block`). The wheel of the **master channel** (the first) bends every note, and the wheel of a **member channel** bends the notes that began on that channel as well: MPE, so that the notes of a Seaboard glide each by itself (`a_member_channel_bends_the_notes_that_began_on_it_and_the_master_channel_bends_all_of_them`), and a lane only gets the member channels of the notes it sounds (see "MIDI routing"). The notes of an MPE controller bend by ±48, which is the range to choose for one. The wheel and `detune` add up (`the_wheel_and_the_detune_add_up`), the sub goes with the note, and the bend is not smoothed (the phase goes on, so that a step of the wheel is a step of the pitch and not a click) and is not put back by a new note: a controller that bends per note says where its wheel is before it begins one. A lane's note remap (see "Note remapping") sends bends of its own on channels 2 to 16 for the notes it moves, which the oscillator takes as it takes an MPE controller's, so that the lane's `pitch_bend_range` is to be the oscillator's range.
  The **sustain pedal** (controller 64, down from 64 up, on whatever channel) holds the notes that are let go of while it is down until it goes up (`a_note_that_is_let_go_under_the_pedal_sounds_until_the_pedal_goes_up`): a note that was let go of before it went down is not held (`a_note_that_was_let_go_before_the_pedal_went_down_is_not_held_by_it`), a key that is down is not let go of by the pedal going up (`a_pedal_that_goes_up_does_not_let_go_of_a_key_that_is_down`) and a key that is hit again under it is a key that is down again (`a_key_that_is_hit_again_under_the_pedal_is_a_key_that_is_down_again`). **All notes off** (controller 123) lets go of every note, those that the pedal holds too, with the release of the envelope, and **all sound off** (controller 120) with a release of 5 ms (`FAST_RELEASE_SEC`) whatever the envelope's (`all_notes_off_lets_go_of_every_note_under_the_pedal_too_and_all_sound_off_is_quicker`); neither changes where the pedal is, and a voice that is on its way out by all sound off goes on with the envelope's release if its key is hit again (`a_key_that_is_hit_again_after_all_sound_off_is_let_go_of_by_the_envelope_again`). The other controllers are ignored (a modulator can follow them: MIDI learn).
  **Panic** (step 65, `docs/web-ui.md` D65): `Ctrl+P` in the terminal (from every tab, the Piano tab that takes the letters included, and not while a popup is open: `a_popup_has_the_keys_while_it_is_open_and_ctrl_p_is_not_heard_in_it`) and the **Panic** button at the right end of the bar above the page's piano (the message `{"type":"panic"}`) let go of every note that sounds, for the notes that hang: a release that was lost, a plugin that did not hear one, the stuck notes that the TODOs about undo name. `NoteInput::panic` sends the pedal up (controller 64 at 0) on all 16 channels, a note-off for each note that is held (channel 1, the lowest first, so that a plugin that has no ear for the controllers that follow still hears these), all notes off (123) on all 16 channels and all sound off (120) on all 16: 48 messages and the held notes, all sent as no source's (`Source::BROADCAST`, so that every lane hears them whatever the sources it hears are: see "MIDI routing") (`a_panic_lets_go_of_the_held_notes_and_says_pedal_up_all_notes_off_and_all_sound_off_on_every_channel`), and then clears the held-notes bits, so that the piano's lights go out whatever the plugins do. It returns how many messages the channel to the audio thread had no room for (it holds 1024), which the terminal and the server log. It is every note the server holds, not only those of the connection that asked (`a_panic_lets_go_of_every_note_the_server_holds_not_only_this_connections_and_is_not_held_back_by_the_rate`), and the rate that holds back a browser's notes does not hold it back. It is no intent: no revision, no step of the history, nothing asked of the controller, and no change of the session. The page's `NoteHolds` lets go of what it holds with it (`panic()`): a key that is let go of after the panic ends nothing, and the key can be played again. **The effects are not reset**: `Plugin` has no reset, so what a reverb or a delay has in hand rings on. Measured (left side, a note held for 0.5 s and ended by a panic or by a note-off, the oscillator into the effect): the reverb's rms from 1.0 to 1.5 s was 0.0106 after a panic and 0.0109 after a note-off, the delay's 0.0716 and 0.0720, and an oscillator with no effect was silent from the first window, 0.1 s after the panic.
  One parameter more comes after `bend` (the others keep their places; what it starts at changes nothing, which the pinned chords and `a_quantizer_set_to_a_detune_that_is_on_the_scale_is_the_oscillator_with_none_to_the_last_bit`
  hold), in the section "Voice": **`quantize`** (step 69, `docs/web-ui.md` D69), a choice of `off` and then the scales of the Piano tab by their short names (`crate::scale::SCALES`: major, minor, harm minor, mel minor, dorian,
  phrygian, lydian, mixolydian, locrian, maj pent, min pent, blues, whole tone, diminished, chromatic; the value is the place in the list: 0 is off, 11 the minor pentatonic). With a scale chosen, `detune` is held, once a block,
  to the tone of the scale that is nearest to it (`scale::nearest_tone`: halfway between two tones, the lower one), counted in semitones **from each key that is held**: the key is the root, so that C plays the notes of C's minor
  pentatonic and F those of F's. The wheel is added after it and is not quantized; the sub follows the pitch that is heard, and a unison's spread and the drift are added to each oscillator after it. A modulator that moves
  the detune plays the steps of the scale: a step sequencer whose bars are painted roughly (the page's bars are held to a hundredth: f161) is in tune, and a random LFO is a melody (`examples/melody.toml`). It is the oscillator's
  own and not a cable's, since a cable cannot know a scale; every modulation of `detune` is quantized with it, a vibrato included (the wheel is the vibrato of a quantized pitch).
  TODO: not here: a scale counted from the session's root (the Piano tab's scale is not given to the plugins), the notes of a chord each moved to a tone of its own (a chord is moved as a whole, each note by the same number of
  semitones), and a quantizer for the other parameters (a cable that holds what it drives to a step).
  TODO: the panic leaves what the effects have in hand (a reverb's tail, a delay's repeats) to ring out; a reset of the plugins on the audio thread (CLAP `reset`, LV2 and VST3 deactivate and activate again, the built-ins' own state) would make it silent at once. And a key for the page: its letters are the piano, Escape closes what is open and Control+P is the browser's print, so that the button is all it has.
  TODO: not here: the mod wheel (controller 1) as a vibrato, channel and polyphonic pressure (a cable from a MIDI learn control does them), a velocity curve and a wheel that springs by its own.
  The voices are kept in the order of their keys (`Voices`: a sorted vector with room for every key, so that a note-on does not allocate), and a block walks them as a slice, so that the
  sum of a chord is the same bits in every instance and in every run (`a_chord_is_rendered_with_the_same_bits_by_every_oscillator`; step 70). It was a hash map, which walks its entries in
  an order that is another for each map, so that a sum differed by a rounding from one run to the next: the oracle of `docs/patch-graph.md` §11.6 holds renders to a hash, and needs it.
  The old
  `builtin:sine`/`builtin:triangle`/`builtin:square` IDs still load (as
  aliases that start on that waveform) so older sessions keep working, but
  enumeration and the selector show only the unified `builtin:osc`.
- **Audio Input** (`builtin:input`, step 99) — the computer's microphone, or another input of the sound card, as an instrument that starts in a lane whose way out is cut, so that it is heard only through the audio cables that are drawn from it. Parameters: `gain` (0–4, default 1), `pan` (a balance, −1…+1) and `device` (the system's default input, or one of its inputs by its place in the list). With no device (`--audio-device none`, the tests) it plays a sine of 220 Hz at 0.25. See "The audio input".
- **Reverb** (`builtin:reverb`) — stereo effect with presets.
- **Filter** (`builtin:filter`) — stereo multimode filter. Parameters:
  `cutoff` (0–1, exponential 20 Hz→20 kHz so sweeps move musically),
  `resonance` (Q 0.5–10, default 0.707), `type` (enum, switchable live),
  `drive` (1–10, input gain into the analog models' saturation; no effect
  on the clean types). Types:
  - `lowpass` / `highpass` / `bandpass` / `notch` — clean 12 dB/oct
    trapezoidal state-variable filter, stable under cutoff modulation.
  - `ladder` — Moog/Mother-32-style 4-pole transistor ladder (24 dB/oct,
    warm, self-oscillates at full resonance).
  - `acid` — TB-303-style diode-ladder flavor: 18 dB/oct tap with
    high-passed feedback, so the squelch rides on top while bass stays put.
  - `ms20` — Korg MS-20-style Sallen-Key (12 dB/oct) with hard-clipped
    feedback; aggressive, screams at high resonance.

  The ladder/acid use a zero-delay-feedback (TPT) topology with `tan`-
  prewarped tuning: the resonant feedback is resolved instantaneously (no
  one-sample delay), so the cutoff does not wander with input level, the
  resonant peak/self-oscillation track the knob proportionally, and acid
  keeps its resonance down in the bass. The single tanh on the resonant
  feedback bounds self-oscillation while staying linear (correctly tuned)
  for small signals. `drive` saturates the input (`tanh`), adding harmonics
  — it does not auto-attenuate, so heavy drive gets louder/brighter like a
  real filter (and like a real filter, harmonics that fall above the cutoff
  are then filtered out, so driving a closed filter is cleaner than driving
  an open one).

  **Cutoff convention:** `cutoff` marks the filter's −3 dB point for *every*
  type — the standard definition, consistent with the clean SVF modes — so
  switching `type` at the same knob keeps the same brightness. Because a
  3–4-pole cascade's −3 dB point sits well below its per-stage corner, the
  analog modes raise the internal corner by a per-mode tuning multiplier
  (`LADDER_TUNE`/`ACID_TUNE`/`MS20_TUNE`) to compensate. A consequence: at
  high resonance the resonant peak sits somewhat above the knob (the peak
  and the −3 dB point are an octave apart in a steep ladder and can't both
  coincide with the knob) — it still tracks the knob proportionally, so
  sweeps stay musical.

  All analog models run at 2× internal oversampling; switching type resets
  filter state. For the analog types, `resonance` maps onto the model's
  feedback range (full = just past self-oscillation). Cutoff/resonance/
  drive are smoothed ~5 ms. Defaults to a fully open clean lowpass
  (transparent). The filter defaults to 1.0 mix (fully wet) when added —
  unlike other effects (0.5) — because at 50% the dry path passes the
  frequencies the filter removes (a highpass would barely change the sound).
  Adjust with `x` if a parallel/blended filter is wanted. The `#[ignore]`d tests `diag_full_report` and
  `diag_cutoff_consistency` are a measurement harness (Goertzel-based
  magnitude/THD/slope/aliasing) for iterating on the filter voicing.
- **Delay** (`builtin:delay`) — a stereo delay that puts out its echoes only, as the reverb
  puts out its tail only: the host's `mix` (0.5 when it is added) is the blend with the sound
  put in, so that what is heard first of a note is the note, and an echo of it one `time` later.
  Parameters: `time` (s, 0.001–2, default 0.375), `feedback` (0–0.95, default 0.45), `tone`
  (0–1, default 0.7: a one-pole low-pass in the feedback, exponential from 500 Hz to 20 kHz, and
  none at all from 0.999 up, so that the first echo is always the sound as it was put in and each
  repeat is darker than the one before) and `pingpong` (0–1, default 0: how far the feedback
  crosses to the other side, and the sound put in goes to the left as the sum of the two sides,
  so that a mono sound starts on the left and goes from side to side; 0 keeps the sides apart),
  and, in a section of their own as the reverb's are, `mod_rate` (0.05–10 Hz, default 0.5) and
  `mod_depth` (0–10 ms, default 0: off): a sine that moves the time of the left side, and a cosine
  that moves the right side's, a quarter of a cycle apart, by `mod_depth` milliseconds each way
  (a tape echo's wobble at 1 or 2 ms and 0.5 Hz, a chorus at 5 ms and 2 Hz; a modulator can move
  either, `mod_depth` over a range of 10 ms, where a cable to `time` has a range of 2 s). A change
  of the depth is gone through over 50 ms. Seven presets: four echoes (Slapback, Tape Echo, Ping-Pong, Dub Echo;
  Tape Echo has a wobble of 1.5 ms and Dub Echo of 0.8) and three that are not, a short time that its own wobble swings: Chorus (20 ms, a wobble of 3.5 ms at 0.8 Hz, which swings the pitch of what
  comes out by 30 cents each way), Flanger (3 ms with a feedback of 0.6 and a wobble of 2.5 ms at 0.25 Hz: a comb whose peaks sweep, about 10 dB of level over a 1 kHz sine) and Doubler (35 ms, a wobble of 1 ms at 0.3 Hz);
  the effect's mix is the host's and starts at a half, which is what they want. A change of `time` (a knob, a
  modulator) is gone through by a one-pole of 10 ms and no faster than half a sample of delay for
  each sample that goes by, so a jump is a sweep in the pitch of the echoes, within an octave
  down and a fifth up, as on a tape echo, and never a click or a held sample; the delay's place
  is a 64-bit number, since a one-pole in 32 bits stops short of what it follows by as much as the
  steps of its number are over its coefficient (0.1 of a sample at 4,800 samples, and then every
  echo is a fraction of a sample from where it was asked for). `feedback` and `pingpong` are gone
  through over 5 ms. The echoes are soft-clipped (`soft_clip`, shared with the reverb), so that a
  feedback of 0.95 on a loud sound stays within full scale, and a sound that is not a number is not
  put in (it would stay in the echoes for ever). The lines are made for the longest time, and
  nothing is allocated while it runs. `examples/echo.toml` is a session with it.
- **Drive** (`builtin:drive`) — a stereo waveshaper, put in fully wet as a filter is (`default_effect_mix`: it is an insert, and half of it is a parallel distortion that is a choice and not a default; it is late by 23 frames, 0.48 ms, which it says (`Plugin::latency`) and the host makes up for, so that a drive that is blended with the sound that went in is in step with it and no comb: see "Latency"). Parameters: `type` (a choice of four shapes), `drive` (0–1, default 0.35: the gain into the shape, 0 to 36 dB, the knob linear in decibels so that a modulator moves it by the ear's steps), `tone` (0–1, default 1: a one-pole low-pass after the shape, exponential from 500 Hz to 20 kHz and none at all from 0.999 up, as the delay's) and `level` (0–1, default 0.5: the gain out). `drive` and `level` are gone to over about 5 ms (the drive by its knob, so that the gain glides by a ratio a sample and a step of the knob is not a step of the gain: `the_gain_is_gone_to_over_a_few_milliseconds_and_not_jumped_to`, `the_level_is_gone_to_and_not_jumped_to`), and begin at what is set and not on their way from the defaults (`a_drive_that_is_set_before_it_runs_and_one_that_is_given_a_preset_start_at_what_is_set`). The four shapes (`shape`) all have a slope of 1 at the middle, so that no drive is no change (`a_quiet_sound_with_no_drive_comes_out_as_it_went_in_late_by_the_latency_and_at_the_level`): `soft` is the hyperbolic tangent (rounded peaks, the odd harmonics), `hard` a line that stops at ±1 (a fuzz: a square at the top of the knob), `fold` a line that turns back at ±1 and goes on (a triangle with a period of 4: a sine that is driven hard is folded again and again, every fold more harmonics and a more metallic sound, the wavefolder of the West-coast synths; it is bounded by 1, as the others are) and `tube` the tangent with its middle moved off to one side (`TUBE_BIAS`, 0.3), so that the two sides of a wave are not alike and it has the even harmonics as well as the odd (`the_symmetric_shapes_make_the_odd_harmonics_only_and_the_tube_makes_the_even_ones_too`); the offset that makes is taken out by a high-pass at 15 Hz. The shape is made at twice the rate (a half-band filter up and another down, 47 taps with a Kaiser window, `half_band_taps`: flat to 16 kHz within 0.1 dB, 6.02 dB down at the Nyquist frequency of the input, 45.6 dB down at 28 kHz and past 76 dB from 30 kHz, at the doubled rate), since a shape with a corner makes harmonics high above the Nyquist frequency that fold down into the band as notes that are in tune with nothing. A sine of 3137 Hz through the hard shape at the top of the knob has 0.3% of its power in no harmonic (-25.3 dB; without the oversampling 3.2%, -15 dB), and at half of the knob 0.0045% (-43.5 dB); the soft shape and the tube are at -28.5 dB at the top and -47 at half, and the fold at half of the knob is at -33.8 dB (`what_the_shapes_make_above_the_nyquist_frequency_is_mostly_not_folded_down_into_the_band`): the corners of a hard shape and of a fold go on for ever, so that the top of the knob still folds a good part of them. A square that is made of a band of frequencies rings: the hard shape's peak is 13% over its level, and a fold's corners are rounded off (its peak is 0.93 of the level). A sound that is not a number is not put in, and what is put in is held to ±1000 after the gain (a fold of infinity is not a number). Four presets set all four parameters, level-matched so that each comes out 1.32 to 1.42 times as loud as a sine of 0.25 that goes in (2.4 to 3 dB over it): Warm (soft), Crunch (tube), Fuzz (hard) and Wavefolder (fold). `examples/fold.toml` is a session with it.
TODO: the hard shape and the fold still alias at the top of the knob (-25 dB for a sine through the hard shape): antiderivative anti-aliasing (ADAA) on top of the doubled rate would take the corners' harmonics down further, at the price of a half-sample of delay and a mild low-pass in the straight part of the curve; not here.
- **Math** (`builtin:math`, step 95) — an effect that makes a new sound of two, and the one effect that has **two inputs** (see "An effect with a second input"): **A** is what its chain and the cables at its first input give it, **B** is what the cables at its second input bring or, while
  nothing is patched there, **a carrier of its own** (`pitch`, a note number from −48 to 120 where 57 is A3 and 69 is A4, and `wave`: sine, triangle, saw or square; it goes on whether it is heard or not, so that it is where it should be when it is, and begins at the start of its wave). B goes from the carrier to the cables' sound and back over about 5 ms when a cable is patched or taken
  out, so that neither is a click (`a_cable_that_is_patched_or_taken_out_is_not_a_click`, `with_no_cable_at_b_the_carrier_is_b_and_it_has_the_pitch_that_is_set`). Parameters: `mode` (a choice of twelve), `amount` (0–1, default 0.5), `bits` (a choice of 1 to 12, default 6, in the section "Logic"), `pitch` and `wave` (the section "Carrier"),
  `attack` (0.5 ms to 0.2 s) and `release` (5 ms to 1.5 s) of the follower (the section "Follower") and `level` (0–1, default 0.7: the gain out). `amount`, `level` and `pitch` are gone to over about 5 ms and begin at what is set. The modes, sample by sample, channel by channel (a cable of one channel is B on both sides):

  | mode | what it makes |
  |------|---------------|
  | `ring` | A × B: the sum and the difference of the pitches, and neither pitch (`a_ring_of_two_sines_has_their_sum_and_their_difference_and_neither_of_them`) |
  | `am` | A × (1 − `amount` + `amount` × (B + 1) / 2): a tremolo that becomes a ring as `amount` goes up |
  | `fold` | A + 3 × `amount` × B, folded back at ±1: a wavefolder that B moves |
  | `warp` | A, read at a delay that B moves (0 to `amount` × 10 ms, no latency of its own): vibrato when B is slow, FM when B is a tone |
  | `compare` | whether A is above B, as +1 or −1 (`amount` is how hard: soft is a difference that is limited): pulse width modulation when A is a wave and B is slow |
  | `min`, `max` | the lower and the higher of the two |
  | `xor`, `and`, `or` | the two taken as numbers of `bits` bits, and the logic of their bits: the buzz of a digital circuit |
  | `hold` | A, held at each place where B goes up through zero (a sample and hold clocked by B; `amount` glides from step to step, over 5 ms at the most) |
  | `follow` | A, turned down by how loud B is (`amount` is how far: a sidechain, a ducker; the follower takes the louder of B's two channels, rises over `attack` and lets go over `release`: `a_follow_turns_a_down_by_how_loud_b_has_been`) |

  The modes that make an offset (`fold`, `compare`, `min`, `max` and the logic) have it taken out by a high-pass at 15 Hz (`the_modes_that_make_an_offset_have_it_taken_out`); the others put out exactly what the arithmetic says. Nothing is band-limited (a ring of two bright sounds, a hard comparison and a logic of bits make partials above the Nyquist frequency that
  fold down into the band, which is some of what these sound like), a sound that is not a number is not put in, and nothing allocates. Nine presets: Ring Bell, Tremolo, Pump (a follower whose own carrier is a sine of a little over 1 Hz, so that it pumps with nothing patched), Xor Growl, Square Up, Fold Sweep, Decimator, Vibrato and Warp Metal. It is added fully wet, as a filter is
  (`default_effect_mix`). A parameter that has no effect in the state it is in (`amount`, `bits`, `attack` and `release` in the modes that do not read them, `pitch` and `wave` while a cable is patched at B) says so, and is dimmed (step 95b: see "A parameter that has an effect only sometimes").
  `examples/ring-bell.toml`, `growl.toml` and `pump.toml` are sessions with it.
TODO: the carrier is one sine, triangle, saw or square that is the same for both channels (a stereo B comes from a cable), the logic modes quantize to `bits` bits but do not lower the sample rate, and there is no mode that mixes (A + B) or crossfades: a cable to the mix does that.

TODO: a delay that follows the session's tempo (a `time` that is a note value at `bpm`), and a
finer step for a modulator's depth (a cable's depth is a fraction of the range, so that ±5 ms on
the `time` of a delay is 0.0025 of its 2 s, finer than the page's slider (0.01) and the terminal's
keys set it: a file can say it, as `examples/echo.toml` does, and `mod_depth` has the milliseconds).

## Plugin I/O handling

How Tang handles common plugin I/O mismatches:

- **Multi-output instruments** (e.g. Pianoteq with 16 outputs): the chain uses
  the first stereo pair (channels 1-2) and discards the rest. This is the standard
  main mix output for all known multi-output instruments.

- **VST3 parameter lists**: a VST3 plugin's parameters are listed without those that it hides (`kIsHidden`) or says are read only
  (`kIsReadOnly`), and without the placeholders that it makes so that a host can send MIDI controllers through its parameters: the
  parameters that its `IMidiMapping` points at, over 16 channels and 130 controllers, that cannot be automated (`is_listed`,
  `midi_controller_parameters` in `src/plugin/vst3.rs`). Pianoteq 9 has 2,080 of them among its 2,294 and ROLI Airwave Player 2,080
  among 2,089, so what is left is 214 and 9; `tang enumerate` counts what is listed. The routing of controllers to the plugin is
  by parameter id and is as it was. (A test of Pianoteq 9 is `#[ignore]`d: `cargo test -- --ignored`, with it installed.)
  A VST3 parameter that has 1 to 64 steps (`stepCount`) is a list, an enum to the page and the terminal: its labels are what the plugin says at
  each step (`getParamStringByValue`: "Off" and "On", "EXP" and "LIN", Studio Drums' "Scale Type" and "Root Note", Pianoteq's 30 "Bounce Sync
  Speed"s), and its value is the step, 0 to n, since VST3's steps are k/n whatever the plain range that the plugin says (`is_list`,
  `step_to_normalized`; Pianoteq's plain ranges are all 0 to 1). A session that has such a parameter at a plain value that is not a step (0.34 for
  the 10th of 29) reads it as the step 0.34, which the model rounds: only a list of more than two, of a plugin whose plain range is 0 to 1.

- **Latency**: a plugin says by how many frames what it makes is late (`Plugin::latency`: a CLAP plugin's latency extension and a VST3 plugin's `getLatencySamples`, asked once when the plugin is activated, an LV2 plugin's
  `lv2:reportsLatency` port, read every block), and the host puts the paths that are added in step by it; see "Latency".

- **Sidechain inputs**: effects with more audio inputs than the chain provides
  (e.g. Calf Reverb with 2 audio + 1 sidechain = 3 inputs) get silence on the
  extra ports. Sidechaining is not supported for a plugin of these formats; the built-in Math has a second input of its own that cables end at (see "An effect with a second input").

- **Atom sequence ports (LV2)**: only passed to plugins that declare atom
  sequence input ports. Effects without them (e.g. ACE Reverb) get no event
  buffer — avoids livi's AtomSequenceInputsSizeMismatch error.

- **Channel count validation**: the chains are stereo (`plugin::chain::CHAIN_CHANNELS`,
  2), and an effect must have as many audio outputs. The audio thread turns away
  one that has not (it logs why and hands the plugin back), and one rule
  (`app::controller::outputs_fit_the_chain`) refuses it first: in
  `Controller::add_effect` / `add_bus_effect`, in undo (a plugin that is loaded
  again) and in the loader, so the model never lists an effect the engine does not
  have. A session file that names one is refused when it is loaded, with its place in
  the file and the plugin's own name (`instrument[0].effect[1]: Foo has 1 audio
  output, and an effect here needs 2`; `group[0].effect[0]: …` for a group's bus).
  The instrument may have more outputs (the first two are used, the rest discarded).
