# Overview

This is Tang, a terminal-based audio plugin host.

- Terminal user interface (TUI)
- Supports LV2, CLAP, and VST3 plugins
- Supports MIDI keyboard
- Includes a Virtual Piano (tracker-style)

## Usage

```
tang                             # launch TUI with default session
tang session.toml                # launch TUI with specified session
tang play session.toml           # play session with virtual piano (no TUI)
tang --web session.toml          # TUI, and serve the session to a browser too (needs --features web)
tang serve session.toml          # serve the session to a browser (no TUI; needs --features web)
tang enumerate plugins           # list installed LV2, CLAP, and VST3 plugins
tang enumerate builtins          # list built-in plugins
tang enumerate midi              # list available MIDI input devices
tang enumerate audio             # list available audio output and input devices
tang describe <plugin>           # show plugin details
tang listen [input]              # listen to an audio input for a few seconds and say what it hears (plays nothing)
tang window <plugin>             # open a plugin's own window by itself, for a check (macOS, VST3; plays nothing)
```

Tang is also a **plugin for a DAW** (step 113): `cargo build --release -p tang-clap` makes the CLAP file that Bitwig loads, which has an instrument (Tang) and an effect (Tang FX), each with its session in the host's project and its page in a browser (`docs/reference/plugin-host.md`: how to build it and where to put it, on Windows too). The package is a library (`src/lib.rs`) and a thin binary (`src/main.rs`); `cargo test --lib` runs the unit tests.

## Flags

These flags apply to the TUI and to the `play` and `serve` subcommands, and
can be written before or after the subcommand (`tang play x.toml --audio-device
none`):

| Flag | Default | Effect |
|------|---------|--------|
| `--midi-device <name>` | all inputs | Open only the named MIDI input |
| `--audio-device <name>` | system default | Use the named audio output; `none` opens no device and renders the engine on a timer (for CI, headless runs and tests: the audio thread's work happens, nothing is heard) |
| `--jack` | off | Linux: play through PipeWire's JACK bridge instead of ALSA (see "Audio host selection") |
| `--buffer-size <frames>` | 1024 (Linux), 256 (macOS/Windows) | Audio buffer size in frames |
| `--sample-rate <hz>` | 48000 | Sample rate |
| `--debug` | off | Show a live incoming-MIDI monitor at the bottom of the TUI (diagnoses what a controller actually sends) |

`--debug` adds a strip at the bottom of the TUI tallying incoming MIDI by kind
(note / pitch bend / channel pressure / poly aftertouch / each CC number), each
row showing the latest raw bytes and a hit count. Useful for confirming what an
expressive controller emits — e.g. whether a hard key press sends Channel
Pressure (learnable) or is routed to a CC. Implemented as a lock-free-ish
`Arc<MidiMonitor>` (`src/midi_monitor.rs`) written by the MIDI input thread and
read by the render loop; a no-op when the flag is off.
TODO: `--debug` is a global flag, so `play` and `serve` take it, but only the TUI draws the tally: there the monitor is
written by the MIDI input thread and read by nothing, and the flag changes nothing that can be seen. `play` could print the
lines the monitor keeps, or the two could refuse the flag.

**`--web[=<addr>]`** (the TUI only; needs a build with `--features web`, without
which it is refused before the session is loaded) serves the session to browsers
while the TUI runs, so that what is changed in either shows in the other. The
address, an IP and a port, comes only after an `=` (`tang --web=0.0.0.0:9000`; so
`tang --web x.toml` is a session) and is `127.0.0.1:8765` without one;
`--web-origin <origin>` (repeatable, needs `--web`) lets another page connect, a
dev server's, say. As for `serve`, beyond loopback a token is made and put in the
address's fragment. The address (with the same warnings `serve` logs, for an address
beyond this machine) is printed on stdout before the TUI takes the screen, and shown
while it runs: in the tab bar (`web 127.0.0.1:8765 (2)`, where it listens and how
many browsers are on it, shortened or left out on a narrow terminal) and on the Help
tab. A port that is taken is an error at once, before the session is loaded and the
terminal is taken over (the address is tried, and let go again). `play` and
`serve` do not take it (`serve` has `--web-bind`). See "Web UI" and
`docs/web-ui.md` §3.4.

The `play` and `serve` subcommands take the session path as an optional
positional argument. When launched without a session path, the TUI (or the
subcommand) starts an in-memory default
session (`builtin:osc` as the instrument, no effects) targeting a fresh
timestamped file `~/.config/tang/sessions/session-<id>.toml`. Nothing is
written to disk until the first `Ctrl+S`.

## Subcommands

**enumerate** `<target>` — lists one category of available resources. Target is
one of `plugins`, `builtins`, `midi`, or `audio`.

**describe** `<plugin>` — prints plugin info (type, latency, the names of the two audio inputs of an effect that has two, parameters, each of which that has an effect only sometimes says when, presets) and exits.

**listen** `[input] [--seconds N]` — opens an audio input (the system's default, or the one that has `input` in its name: `tang enumerate audio` lists them), **plays nothing**, listens for N seconds (3) and says what it hears: the frames that arrive, the level every half second and the loudest sample. It is the one thing that tells a microphone that Tang does not hear from one that the system does not give it (`input.rs`, `probe`): an input that gives only exact zeros, which a microphone does not (a room has a noise floor), is said so with the likely cause. On macOS that is the permission, which belongs to the terminal that runs Tang, and a refused one is answered with silence and no error; the advice names the terminal (`__CFBundleIdentifier` and `TERM_PROGRAM`, which the programs that a terminal starts inherit) and the command that asks again (`tccutil reset Microphone <id>`). Status 1 for an input that cannot be opened and for one that gives silence. `cargo test --lib the_real_input -- --ignored --nocapture` is the same from the test program (an ignored test, for a person who has a microphone).

**window** `<plugin> [--seconds N] [--snapshot FILE] [--no-focus] [--tree]` (step 112) — loads the plugin as `describe` does, **plays nothing**, opens the plugin's own window (the face that it draws for itself: a VST3 plugin's editor, on macOS only so far) and keeps it open for N seconds, or until the person closes it. `--snapshot` writes what is in the window as a PNG a moment before it closes (a plugin that draws with the graphics card gives a blank picture), `--tree` prints the views that the plugin made in it (for a window that stays empty), `--no-focus` leaves the keyboard with the terminal. Status 1, with the reason, for a plugin that has no window, a machine that has no screen (a login over ssh, CI) and a window that did not attach. The window opens on the machine that runs Tang. In a session the same window is opened from the TUI's `u` (Session tab, on an instrument or an effect) and from the page's "UI" button and "Open its own window" menu item (the intent `OpenEditor`: no step of the history; a knob turned in the window is an ordinary `SetParam`, and the window follows what the session sets). See "A plugin's own window" in `docs/reference/plugins.md`; `cargo test --test cli -- --ignored` opens the first installed VST3 plugin for a second (for a person at a Mac), and `cargo test --features web --test serve -- --ignored` opens one through the server.

**play** `<session.toml>` — loads a session and plays it in the terminal.
No editing, no TUI. Accepts input from any connected MIDI device, and the
computer keyboard doubles as a virtual piano (Amiga tracker layout).
Logs MIDI events and audio info to the screen.

**serve** `[session.toml] [--web-bind <ip:port>] [--web-origin <origin>]...` —
loads a session like every mode and serves it to browsers, with no terminal
interface (needs a build with `--features web`; without it the subcommand says
so). The first line on stdout is the address to open (`Web interface:
http://127.0.0.1:8765/`), the log is plain text on stderr, and Ctrl+C / SIGTERM /
SIGHUP closes the browsers and stops the program. `--web-bind` (default
`127.0.0.1:8765`, an IP and a port) beyond loopback makes a token for the run and
puts it in the address's fragment (`http://host:port/#token=…`); `--web-origin`
(repeatable) lets another page connect, a dev server's, say; an address that is
taken, or beyond loopback without a token that can be made, is an error before the
session is loaded. Works with `--audio-device none`, which is how CI and the
end-to-end tests run it. Without a
MIDI subsystem (a container) it warns and carries on. The page at `/` is the web
interface (`web/`, read from the disk in a debug build and embedded in a release
build; a placeholder if it was not built).

## Config file

**Path:** `~/.config/tang/config.toml`

Optional application-level config. Currently supports custom plugin search paths.
Missing file or missing keys = no extra paths.

```toml
[plugin_paths]
clap = ["/home/user/my-plugins/clap"]
vst3 = ["/home/user/my-plugins/vst3", "/mnt/external/vst3"]
lv2 = ["/home/user/my-plugins/lv2"]
```

All fields optional (default: empty). Extra paths are appended after platform
defaults. Loaded once at startup via a `OnceLock<Config>` global in `src/config.rs`.

For LV2, extra paths are injected into the `LV2_PATH` environment variable before
`livi::World::new()` is called. For CLAP and VST3, the respective plugin modules
read the extra paths directly.

The library of modules (step 104) is not part of this file: it is a directory, `~/.config/tang/modules/` (`TANG_MODULES_DIR` says another), with a file `<name>.toml` for each module that was
saved (see "Modules: effects and instruments, with the cables between them, saved to a file").

## Session config

Sessions are TOML files. Without a session argument the TUI creates a new
in-memory session saved to `~/.config/tang/sessions/session-<id>.toml` on
first save. Sessions are saved explicitly only (no auto-save on parameter
change). The session's file is known by its absolute path from the start
(`session::absolute_path`): some plugins change the working directory of the
whole process while they are scanned or loaded (a ROLI one does), and a path
relative to it would then lead somewhere else.
A plugin can be specified using:
- `./path/to/Plugin.lv2`: LV2 bundle by path
- `./path/to/Plugin.clap`: CLAP bundle by path
- `./path/to/Plugin.vst3`: VST3 bundle by path
- `lv2:<uri>`: LV2 lookup by URI (lv2: prefix OPTIONAL)
- `clap:<id>`: CLAP lookup by plugin ID (clap: prefix OPTIONAL)
- `vst3:<name>`: VST3 lookup by name (case-insensitive)
- `builtin:<name>`: built-in plugin (e.g. `builtin:osc`)

### Instrument format

Sessions are a flat list of instruments. Each instrument has a plugin, an
optional key range, and its own effect chain.

```toml
[[instrument]]
plugin = "lv2:http://tytel.org/helm"
range = "C0-B3"
preset = "Pad"
volume = 0.8

[instrument.params]
"reverb_on" = 1.0

[[instrument.effect]]
plugin = "./reverb.lv2"
mix = 0.5

[[instrument]]
plugin = "builtin:osc"
range = "C4-C8"
```

Simple case (one instrument, full range, no effects):
```toml
[[instrument]]
plugin = "builtin:osc"
```

- `[[instrument]]` — one or more instruments
  - `plugin` — plugin path or URI (required)
  - `range` — note range like `"C0-B3"` (optional, omit for full range).
    Either bound may be omitted for an open-ended range: `"C4-"` means C4 and
    up, `"-B3"` means everything up to and including B3.
  - `state` — the plugin's own state, base64 (optional, written by Tang: see "A plugin's own window", part C)
  - `preset` — preset name to load (optional)
  - `volume` — host-side output gain, applied before effects (default: 1.0,
    uncapped in the TOML; the in-TUI editor (`v`) caps at 4.0)
  - `transpose` — shift incoming notes by this many semitones (default: 0,
    config-file only — shown in the chain tree but not editable in the TUI)
  - `params` — parameter overrides applied after preset (optional)
  - `group` — submix group membership: index into the top-level `[[group]]`
    list (optional, omit for ungrouped)
  - `mute`, `solo` — muted / soloed (default: false, written only when true; see
    "Mute and solo")
  - `route` — where the lane's end goes: `"out"` (the default: to the bus of its group, or to the
    mix if it has none) or `"none"` (nowhere but its audio cables; written only then; see "The way out")
  - `[[instrument.effect]]` — zero or more effects in series
  - `[[instrument.modulator]]` — zero or more modulators (LFO or ADSR envelope): **the older form, which is read and not written** (since step 96 a lane has no modulators of its own: a file's are moved into
    the session's own `[[modulator]]` list when it is loaded, each hearing the lane, and the file that is saved says so; see "Free modulators", "Every modulator is global")

### Plugin slot fields

Each plugin slot has:
- `plugin` — plugin path or URI (required)
- `state` — the plugin's own state as base64 text (optional; written by Tang, never by hand): what a person picked in the plugin's own window that is not a parameter, which the plugin gives back when it is loaded. Only a VST3 plugin has one so far. Read leniently: a `state` that is not base64, or is more than 32 MiB, is left out with a warning, and the plugin starts as its preset and parameters say. It is read from the plugin when its window is closed and when the session is saved (see "A plugin's own window" in `docs/reference/plugins.md`)
- `preset` — preset name to load (optional)
- `params` — parameter overrides applied after preset (optional)

The instrument additionally has:
- `volume` — host-side output gain, applied before effects (default: 1.0, uncapped in the TOML; a modulator can drive it, `gain = "volume"`: see "The volume, the mix and the pan that a modulator drives")
- `transpose` — note shift in semitones (default: 0)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0; a modulator can drive it, `gain = "mix"`)
- `feed` — `"chain"` (default) or `"none"`: whether the stage before the effect feeds it (written only when none; see "The input of an effect")

Load order per plugin: load → state → preset → params.

A number in a session file must be finite, and small enough for the 32-bit floats
the model and the audio thread hold: TOML has `nan` and `inf`, and 64-bit floats, so
`1e39` is finite to TOML and infinite once narrowed. A session that has one is refused
when it is loaded, with the place in the error
(`instrument[0].volume is not a finite number (NaN)`,
`instrument[0].volume is too big for the 32-bit numbers the audio engine holds (1e39)`).
An LFO's `rate` below 0 is refused in the same way, wherever the LFO is (a lane's, a group's, a free one's, an older
session's modulator nested in an effect): `instrument[0].modulator[0].rate is below 0 (-1): an LFO cannot go
round backwards` (`modulator[0].rate …` for a free one) (`session::reject_backwards_lfos`; the phase of such an LFO counts down from 0 and its
shapes leave the -1 to 1 that a cable's law assumes: a saw from -3 to -1, f147). Zero is an LFO that
stands still, a rate above the knob's 50 Hz is the file's to say, a step sequencer's rate is refused in the same way
(`a step sequencer cannot go round backwards`), and a modulator that is neither (an envelope, a MIDI control, a macro)
has no rate that is read. The steps of a step sequencer are refused when the list is empty or has more than 16
(`instrument[0].modulator[0].steps has 17 values: a step sequencer has 16 steps at most`, `session::reject_bad_steps`).

## Signal chain

```
MIDI sources (hardware keyboards + virtual piano)
  → Modulators apply (set_parameter on the plugins of any lane or bus that their cables drive)
  → For each instrument (filtered by note range):
      → Instrument → volume gain → N Effects (in series, each with dry/wet mix)
      → lane meter (peak, clip count)
  → Each instrument routes to either:
      → its submix GROUP's bus  → group volume → N group effects
        → group meter, OR
      → the master mix directly
  → Sum all groups + ungrouped instruments
  → master volume → limiter (look-ahead peak, in the sound by default: see "The master limiter and volume")
  → master meter (peak, clip count, how far the limiter works) → Audio output
```

Each instrument has its own effect chain. MIDI note events are filtered by the
instrument's key range; channel-1 (global) CC/pitch-bend/pressure is duplicated
to all instruments, while member-channel (2–16) per-note MPE expression is
routed only to the lane sounding a note on that channel (see "MIDI routing").

**Groups (submix buses).** An instrument can belong to a group. All members of
a group are summed, then run through the group's own end-of-chain effect chain
and output volume, before being added to the master. Ungrouped instruments go
straight to the master. Groups are one level deep (a group holds instruments,
not other groups). In the audio graph the instrument lanes stay a flat list,
addressed by `inst` index as usual; group membership is an overlay
(`InstrumentLane.group: Option<usize>`) and groups own their bus
effects and volume — so all the per-instrument commands
are unchanged.

**Modulators** (all of them the session's own since step 96, and each hearing one lane's or one group's notes or every note) are applied at the top of `process()`, before any lane renders: each ticks (an envelope that hears a group is triggered by the union
of its members' notes), cross-mod resolves, and the resulting parameter writes, **added** where several cables drive one parameter, are
applied to the plugins of the lanes and the buses that the cables name by node. Lanes then read those values when they render; bus effects read them when the
group's chain runs. No modulator wins over another: they add up (until step 96 a lane's own modulator, applied later inside the
lane, won over a group's: see "Free modulators", "The racks go (step 96)").

**Reordering instruments** (`Shift+Up` / `Shift+Down` on an instrument, `Controller::
swap_lanes`) moves the whole lane: its plugins and effects, its volume,
range, transpose and group, and its pattern, in the model and on the audio thread
(`SwapInstruments` swaps the lanes, and the pattern player is told which lane it is now).
A modulator's cables name the plugins by their nodes, and keep driving the same plugins
wherever the lanes are (the audio thread finds the plugin by its node: `locate_anywhere`), and a modulator that hears a lane
names it by its node as well. (Until the review of step 14 found it, `SwapInstruments` swapped only the
instrument plugin while the model swapped the whole node, so that after any reorder, grouped
or not, a lane's effects, modulators and volume were on different lanes in the engine and in
the view.) Taking the instrument plugin out of a lane (`d` on it, the intent `ClearInstrument`: `Controller::clear_instrument`)
leaves the lane what else it has: its effects (which go on running) and volume, and the modulators that hear it go on hearing it. Putting an instrument in a lane
(`i`, `Controller::put_instrument`: from the catalog only, one with an audio output, like an effect)
replaces the one it has or fills an empty lane, under the lane's own node. Either way the cables that
drove the old plugin's parameters are taken out first (`take_out`, with what
drives their depths): a cable keeps its parameter's *index*, so left in place it would drive whatever the
new plugin has there. A note remap goes with the plugin too, since the audio thread drops the remapper
with it (`InstrumentNode::forget_remap`).

Effects can be reordered within an instrument (or a group bus) in the Session
tab (`Shift+Up` / `Shift+Down`; `Controller::move_effect` / `move_bus_effect`, which the page's
buttons reach by the intent `MoveEffect`). Instrument volume is applied after the instrument's output and before its
first effect; group volume is applied to the member sum before the group's
first bus effect.

## Reference files

This file is the front door: what every session needs (usage, flags, subcommands, the session file's basics, the signal chain, the working notes). The rest of what this project does, and how, is in `docs/reference/`, one file for each topic, with the sections that were here before the split (2026-11-04) under their own titles: the other documents and the comments in the code say "see CLAUDE.md, ‘Free modulators’" and the index finds the file. Read the file of the area you are changing before you change it, and find a section with `grep -n '^#' docs/reference/FILE`. A step's section goes in the file of its topic, and a new file is put in this index.

| File | What is in it | Sections |
|------|---------------|----------|
| `docs/reference/session-file.md` | the file format beyond the basics (groups, mute and solo, the way out of a lane, the input of an effect, an effect that stands by itself, places and pins), tempo, patterns, note remapping and the example sessions | Group format; Group modulators; Mute and solo; The way out; The input of an effect; An effect that stands by itself; Node positions; Pinned parameters; Tempo; Pattern recorder/player; Example sessions; Note remapping; Config syntax; How it works; Limits |
| `docs/reference/modules.md` | a chain, or any plugins with the cables between them, saved to a file and added again; a module that is added as one box; collapsing what is chosen into one box; the knobs that a box shows for the macros in it, and the port of a knob that cables are drawn from; the bar's `+ Module` | Modules: effects and instruments, with the cables between them, saved to a file; A module that is added is one box; Collapsing what is chosen into one module where it is; A macro that has a name and a knob that a box shows; Modules have a category of their own, and the knob of a macro on a box has a port |
| `docs/reference/cables.md` | audio cables, links that are cables, the second input of an effect | Audio cables; Links are cables; An effect with a second input |
| `docs/reference/modulators.md` | the sources and their laws, cables by node and port, what a modulator drives besides plugin parameters | Modulator fields; Cables by node and port; The volume, the mix and the pan that a modulator drives |
| `docs/reference/free-modulators.md` | the modulators of the session's own: their reach, their ear, the file, the terminal, the page, the racks that went | Free modulators |
| `docs/reference/inputs-and-midi.md` | sources of notes and cables of notes, MIDI routing, the virtual piano, the audio input (the microphone), the audio host | Sources of notes and cables of notes; The audio input; MIDI routing; Virtual piano; Audio host selection |
| `docs/reference/engine.md` | the threads and the exit, the master limiter and volume, latency, the kernel of the patch graph (the signal chain is in `CLAUDE.md`) | The master limiter and volume; Latency; Architecture; The kernel |
| `docs/reference/oracle.md` | what renders are held to, the corpus, the edits, the benchmark | The oracle |
| `docs/reference/plugin-host.md` | Tang as a CLAP plugin in a DAW (Bitwig, step 113): the two plugins in `Tang.clap`, how to build and install them (Windows first), the engine driven by the host's blocks (`tang::embed`), the session as the host's state, the page as the plugin's window, what is not done | The two plugins in `Tang.clap`; How it is built; Building it and putting it where a DAW looks; Using it in Bitwig; What it does not do yet; Held by tests; Found on the way; Decisions |
| `docs/reference/plugins.md` | plugin formats, a plugin's own window (the editor of a VST3 plugin in a window of the program's own, and `tang window`), the built-in plugins, how plugin I/O is handled, a parameter that has an effect only sometimes | A parameter that has an effect only sometimes; A plugin's own window; Plugin compatibility; Built-in plugins; Plugin I/O handling |
| `docs/reference/terminal-ui.md` | the tabs, the Session tab and its popups, the Piano, Scope and Patch tabs, logging, the TUI's tests | TUI; Splash screen; Tabs; Global keybindings; Session tab; Chain; Parameters; Plugin selector popup; Preset selector popup; Value entry popup; Range entry popup; Rename group popup; Save As popup; Modulation target selector popup; Modulate popup; Piano tab; Scope tab; Patch tab; Help tab; Dirty indicator; Logging; TUI framework; TUI tests |
| `docs/reference/controller.md` | intents, events, the history (undo and redo), snapshots, telemetry | Session state |
| `docs/reference/web-page.md` | the browser front-end: the protocol, the server, every step of the page | Web UI |
| `docs/reference/canvas.md` | arranging the nodes, right-click menus, choosing with Shift and moving what is chosen | Arranging the nodes; Right-click menus, choosing with Shift and moving what is chosen |
| `docs/reference/platform-and-future.md` | build requirements, CI, and the ideas that are not built | Platform; Future ideas |

## Notes for Claude

- This file and the files of `docs/reference/` serve as a design document for us. This file is read in full at the start of every session, so it is kept short: detail goes in the reference file of its topic (see the index above).
- We use TODO:s (on new lines) to clarify what is yet to be implemented; they are in the reference files (`git grep -n '^TODO' docs/reference`).
- Notify me when you find refactorings we should do before implementing new things.
- Notify me when you find discrepancies in this document vs how it actually works.
- Commit hashes cited in this file and in `docs/` are from the step-by-step history of this work, which went into `main` as one squashed commit: it is kept as pull request #10 (`git fetch origin refs/pull/10/head:web-ui-history`).
