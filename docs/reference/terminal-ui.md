# The terminal interface

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): the tabs, the Session tab and its popups, the Piano, Scope and Patch tabs, logging, the TUI's tests. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

## TUI

### Splash screen

While session plugins load (which can take seconds for heavyweight
instruments like Pianoteq), the TUI shows a splash screen: TANG logo,
animated spinner, the name of the plugin currently loading, and a progress
bar over plugin slots (instruments + effects). Implemented in
`src/tui/splash.rs` — a transient render thread owns the terminal and
animates at ~12 fps while the main thread loads plugins and reports progress
over a channel. Dropping the `Splash` handle restores the terminal, so load
errors clean up automatically before the error prints. TUI mode only; `play`
mode keeps plain log output. While the splash is up, logging to a terminal
stderr is suppressed (redirected stderr still gets logs).

The interface is tab-based.
The status bar at the top shows all tabs with the active one highlighted.
The global BPM is displayed on the right side of the status bar (e.g. `120 BPM`).
With `--web`, left of the CLIP light and the BPM, it says where the web interface
listens and how many browsers are connected (`web 127.0.0.1:8765 (2)`; `web (2)`
on a narrow terminal, nothing if even that would touch the tabs).

A `LIM -3.2 dB` light (bold yellow) sits left of the CLIP light while the limiter is in the sound and has brought the sound down by a tenth of a decibel or more lately (step 102: see "The master limiter and volume").

A `CLIP` light (bold red) sits on the right side of the status bar, left of the
BPM, while the output has clipped lately. The audio thread counts every buffer
whose mixed output has a sample above full scale (`|s| > 1.0`; exactly 1.0 is
not a clip) in a telemetry slot (`telemetry::MASTER.clips`, `src/telemetry.rs`);
the TUI lights `CLIP` when that count has moved since it last looked (once per
frame, `src/tui/clip.rs`) and keeps it on for 2 seconds after the last move.
The count only goes up, so any number of readers (the web runtime later) can
watch it without resetting it. Clips from before the TUI started do not light it.

### Tabs

| # | Tab | Description |
|---|-----|-------------|
| 1 | Session | Instrument and effects chain editor with parameter control |
| 2 | Piano | Virtual piano using computer keyboard |
| 3 | Scope | The output as a wave or as a spectrum (see "Scope tab") |
| 4 | Patch | The audio cables as a matrix of outputs against inputs, and the way out, mute and solo of the lanes and groups (see "Patch tab") |
| 5 | Help | Keybindings and usage reference (static, scrollable) |

TODO: the Piano tab draws the keyboard, the scale and the notes being held (see "Piano
tab"), but takes no note input yet.

### Global keybindings

These work from any tab (except where noted for the Piano tab):

| Key | Action |
|-----|--------|
| `1` `2` `3` `4` `5` | Switch to tab by number |
| `Tab` | Next tab |
| `Shift+Tab` | Previous tab |
| `Ctrl+Q` | Quit |
| `Ctrl+C` | Quit |
| `Ctrl+P` | Panic: let go of every note that sounds (see "Built-in plugins"; not heard while a popup is open) |

TODO: `?` shortcut to jump to the Help tab.

On the Piano tab, alphanumeric keys are captured for note input. Tab switching
uses `Tab`/`Shift+Tab` only.

A popup has the keys while it is open (so has the filter of the parameters, `/`).
What it takes as text is text, and a letter with Control held is none of it: a
popup ignores such a chord and does not put the letter in (`Ctrl+Z`, which undoes in
the chain, would type a z), except `Ctrl+C` and `Ctrl+Q`, which leave the program
from a popup as from the chain. Control with Alt is how Windows reports AltGr (`@`
and `\` on some keyboards), and is text; so is a capital letter (Shift).

### Session tab

The session tab has two panes: the chain (left) and parameters (right).
The focused pane is visually distinct so it's always clear which pane is active.
`Enter` moves focus to the parameter pane, `Esc` moves it back to the chain.
`Up`/`Down` navigate within whichever pane has focus.

#### Chain (left pane)

The chain is rendered as a tree. Instruments are top-level nodes, with effects
and the modulators that hear the lane nested under each instrument.

```
▦ Pad  vol 0.80
  ♪ Helm [LV2]            C0-B3
  ├─ fx Reverb [LV2]
  └─ ~ LFO 0.5Hz sine → Helm: cutoff (50%)
  ♪ Oscillator [Built-in]  C4-C8
  fx Compressor [LV2]  (bus)
  ~ LFO 0.2Hz sine → M0 Helm: cutoff (40%), Bus: Compressor: threshold (25%)  (bus)
♪ Bass [Built-in]
└─ fx Delay [CLAP]
```

Navigation is a single cursor through the flattened tree. Actions (`i`/`a`/`d`/`m`)
operate on the instrument containing the selected node (on a modulator, the lane or the group it hears: see "The racks go (step 96)"). A modulator that hears the lane
is listed under it — in magenta after the effects — and a
single modulator's targets can span the whole session (note the example LFO
driving the Helm instrument's cutoff), each labelled by the plugin it lands on.

Groups render as a Cyan header (`▦ name  vol`) with their member instruments
nested under them, then the group's bus effects (`(bus)`) and finally the
modulators that hear the group (magenta, also tagged `(bus)`). Such a modulator's
target labels show `M<n> <plugin>: <param>` for a member instrument and
`Bus <effect>: <param>` for a bus effect.

Free modulators (modulators that are in no lane and no group: see "Free modulators") that hear every note come first, above the first group, in magenta and not indented; each says what its cables
drive by the lane or the group of the plugin, as the Patch tab tags them (`L1 Oscillator: detune`, `G1 Filter (bus): cutoff`), and ends in `(free)`. Since step 94 every modulator is one of these (and since step 96 there is no other kind), and the ones that hear a lane or a group are
listed with it, as a lane's rack and a group's were: under the lane after its effects (`├─ ~ LFO 0.5Hz sine → Oscillator: detune (4%)`: the plugins of its own lane by their names, the others tagged) and after the group's bus effects (`~ LFO … → M0 Oscillator: detune (40%), Bus Reverb: damping (25%)  (bus)`).

The selected entry is highlighted (bold, white). Unselected entries are dimmed. A group's header is cyan
and a modulator magenta, a group's own included: they keep their colour, bold when selected and dimmed when not
(`TreeEntry::color`; the other entries have none of their own).

#### Parameters (right pane)

Shows all parameters for the selected plugin as horizontal bars:

```
  cutoff          ▓▓▓▓▓▓▓▓░░░░ 0.75
  resonance       ▓▓▓░░░░░░░░░ 0.25
▸ attack          ▓░░░░░░░░░░░ 0.05
  decay           ▓▓▓▓░░░░░░░░ 0.30
```

The selected parameter is marked with `▸`. The bar shows the parameter's
position within its min–max range. The numeric value is shown on the right.

Plugin parameters that declare value labels (`ParameterInfo::labels`, e.g.
the built-in filter's `type`) render as enums instead of bars — shown as
`◂ lowpass ▸` — and step one named value at a time with `Left`/`Right`
(or by clicking the left/right half of the row). Numeric entry is disabled
for enum parameters. In the session file they are still stored as plain
numeric `params` values (the index into the label list).

A parameter that has an effect only sometimes (`ParameterInfo::used_when`: see "A parameter that has an effect only sometimes") is dimmed while it has none, and while the cursor is in the pane and on it the line under the pane says why (`pitch has no effect now: only while nothing is patched at B`).

| Key | Action |
|-----|--------|
| `Down` / `Up` | Move selection in focused pane |
| `PageDown` / `PageUp` | Jump selection by a page |
| `Shift+Down` / `Shift+Up` | Move selected effect down/up (reorder, focus follows) |
| `n` | Add a new instrument (defaults to `builtin:osc`, opens the plugin selector to replace it; full range until set with `R`; a step of the history, which the replacement that follows empties; needs the plugin catalog to name the oscillator, which the startup scan does at once) |
| `i` | Replace instrument (opens instrument selector popup). The lane keeps its effects, modulators, range, volume and group; the cables that drove the old plugin's parameters are taken out (the new plugin has parameters of its own), and a note remap goes with the old plugin |
| `R` | Set the selected instrument's key range (opens range popup, prefilled) — or, on a group header, rename the group (opens rename popup, prefilled) |
| `v` | Set the output volume of the selected instrument or group (host-side gain, opens value popup) |
| `M` / `S` | Mute / solo the lane or group the selected entry is of, as the intents `SetMute` and `SetSolo` do (see "Mute and solo"): the instrument, its effects, modulators and pattern are the lane's, a group's header, bus effects and modulators are the group's. They toggle, are steps of the history, and show at the end of the line as `[mute]`, `[solo]` or both (`▦ Pad  [mute]`, `♪ Oscillator  [Built-in]  [mute] [solo]`); the action bar of an instrument and of a group has them (and, so that the bar fits 120 columns, `replace`, `vol` and `add fx`) |
| `w` | Cut / give back the way out of the lane or group the selected entry is of, as the intent `SetRoute` does (see "The way out"): the lane's or group's end goes nowhere but its audio cables. It toggles, is a step of the history, is refused (logged) where giving it back would make the audio go round, and shows at the end of the line as `[no out]`, after `[mute]` and `[solo]`; the Help tab lists it, the action bar does not; the Patch tab has it too, for the row's lane or group |
| `f` | Cut / give back the link into the effect the cursor is on (a lane's or a group's bus), as the intent `SetFeed` does (see "The input of an effect"): the effect is then fed by its audio cables and by nothing else, and the chain goes on from it. It toggles, is a step of the history, is refused (logged) where giving it back would make the audio go round, and shows at the end of the line as `[no in]`; the Help tab lists it, the action bar does not |
| `N` | Add an effect that stands by itself (opens the effect selector; chain focus, whatever the cursor is on): the intent `AddFreeEffect` puts it in a lane of its own that has no instrument and no way out, where audio cables feed it and a cable from it sends it on (see "An effect that stands by itself"); a step of the history. The selector lists the modules of the library first (see "Modules: effects and instruments, with the cables between them, saved to a file"): one that is chosen is added whole, in a lane for each of its own, with its plugins and its cables. The tree shows the lane as `♪ (empty)` with the effect under it; the Help tab lists it, the action bar does not |
| `K` | Keep the chain of effects of the lane the cursor is of (on the lane, one of its effects, its pattern or a modulator that hears it; chain focus) as a module of the library: a popup, "Save Chain as Module", asks for the name, and Enter asks the intent `SaveModule` (see "Modules: effects and instruments, with the cables between them, saved to a file"); nothing opens for a lane that has no effect, a group, the other tabs or the parameter pane. A module changes nothing of the session: it is not marked, and there is nothing to undo; the Help tab lists it, the action bar does not |
| `F` | Add a modulator that hears every note (an LFO that is in no lane and no group: chain focus, whatever the cursor is on): the intent `AddFreeModulator` puts it at the end of the session's own rack and the cursor goes onto it; its cables reach any plugin of the session (`t`) and the settings of the other modulators. A step of the history; the tree shows it first, in magenta, ending in `(free)`; the action bar of a free modulator has it, the Help tab lists it (see "Free modulators") |
| `h` | Choose whose notes a modulator hears (chain focus, the cursor on a modulator; opens a popup, "Whose notes it hears": every note, `L1 Oscillator` …, `G1 Group 1` …, the one it hears marked): the intent `SetModulatorEar`, a step of the history; the tree says `(free, hears L2)`; the action bar of a free modulator has it and the Help tab lists it (see "Free modulators", "The ear") |
| `L` | Put the limiter on the output mix in the sound, or take it out (the master's, chain focus, whatever the cursor is on): the intent `SetLimiter`, a step of the history; it is in the sound from the start (see "The master limiter and volume"); the Help tab lists it, the action bar does not |
| `C` | Set the limiter's ceiling, in dB below full scale, −24 to 0 (opens a value popup): `SetGain` of the master's `LimiterCeiling`, a step of the history |
| `V` | Set the master volume, 0 to 2, the gain on the output mix before the limiter (opens a value popup): `SetGain` of the master's `MasterVolume`, a step of the history |
| `a` | Add effect to the selected instrument, or — on a group/group-effect node — to the group's bus chain (opens effect selector popup). New effects default to 0.5 mix (half-wet); `builtin:filter` defaults to 1.0 (fully wet, since a half-wet filter barely filters), and so does `builtin:drive` (its latency would put the dry sound out of step), and `builtin:math` (an insert: the sound that it makes of its two inputs is the point) |
| `m` | Chain focus: add an empty LFO that hears the lane or the group the cursor is of (one of the session's own: step 94), or, on a modulator, another that hears what it hears (a lane, a group, or every note if it has no ear, as `F` does: step 96). Param focus: modulate the selected parameter — opens a popup to bind it to a new (LFO / envelope / macro / velocity / key / step sequencer) modulator, which hears the lane of the plugin or the group of the bus effect, or to any modulator of the session's own (each listed with what it hears: `attach → LFO 0.5Hz sine (hears L2)`; a modulator drives any plugin); a **modulator's own param** binds a sibling modulator for cross-mod (one that hears what it hears); a new modulator and its cable are one step of the history, and a modulator that drives the parameter already is not attached again) |
| `l` | Param focus: MIDI-learn the selected parameter — arms a MIDI modulator and binds the next incoming CC / pitch-bend / aftertouch to it (works on plugin params and modulator fields) |
| `x` | Set the dry/wet mix of the selected effect or group bus effect (host-side blend, opens value popup) |
| `g` | Assign the selected instrument to a submix group (new / existing / ungroup, opens popup) |
| `p` | Open the preset selector for the selected instrument or effect (only if its plugin has presets) |
| `d` | Delete selected instrument/effect/modulator/pattern/group/group-effect (no confirmation; deleting a group ungroups its members; on an instrument that has a plugin it takes the plugin out and leaves the lane, empty, with its effects and modulators: `ClearInstrument`, a step of the history; a second `d` takes the lane out) |
| `t` | Add modulation target (when a modulator is selected, opens the target selector, which lists every parameter of every plugin of the session, each labelled by its lane or group (`L1 Oscillator`, `G1 Filter`), and the settings of the other modulators, and since step 97 the volume of each lane that has an instrument (`L1 Oscillator` / `output volume`), the mix of each effect (`dry/wet mix`) and the volume and the pan of each group, after the plugins' parameters; before step 96 a lane's and a group's modulator had a selector of their own that listed a smaller reach) |
| `Enter` | Focus parameter list / open value editor on selected parameter |
| `Esc` | Back to chain focus / close popup |
| `Left` / `Right` | Decrease / increase selected parameter |
| `Shift+Left` / `Shift+Right` | Fine decrease / increase selected parameter |
| `Ctrl+Left` / `Ctrl+Right` | Coarse decrease / increase selected parameter |
| `/` | Filter parameter list by name |
| `r` | Record/stop pattern for current instrument |
| `Ctrl+R` | Clear the pattern of the selected instrument or pattern (chain focus; `d` on the pattern does the same) |
| `b` | Set the tempo patterns are recorded at, in BPM (opens value entry popup; the `SetBpm` intent: held to 20–300, a step of the history, saved as `bpm`) |
| `c` | Send a MIDI CC message (any focus; popup takes `<cc> <value> [channel]`, channel defaults to 1, which reaches every instrument; prefilled with the last message sent). E.g. `64 80` presses the sustain pedal, `64 0` releases it. Sent as the keys' source (`Source::KEYS`), like the virtual piano, so an armed MIDI-learn modulator captures it and a lane wired to hear only some devices does not get it |
| `o` | Show / hide the scope of the node under the chain cursor, in a strip under the chain and the parameters (see "Scope tab"; a terminal of 24 rows or more) |
| `O` | Wave or spectrum of that scope |
| `Ctrl+S` | Save session (to the file it was loaded from) |
| `Ctrl+Shift+S` | Save session as (prompts for filename, saves in the current session's directory) |
| `Ctrl+Z` | Undo the last thing done (the history of the controller, which the browser's Undo has too; not heard while a popup is open) |
| `Ctrl+Y` / `Ctrl+Shift+Z` | Redo |

A step of `Left` / `Right` is a twentieth of the range of the row (a hundredth with `Shift`, a tenth with
`Ctrl`). The rate of an LFO and the times of an envelope run over decades (0.01 to 50 Hz, 1 ms to 10 s), so
for them the range is a ratio and a step multiplies by that fraction of it: a rate by 1.53 (1.09 with `Shift`, 2.34 with
`Ctrl`), a time by 1.58 (1.10, 2.51), and `Left` divides by the same, so a step up and a step down of one size give back
the value. It is the scale that the page's knobs turn them on (`ModulatorField::taper`, which the page reads from
the generated constants). A sustain and the depth of a cable move by 0.05 (0.01, 0.10), and an LFO's fade, which runs from 0 to 5 seconds, by a twentieth of that, 0.25 s (0.05, 0.5).
TODO: the bar of a rate or a time is still linear (0.01 to 50 Hz in one scale) though a step on it is a ratio, and so is a drag
on it with the mouse; a bar and a drag on the log scale would match a step and the page's knob.

Mouse: click to select, drag a parameter bar to set its value, drag the
parameter scrollbar to scroll. Mouse wheel scrolls the parameter list and the
Help tab.

TODO: `p` on a group bus effect, and a "≡" menu on one in the page: the controller refuses `LoadPreset` for a
bus effect (`a bus effect has no presets to load`), since the audio thread's `LoadPreset` command addresses a
lane's slot; a command for a group's chain would do it.
TODO: `e` shortcut to open the value entry popup directly (currently use
`Enter` from chain focus or land on a param and press `Enter`).

### Plugin selector popup

Opened by `i` (instruments only) or `a` (effects only). Same layout for both,
filtered by plugin type.

- **Top**: text input for filtering (matches against any column: name, type, etc.)
- **Below**: table with columns — Name, Format (LV2/CLAP/VST3), Params, Presets
- `Up` / `Down` — navigate rows
- `Enter` — select plugin and close popup
- `Escape` — cancel and close popup
- Typing updates the filter immediately

A built-in is added at once. Any other plugin can take a while to load (Pianoteq 9 takes about a third of a second here: `tang enumerate plugins` says), and the loop is blocked for as long as
it takes, so a frame is drawn first with a box in the middle of the screen that says `Loading <name>…`
(`State::pending_load`: `handle_event` takes the key, the loop draws, `run_pending` asks the controller for the plugin;
`process_event` is the two together, for the tests' keys, which have no frame between them).

The selector of `N` (an effect that stands by itself) lists the modules of the library first, with the format `Module` and their number of effects where a plugin has its number of parameters, and then the plugins; a module that is chosen is added whole, after the same
frame that says `Loading <name>…` (its file names the plugins to load). The selectors of `a`, `n` and `i` do not list modules (see "Modules: effects and instruments, with the cables between them, saved to a file").

The plugin catalog is scanned on a background thread started when the TUI
launches (a full scan instantiates every installed plugin and can take
seconds, so it must not block the first frame). Results arrive per format
(built-ins, LV2, CLAP, VST3) and the popup fills in progressively; while the
scan is still running the popup title shows "(scanning…)".

### Preset selector popup

Opened by `p` on an instrument or an effect in the chain (not on a group bus
effect yet). Titled "Select Preset". Nothing opens if the plugin has no presets.

- **Top**: text input for filtering by name
- **Below**: single-column list of preset names, starting on the plugin's
  current preset
- `Up` / `Down` — navigate rows
- `Enter` — load the highlighted preset (the `LoadPreset` intent) and close popup
- `Escape` — cancel and close popup

### Value entry popup

Opened by `Enter` on a selected parameter (and also used for `b` BPM entry,
`x` effect mix entry, and `v` instrument volume entry).

- Shows parameter name, current value, and valid range
- Text input for entering a numeric value
- `Enter` — accept value and close popup; text that is not a finite number
  (`abc`, `nan`, `inf`) is ignored: the popup closes and nothing changes
- `Escape` — cancel and close popup

### Range entry popup

Opened by `R` to set the selected instrument's key range. Titled "Set Range"
and prefilled with the current range.

- Text input for a note range like `C0-B3`. Either bound may be omitted:
  `C4-` (C4 and up), `-B3` (up to B3), or empty (full range)
- `Enter` — accept and close popup (kept open on parse error)
- `Escape` — cancel and close popup

To build a split: press `n` to add a new instrument (this adds a full-range
lane defaulting to `builtin:osc` and opens the plugin selector to replace it —
cancelling the selector leaves the oscillator in place), then press `R` to set
that lane's key range. Repeat for each split zone.

### Rename group popup

Opened by `R` on a group header (range doesn't apply to groups). Titled
"Rename Group" and prefilled with the current name.

- Text input for the group's display name. Empty clears the custom name (the
  header falls back to "Group N"). The name is display-only (not sent to the
  audio thread), so it just updates the model and marks the session dirty.
- `Enter` — accept and close popup
- `Escape` — cancel and close popup

### Save As popup

Opened by `Ctrl+Shift+S`. Titled "Save As" and prefilled with the current
session's filename.

- Text input for a filename, which goes through the intent `SaveAs { name }` (the
  one the browser's "Save as…" asks too), so the name is checked by
  `session::plain_file_name`: the spaces round it go, `.toml` is appended if
  missing, and it has to be a plain file name: no `/` or `\`, none of `:*?"<>|`
  or a control character, something before the extension, not a name that
  Windows keeps for itself (`con`, `nul`, `com1`…), at most 255 bytes. The file is
  saved in the current session's directory (cwd if there is no current
  session). A file that is there already is not written over (a link that leads
  nowhere counts as there), unless it is the session's own file: Save As to the
  name the session has is a save.
- `Enter` — save and close; `session_path` is updated so subsequent `Ctrl+S`
  targets the new file, and the browsers are told (`Saved { file }`). On failure
  (or an empty field) the popup stays open — the reason is shown in the popup
  and `session_path` keeps its old value.
- `Escape` — cancel and close popup

Distinguishing `Ctrl+Shift+S` from `Ctrl+S` requires the kitty keyboard
protocol (the TUI pushes `DISAMBIGUATE_ESCAPE_CODES` when the terminal
supports it). On terminals without it, the SHIFT modifier is not reported
for Ctrl+letter chords and `Ctrl+Shift+S` performs a plain save instead.

### Modulation target selector popup

Opened by `t` when a modulator is selected. Since step 96 every modulator is one of the session's own and the list is the same for all: every parameter of every plugin of the session,
each labelled by the lane or the group it is in (`L1 Oscillator`, `G1 Filter (bus)`), and the rate, the stages and the cable depths of the other modulators (`Mod 1 rate`, by the place that the other has in the session's rack). Since step 97 the list has the values that the host keeps as well, after the parameters, lane by lane and group by group (`L1 Oscillator` / `output volume`,
`L1 Filter` / `dry/wet mix`, `G1 Pad` / `volume` and `pan`: see "The volume, the mix and the pan that a modulator drives").
(Until step 96 a **lane** modulator listed the parameters of its own instrument chain and its sibling lane modulators', and a **group** modulator every member instrument's params (`M<n> <plugin>`), each
member's modulators (`M<n> Mod<k>`), the group's bus effects (`Bus <effect>`) and its sibling group modulators'.)

- `Up` / `Down` — navigate rows
- `Enter` — bind target and close popup
- `Escape` — cancel and close popup

### Modulate popup

Opened by `m` while focused on a parameter (the parameter-centric way to wire
modulation). Lists: "New LFO", "New envelope", "New macro", "New velocity", "New key",
"New step sequencer" (a velocity or a key follows the notes the lane plays: see
"MIDI-learn modulator"; a step sequencer is last, so that what a person's fingers
know does not move),
and each modulator of the session's own ("attach → … (hears L2)"; `NEW_MODULATORS`,
`src/tui/mod.rs`, is the table of the new ones). It works on:
- an **instrument/effect** param → a new modulator of the session's own that hears the instrument's **lane**, or any modulator of the session's own to attach;
- a **group bus-effect** param → a new modulator of the session's own that hears the **group**, or any modulator of the session's own to attach;
- a group's **Volume** or **Pan** row (step 97) → the same, hearing the group, bound to the host-side value (`gain`);
- a **modulator's own** param (its rate / ADSR stage / a target depth) →
  **cross-modulation**: creates or attaches a *sibling* modulator that drives this field (a new one hears what the modulator whose field it is hears). The modulator can't modulate itself (it's
  excluded from the list), and enum/separator rows aren't modulatable.

- `Up` / `Down` — navigate rows
- `Enter` — create the chosen modulator (if new) and bind the selected
  parameter to it as a target (depth 0.5), then close
- `Escape` — cancel and close popup

This is the inverse of `t`: `t` starts from a modulator and picks a parameter;
`m` starts from a parameter and picks/creates a modulator. A modulator that hears a group and targets a *member* instrument is wired from the modulator side (`t`) or by `m` on the member's parameter (every modulator reaches every plugin).

### Piano tab

Visual piano keyboard for learning scales and seeing what's playing in
real time.

Layout: status line on top (scale + view octave + currently-held notes
with degree labels), the keyboard itself in the middle, a "scale strip"
underneath showing the notes of the scale, and a hint line at the bottom.

The keyboard auto-sizes to the terminal width — it tries 4 octaves and
the widest white-key width that fits, falling back to narrower or fewer
octaves as needed. The view is centered on `piano_view_octave`.

The keys are a natural piano (white and black bodies) with a stripe along the
bottom of each for what it is in the scale: **gold** for the root, **teal** for
the other scale tones, none for the rest. A key that is held sinks (its body
goes a shade grayer, the stripe stays); one that is held and is **not in the
scale** goes red, the "wrong note" (in Locked mode it never appears: off-scale
notes cannot be held).

| Key | Action |
|-----|--------|
| `k` | Open scale picker (root + scale type, filterable); the choice is the `SetScale` intent |
| `l` | Toggle Locked mode: off-scale notes are dropped at the sender (Highlight, the default, only colours them); it is the `SetLocked` intent |
| `[` | Shift view octave down |
| `]` | Shift view octave up |

The browser page has the same piano under its canvas, with the same colours
(see "Web UI"); a scale or a lock chosen in either is seen in the other.

The scale persists in the session file under a top-level `[piano]`
section:

```toml
[piano]
scale = "C major"
locked = true           # optional; Locked mode, default false (Highlight)
```

Accepted forms include sharps (`C#`), flats (`Bb`), short names
(`major`, `minor`, `dorian`, `maj pent`, `min pent`, `blues`,
`chromatic`, `whole tone`, `diminished`), and long names
(`Natural Minor`, `Harmonic Minor`, `Melodic Minor`, etc.).

Currently-held notes are tracked in a lock-free `Arc<HeldNotes>` (a
128-bit atomic bitset). Both the hardware MIDI thread and the virtual
piano set/clear bits on NoteOn/NoteOff; the TUI reads them every render
frame (~10 fps).

TODO: Capture letter keys on the Piano tab for note input via the
virtual piano (currently the Piano tab is read-only — notes still play
from the global virtual piano which is only wired up in `play` mode).

### Scope tab

The output as it goes by, drawn on a braille canvas as a **wave** or as a **spectrum**: the picture of the page's scope on the master node (see "Web UI"), with the same trigger, time bases and spectrum
(`src/tui/scope_view.rs`: pure functions that have the tests of `web/src/client/scope.ts` and `fft.ts`). A line on top says what is shown and the loudest sample of the picture
(`Peak: -6.0 dB`: decibels below full scale, `+3.5 dB` in red when the output clips, a dash for silence), the picture is in a box, and the keys are under it.

| Key | Action |
|-----|--------|
| `v` | Wave or spectrum |
| `Left` / `Right` | A shorter / longer time base for the wave: 256, 512, 1024 or 2048 samples across (5, 11, 21 and 43 ms at 48 kHz; it starts at 1024) |

- **Wave.** The left side in teal, and the right side under it in green where it is not the same (a mono sound is one line). It is drawn from where the left side goes up through the middle after having been 0.02 under it
  (so that a wobble is not a wave), which makes a wave that goes round again and again stand still; with no such place (silence, a wave that stays on one side) it is the latest frames. Lines at 0.5, 0 and −0.5.
- **Spectrum.** The mix of the two sides: a Hann window and a 2048-point FFT of the latest 2048 frames (a bin is 23 Hz at 48 kHz, so that the low notes are a few bins wide), from 20 Hz to 20 kHz on a logarithmic axis and from 0 to −90 dB,
  with lines at 100 Hz, 1 kHz and 10 kHz and at every 20 dB. A level falls 40 dB a second when the sound is quieter than it was (a wait of more than a quarter of a second between two looks is made up for a quarter of a second of the fall).
- The frames are copied from the scope ring (`telemetry::ScopeRing`, see "Architecture") on every frame that is drawn while the tab is shown, about ten a second, so that a wave that moves fast is seen in steps. The choices
  are the tab's own: they are not saved with the session, and not told to the audio thread or to the browsers (a browser's scope has its own).

**The scope of a node** (step 47, `docs/web-ui.md` D47; `o` on the Session tab). The picture of the page's scope in a node, in a strip of ten rows under the chain and the parameters (`scope_view::Strip`, `STRIP_ROWS`): what comes out of the
node under the chain cursor, as a wave or a spectrum, drawn as the Scope tab draws (`scope_view::paint`), with the node's name, the view, the time base and the loudest sample in the top line of its box and the keys in the bottom. The node is an
instrument that has a plugin (after its volume), an effect (after its mix), a group (after its pan) or an effect of a group's bus (after its mix); what has no signal, a modulator, a pattern and a lane that has no instrument, says so in the box
(`No signal here…`), and so does a node that no ring is left for (`No ring is free…`: the engine has eight rings, for the terminal and every browser together, `Controller::probe_ring` says which a node has). `o` shows the strip and takes it away,
`O` chooses wave or spectrum; both are the strip's own (not the Scope tab's, not saved, not told to the audio thread or to the browsers), and a terminal that has not room for the strip and twelve rows of the chain and the parameters (24 rows in
all: `split_for_strip`) shows the chain and the parameters and nothing else. The engine listens to a node only while somebody watches it, so the strip watches the node under the cursor, and only while it is shown and the Session tab is: every tick
of the loop (a turn of it begins with one, after the events of the terminal and the requests of the browsers, so that a node that a browser took out is let go of) calls `Strip::sync`, which asks `Controller::set_probes(Watcher::TERMINAL, …)` when the
node is another, and forgets the picture of the node that was watched (a spectrum falls slowly, and the levels of one node would be seen under the name of the next). `Watcher::TERMINAL` is the terminal's own number, the highest there is: the
owner of the web interface lets go of the watchers of the connections that have closed (`Owner::refresh_probes`) and keeps this one, so that `tang --web` has both. The frames are copied from the node's ring on every frame that is drawn
(`Strip::take`), about ten a second.

TODO: a time base for the strip (it is 21 ms; `Left` and `Right` are the parameters' on the Session tab), `o` in the action bar of the Session tab (its hints are in about a hundred snapshots; the Help tab lists it) and a word where a
terminal is too short for the strip (it shows nothing then), and the scope's choices and the strip's being on kept with the session or the config (the page's are not either: Open question 8).
TODO: a reading of the pitch (a bin of the spectrum is 23 Hz wide: the peak could be interpolated between bins), a vertical zoom for a quiet sound (the tracer's wave, at its oscillator volume of 0.1, is two pixels high on the page's scope),
and a key that freezes the picture. The page keeps the view and the time base in the master node, so that a reload goes back to the wave at 21 ms.

### Patch tab

The audio cables of the session (see "Audio cables (`[[cable]]`)") as a matrix, the pin matrix of an old modular synthesizer (P3e of `docs/patch-graph.md` §11.6, step 79; `src/tui/patch_view.rs`): a **row** for every output of audio, a **column** for every
input of audio, and at each crosspoint the cable that is drawn from the one to the other, or none. Since step 88a the links of the audio path that a lane and a group make by themselves are cables here too, at the crosspoints that join their ends (see "Links are cables").

```
┌ Patch ───────────────────────────────────────────────────────────────────────┐
│outputs ↓  inputs →              L1   L1   L2   G1   G1  Mix                  │
│                               Filt Dela Driv  bus Reve                       │
│▪G1     L1  ♪ Oscillator          ●    ·    ·    · 0.60    ·                  │
│        L1    fx Filter           ×    ●    ·    ·    ·    ·                  │
│        L1    fx Delay            ×    ×    ·    ●    ·    ·                  │
│▪Mix    L2  ♪ Oscillator          ·    ·    ●    · 0.40    ·                  │
│        L2    fx Drive            ·    ·    ×    ·    ·    ●                  │
│▪Mix    G1  ▦ Group 1             ×    ×    ·    ×    ●    ·                  │
│        G1    fx Reverb (bus)     ×    ×    ·    ×    ×    ●                  │
│──────────────────────────────────────────────────────────────────────────────│
│L2 Oscillator (the instrument's output, after its volume)  ▸  G1 Reverb (bus) │
│A cable at level 0.40: Enter takes it out, + and - turn it, e types a level   │
│L2's way out goes to the mix (w cuts it)                                      │
└──────────────────────────────────────────────────────────────────────────────┘
```

- **Rows** (the outputs, `CableFrom`): the instrument of each lane (`♪ Oscillator`: what it puts out after the lane's volume and before its chain; `(empty)` for a lane that has none) and each effect of its chain (`  fx Filter`), in the order of the
  lanes, then each group (`▦ Pad`: what it puts out after its effects, its pan and its fade) and each effect of its bus (`  fx Reverb (bus)`). The tag in front of a row says whose it is: `L1` for the first lane, counted from 1 in the order of the
  session, `G1` for the first group; the keys on any row of a lane or a group are that lane's or group's.
- **Columns** (the inputs, `CableTo`): each effect of each lane's chain, each group's bus (`bus`) and each effect of it, and the mix (`Mix`), headed by the tag of what has it and the first four letters of its name; a lane has no input. An effect that has a second input (a Math: see "An effect with a second input") has a column of its own after its first, headed `↳B` under the tag of what has it (`L1 Math (B)`, and `G1 Math (bus, B)` for a bus effect, in the foot): a cable ends there, and no link does.
- **Flags**, on the first row of a lane or a group: its way out (`▪G1`: it goes to the bus of group 1, `▪Mix`: to the mix, `▫`: it is cut), and `M` and `S` while it is muted and soloed.
- **A crosspoint** is the level of the cable that is drawn there (`0.60`, yellow), a `●` (yellow) where a link of the audio path is (a lane's instrument to its first effect, an effect to the next, the end of a chain to its group's bus or to the mix, a group to its first bus effect: a link has no level),
  a dot where there is none and one can be drawn, or where a link is cut and can be given back, and a cross (dim red) where one cannot, because the audio would go round (a cable from an effect to one before it in the same chain, from a group's output to a lane in it): the wiring with a cable there would
  not compile (`SessionModel::cable_problem`, the rule that the controller and the audio thread refuse by; for a link that is cut, the link's own: `loop_if_route_is`, `loop_if_feed_is`). A cable that is drawn at the ends of a link as well (a file's, or an intent's at another level) is the pin there, and the link is not seen under it. The cell under the cursor is reversed.
- **The foot**, three lines under a rule: what the cursor joins (`L2 Oscillator (the instrument's output, after its volume) ▸ G1 Reverb (bus)`), what is at the crosspoint (the cable and its level, none, why none can be, or the link: `A link of the audio path (no level of its own): Enter cuts it`, `The link of the audio path is cut: Enter gives it back`) and the way out of the lane
  or group of the row, muted and soloed if it is, or what the last key was refused with (red, bold) until the next key. The terminal needs 12 rows of room inside the tab for it.
- When the session does not fit, the title says what is in view (`outputs 5–10 of 10 · inputs 4–8 of 8`) and the view follows the cursor.

| Key | Action |
|-----|--------|
| `Up` / `Down` / `Left` / `Right` | Move the cursor |
| `PageUp` / `PageDown` | A page of rows, less one |
| `Home` / `End` | The first / the last input |
| `Enter` / `Space` | Draw a cable at the crosspoint, at level 1 (`AddCable`), or take the one that is there out (`RemoveEdge`). Where the audio would go round nothing is asked, and the foot says why. At a link of the audio path that is there it cuts the link (`SetRoute none`, `SetFeed none`), and where it is cut it gives it back (an `AddCable` between the two ends, which the controller makes the intent that gives the link back: see "Links are cables") |
| `+` (`=`) / `-` (`_`) | Turn the level of the cable by 0.05 (`SetCableGain`: held to 0 … 2, and to hundredths, so that a run of steps does not drift; nothing is asked at the ends; a link has no level: the foot says so) |
| `e` | Type the level of the cable (the value popup; text that is not a number changes nothing; a link has no level) |
| `d` / `Delete` / `Backspace` | Take the cable out (cut the link, at a link that is there) |
| `w` | Cut / give back the way out of the lane or group of the row (`SetRoute`; the same as `w` on the Session tab) |
| `M` / `S` | Mute / solo the lane or group of the row (`SetMute`, `SetSolo`) |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo: the global keys; a cable drawn, turned or taken out is a step of the history as the page's is |

A click puts the cursor on the crosspoint it is on (or on the row, on its name), the wheel moves it three rows, and the items of the tab's action bar press their keys. Everything that is asked is an intent, carried out by the controller as a browser's
request is (`State::try_apply`: the view reacts as it does to any intent, and a refusal is said in the foot, where `State::apply`, which the other tabs use, only logs it: `giving that lane its way out would make the audio go round, by the cables there
are`), so that a cable drawn here is in the page's graph and the history at once, and the other way round. The tab reads the session from the model on every frame (`Bench::of`: the rows and the columns), so that what a browser does under `tang --web` is
on it with the next frame; the cursor goes with the output and the input it is on when rows and columns come and go (`Patch::settle`), and what a refusal is worked out of (a compile of the wiring for each crosspoint) is worked out for the crosspoints
in view only and kept until the wiring changes (`Patch::cell`). The tab bar has five tabs now, 58 columns: the tempo has room beside them from 66 columns, and the clip light as well from 72 (measured; below that they are drawn over the last tab).
The matrix, the cursor and the keys have 26 unit tests of their own, and the tab through the real loop 24 tests with four snapshots; a run of 121 mutants of the tab and its place in the program found ten tests that were missing (they are held now) and three clamps of the cursor keys that `settle` made redundant (taken out).
TODO: not here: a cable drawn with the mouse (a drag from an output to an input), a light at a pin for what the cable carries (the page has none either), rows dimmed for what is not heard (the page dims the nodes: `audibleAt`), the cables in the tree of the
Session tab, and the links of a lane's chain, which are the template's and not cables (nor the cut of one: the matrix does not show `feed`, which `f` on the Session tab and the page do).

### Help tab

Static text showing all keybindings and usage reference. Scrollable with
`Up`/`Down`. With `--web` it starts with the address of the web interface, to open
in a browser (with the token, if there is one), and a warning when it listens beyond
this machine.

### Dirty indicator

When the session has unsaved changes (parameter tweaks, plugin adds/removes,
preset loads, effect reordering, a finished pattern recording, a captured
MIDI-learn control), the Session tab label shows an asterisk:
`Session *`. The asterisk clears on save.

### Logging

All logging goes to stderr via `RawModeLogger`, same as in `play` mode. Use
`tang 2> debug.log` to capture logs to a file. No in-app log viewer.

While the TUI owns the screen it must not be corrupted by stray writes to the
terminal. Two layers guard this, active only when **stderr is a terminal**
(so `tang 2> debug.log` still captures everything):

- Rust `log` output is suppressed (`max_level = Off`) for the TUI lifetime,
  set before the background catalog scan spawns.
- The stderr **file descriptor** is redirected to `/dev/null` for the TUI
  lifetime (`tty::StderrSilencer`). Native plugin/library code — CLAP preset
  scanners, VST3 factories, ALSA/JACK probing — writes diagnostics straight to
  the fd, bypassing `log`; without this those lines land on the alternate
  screen. (`enumerate` uses the same guard.) Consequence: a panic while the TUI
  is up prints to `/dev/null`; re-run with `tang 2> debug.log` to capture it.
  Only stderr is redirected — stdout is the TUI's render surface, so a plugin
  that writes diagnostics to stdout (rare) is not covered.

## TUI framework

The TUI is built on ratatui 0.30 with the crossterm backend. We target the
latest ratatui release and adopt new APIs as they become available. When
upgrading ratatui, update this version note.

### TUI tests

`src/tui/golden_tests.rs` builds a `State` by hand around a `Controller` over
stub engine channels (`app::testutil::stub_engine`), drives it with real key
events through `process_event`, and checks two things: the rendered frame
(characters **and** styles, via `insta` and ratatui's `TestBackend`; snapshots
in `src/tui/snapshots/`) and the commands the TUI sent to the audio thread. After
an intentional UI change run `INSTA_UPDATE=always cargo test golden` and review
the snapshot diff. The Patch tab's tests are in the module `patch_tab` of that file, and its matrix, cursor and keys have unit tests of their own in `src/tui/patch_view.rs`.
