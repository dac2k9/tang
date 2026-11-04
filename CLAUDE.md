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
```

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

**listen** `[input] [--seconds N]` — opens an audio input (the system's default, or the one that has `input` in its name: `tang enumerate audio` lists them), **plays nothing**, listens for N seconds (3) and says what it hears: the frames that arrive, the level every half second and the loudest sample. It is the one thing that tells a microphone that Tang does not hear from one that the system does not give it (`input.rs`, `probe`): an input that gives only exact zeros, which a microphone does not (a room has a noise floor), is said so with the likely cause. On macOS that is the permission, which belongs to the terminal that runs Tang, and a refused one is answered with silence and no error; the advice names the terminal (`__CFBundleIdentifier` and `TERM_PROGRAM`, which the programs that a terminal starts inherit) and the command that asks again (`tccutil reset Microphone <id>`). Status 1 for an input that cannot be opened and for one that gives silence. `cargo test --bin tang the_real_input -- --ignored --nocapture` is the same from the test program (an ignored test, for a person who has a microphone).

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

### Group format

Optional submix groups. A group sums its member instruments (those whose
`group` index points at it), runs its own end-of-chain effect chain and output
volume on that sum, then adds the result to the master mix.

Since step 103 the **page** has no way to make a group or to put a lane in a new one: no "+ Group" in the bar, no "new group" in a lane's choice of group, and a lane has the choice only while
the session has a group (the user's "I think we should remove the groups (at least wihin the UI)", and to the proposal of hiding them, "2. yes hide groups"). The page still draws a group that a
session has, so that nothing sounds that cannot be seen, and the terminal's `g` still makes one; the file, the model, the controller and the engine are as they were, and the groups leave them with P5c
(a loaded file's group becomes a lane that holds its bus effects, with a module for its volume and pan). `examples/shared-reverb.toml` has its reverb in a lane of its own now, not on the bus of a group.

```toml
[[group]]
name = "Pad"            # optional display name (default: "Group N")
volume = 0.8            # group bus output gain (default: 1.0)
pan = -0.3             # group bus stereo balance (default: 0.0 = center)

[[group.effect]]        # zero or more bus effects, in series
plugin = "builtin:reverb"
mix = 1.0

[[instrument]]
plugin = "builtin:osc"
group = 0               # this instrument feeds group 0

[[instrument]]
plugin = "builtin:osc"
group = 0               # ...and so does this one
```

- `name` — display name (optional; editable in the TUI with `R` on the group
  header, default "Group N"; an empty or blank name is no name)
- `volume` — group bus output gain, applied to the member sum before the group
  effects (default: 1.0; a modulator can drive it: see "The volume, the mix and the pan that a modulator drives")
- `pan` — group bus stereo balance, −1 left … 0 center … +1 right (default:
  0.0; balance-style, applied to the bus output after the effect chain). In
  the TUI a group's parameter pane shows Volume + Pan as editable rows
  (`Enter` to focus, `Left`/`Right`/`Enter` to edit); `v` still edits volume.
- `mute`, `solo` — muted / soloed (default: false, written only when true; see
  "Mute and solo")
- `route` — where the group's end goes: `"out"` (the default: to the mix) or `"none"` (nowhere but
  its audio cables; written only then; see "The way out")
- `[[group.effect]]` — zero or more bus effects in series (each with `mix`)
- `[[group.modulator]]` — zero or more group-scoped modulators (**the older form, read and not written**: moved into the session's own `[[modulator]]` list when the file is loaded, each hearing the group; see below)
- Membership is set on the instruments via their `group` index, not listed on
  the group. Groups are one level deep (no nested groups).

#### Group modulators (the older file form)

(Since step 94 a session that is loaded has its groups' modulators moved into the session's own rack, each hearing the group, and since step 96 there is no other rack: the engine, the model, the controller, the
terminal and the page have only modulators of the session's own, which hear one lane's or one group's notes (their ear) or every note and drive any plugin of the session: see "Free modulators",
"Every modulator is global". What is described here and under "Modulator fields" is **what an older file says**, which is read for ever: the loader makes what it says into modulators of the session's own and cables
by node, and a save writes the new form, `[[modulator]]` with `hears`. The sources and the laws below are what every modulator does.)

A group could own modulators that drove parameters anywhere in the group's
world: any **member instrument's** chain, the group's own **bus effects**,
**sibling group modulators**, or a **member instrument's modulators** (a group
LFO could, e.g., sweep a member synth's own LFO rate). They were the group-scoped
analogue of the lane-scoped instrument modulators (same LFO/envelope sources
and cross-mod; see "Modulator fields" below) — they just addressed a wider
target set. A modulator of the session's own that hears the group and has the
cables that such a file says is the same thing, and its reach is wider still.

```toml
[[group.modulator]]
type = "lfo"            # or "envelope" — same fields as instrument modulators
rate = 0.5

# Target a member instrument's parameter. `member` is the member's ordinal
# within the group (0 = the first member in instrument-list order); `slot` is
# 0 = that instrument (default), 1..N = its effects.
[[group.modulator.target]]
member = 0
slot = 0
param = "cutoff"
depth = 0.5

# Target one of the group's own bus effects (`bus` = bus-effect index).
[[group.modulator.target]]
bus = 0
param = "mix"
depth = 0.25

# Cross-mod a sibling group modulator (mod_rate / mod_depth / mod_attack /
# mod_decay / mod_sustain / mod_release — same as lane cross-mod).
[[group.modulator.target]]
mod_rate = 1
depth = 0.3

# Cross-mod a MEMBER instrument's modulator: `member` + a `mod_*` field, where
# the mod_* value is the member's lane-modulator index. Here: member 0's
# modulator 2's LFO rate.
[[group.modulator.target]]
member = 0
mod_rate = 2
depth = 0.3
```

- `member` + `param` (+ optional `slot`) → a member instrument's parameter (moved: `to = "instrument[N]"` or `"instrument[N].effect[M]"`).
- `bus` + `param` → a group bus-effect parameter (moved: `to = "group[G].effect[B]"`).
- `member` + `mod_*` → a member instrument's modulator field (the `mod_*` value
  is that member's lane-modulator index; moved: the place that modulator has in the session's own list).
  It takes effect **a block later** than it did while a group had a rack of its own (D2 of step 94: the cables between the session's own modulators are resolved after all of them have ticked).
- `mod_*` alone (no `member`) → a sibling group modulator.
- A cable keeps the plugin, the modulator or the cable it drives, and not the ordinal
  that the file names the member by: the loader turns each ordinal into the node it names
  and the model says the place again from the structure there is (`SessionModel::kind_of`: see "Cables by
  node and port"). The file stores ordinals, not global lane indices, so it stays
  readable. A lane that leaves its group (or goes to another) keeps the cables of the modulators that heard the
  group: a cable names its plugin, which has not moved, and a modulator of the session's own drives it wherever
  the lane is (`Controller::set_lane_group`, a step of the history since step 96: "take a lane out of its group",
  "put a lane in a group"; D3 of step 94, and the engine's cables find the plugin by its node, so that nothing is told).
- **Envelope trigger:** an envelope that hears a group is triggered by the union
  of its members' note ranges — it opens whenever any member would sound a
  note (`build_group_midi`).
- **Application order** (step 96: there is one rack): every modulator ticks first, before any lane renders (the cables between them are resolved after all of them have ticked, and their outputs published), and then each
  parameter that a cable drives is set, before the lane that has the plugin renders, to the value the user set plus **the sum** of the offsets that the cables on it make, held to the parameter's range.
  **No modulator wins over another**: two on one parameter add up, a modulator that hears a lane and one that hears a group alike, and a control that is not bound yet adds nothing (it used to write the user's value over a group's:
  while there were racks the nearer one wrote after the others, and a lane's won over a group's, which won over a free one's; D1 of step 94). The page's ring and the mock engine's value of a parameter follow it
  (`modulationsOn`: every driver adds up; pinned in `plugin/chain.rs` by `two_free_modulators_on_one_parameter_add_up` and `what_free_modulators_add_up_to_is_held_to_the_range_of_the_parameter`).

### Mute and solo

A lane and a group can be muted (`mute = true`) or soloed (`solo = true`), keys of its table
beside `volume`, written only when on. In the model they are `InstrumentNode::muted` / `solo` and
`GroupNode::muted` / `solo`; the audio thread has them from `GraphCommand::SetMute`, `SetSolo`,
`SetGroupMute` and `SetGroupSolo`, which the loader sends for what a file turns on.

- A **muted** lane or group is never heard. A mute wins over a solo: a lane that is muted and
  soloed is silent, and (since it is soloed) it silences what is not soloed all the same. Its
  notes, modulators and pattern go on, and a muted group's members and bus effects go on, so
  what is heard when it is brought back is where it would be (a note that is held through the
  mute is sounding, a reverb's tail is where it would be).
- While **anything is soloed** (a lane or a group) only what is soloed is heard: a soloed lane,
  a lane in a soloed group, and a group that is soloed or has a soloed lane in it (its bus is
  heard for that lane, with its volume and effects, and its other members are not). A lane's
  mute is its own: a lane in a muted group is not itself muted (its meter still reads); the
  bus is cut.
- A lane or a bus is let go of, or brought back, over one buffer by a linear ramp of its gain
  (`fade` in `plugin/chain.rs`: 5.3 ms at 256 frames), so that it does not click. A gain that is
  1 and stays so costs nothing, and one that is 0 and stays so puts zeros and does not multiply by
  them, so that a plugin that puts out `NaN` is silenced by a mute. The lane's meter and the group's
  are after it (a muted lane stops clipping); the scope taps of a node are before it (they show the
  signal of the node, not what is heard). The lane's mute and solo go where the lane goes (a swap
  of two lanes, undo of a removal: the lane is handed back whole).
- **What cables carry** (P3c-2b of `docs/patch-graph.md` §11.6, step 78; `Schedule::set_gates` in `plugin/chain/nodes.rs`): the audio that an audio cable carries (see "Audio cables") is heard where it
  comes from something that is heard, so that soloing the lane that sends to a reverb on another lane lets the reverb be heard, with what the soloed lane sent. A lane or a group is *let in by the solo*
  as above (nothing is soloed, or it is, or its group is; a group: or a lane in it is), and is also heard **for what is sent to it** when it is not muted and a cable (not told to go, at a level above 0)
  ends at an effect of its chain, at an effect of its bus or at the bus of a group, from something that is heard: the instrument of a lane that is let in (and not muted), an effect of a lane or a
  group that is heard either way, or the output of a group that is. A group is heard as well for a lane in it that is heard for what is sent to it and whose way out (`route`) is not cut. What the chain
  of a lane and the bus of a group are heard at (`out`) is that, or the solo, and a mute wins over both. A cable carries what its start puts out at the level it was set: from an instrument
  by what the lane is heard for by itself, from an effect by what its lane or group is heard at either way, and from a group not gated again (its own fade has done it).
  **The head gate:** a lane that cables end in puts none of its own sound into its chain while the solo does not let it in (`head_goal`, applied in `run_head` after the scope tap), so that what it puts
  out is what it was sent. It is one ramp over the buffer, as the other gates are, except that it is at once for a lane that was not heard when the last buffer ended and stays where it was for one that is not to
  be heard when this one ends: a gate that goes down while the lane's tail comes up (a cable drawn into a lane that is not heard), or up while the tail goes down (a cable taken out of one that is let go
  of), would let through the product of the two ramps, a bump of the lane's own sound that the solo does not let in. A mute is not a head gate (a lane that is muted and let in goes on, and its tail cuts it). A session that has no solo,
  or no cable that ends in a lane, is what it was, bit for bit. What is heard is found by going through the cables again until nothing changes (they go one way, so at most as many times as there are
  cables, and nothing is allocated: `a_session_with_a_solo_and_cables_asks_the_allocator_for_nothing_once_it_is_running`), the least set that the law allows: a lane that only a cable from its own effect would
  let in is not let in. The page has the same law (`web/src/client/audible.ts`: `heardOf`, `audibleOf`, `ownAudibleOf` and `fedOf`; a lane that is heard only for what it is sent has its instrument's node
  dimmed and its effects' not), pinned to the engine by `fixtures/audible.json` (no cables) and `fixtures/audibleCables.json` (two groups and four lanes with five cables of every kind of start and
  end, every combination of their mutes and solos: 4,096 cases, with the engine's sound of each lane's own notes, of its chain and of each bus read). What the effects of a lane hold of its own sound (a reverb's tail,
  a delay's repeats) is heard when the lane is let in for a send, as it is when a mute is let go: the head gate shuts what comes in, and `Plugin` has no reset.
- The terminal (`M` and `S`, see the key table, and the Patch tab's) and the page (the "Mute / Solo" cell of a lane's
  and a group's node, which draws what cannot be heard dimmed) set them through the intents
  `SetMute` and `SetSolo`, which are steps of the history.

TODO: a click that solos one lane and takes the others' solo off (the Shift or Alt click of a
console), and a button that takes every solo off: the buttons turn each one by itself, as the keys do.
TODO: the law goes one way, from what is soloed to what it sends to: a lane or a group that is soloed and has nothing that is heard sending to it (the room of `examples/shared-reverb.toml`, soloed alone) is silent, where a console lets
the sends of what is not soloed feed a soloed return; a flag on a lane that lets its sends through, or a solo that lets in what feeds the soloed node for its sends only, would do it.
TODO: a muted lane (or one that is not heard for a solo) still renders its plugins every buffer, for
what a held note and a tail are when it is brought back; one that has been silent for a while could
stop, at the cost of that. (Measured by the oracle's benchmark: a session of 8 lanes with every lane muted and a chord held takes
801 µs a block of 256 frames, as it does with them heard, and one that sounds nothing takes 363 µs.)

### The way out (`route`)

A lane's end goes to the bus of its group or, with no group, to the mix, and a group's end to the mix: the way that a lane and a group are wired by themselves. `route = "none"` on the table of a lane or
a group cuts it (P3c-2a of `docs/patch-graph.md` §11.6, step 77), so that the lane is heard only through the audio cables that start at it (`[[cable]]`: its instrument's, after its volume and before its chain,
or those of its effects) and a group only through its own: an oscillator that is sent to a reverb can be heard through the reverb and not beside it. `"out"` is what it is without, and is not written; a word that
is neither refuses the session, with its place (`instrument[1].route is "None": a way out is "out" or "none"`), since a lane that goes nowhere is silent and a word that is nearly `none` is not to be read as
the way a lane goes by itself.

- In the model it is `InstrumentNode::route` and `GroupNode::route` (`session::Route`: `Out` and `None`), in the engine `InstrumentLane::route` and `Group::route` from `GraphCommand::SetLaneRoute` and
  `SetGroupRoute` (the loader sends what a file cuts, before the cables, so that a cable that only a cut way out keeps from closing a loop is drawn), and in the plan it is the link that is not there:
  `build_patch` makes no cable from the tail of a lane or a group that has its way out cut (`LaneWiring::route` and `GroupWiring::route` carry it to the model's check for a loop, and the plan is made
  again when it changes: `Shape` has a mark for it, and only for a way out that is cut, so that a session that cuts none has the shape it had). The tail runs all the same (its click, its fade, its meter, a learned control),
  so a lane that goes nowhere still has its meter, and its mute and solo take its cables with it as they do (see "Mute and solo": a lane that is not let in by the solo sends nothing by the cable of its instrument).
- The intent is `SetRoute { node, route }`: a lane (its instrument's node, which an empty lane has too) or a group; refused, with nothing changed, for any other node (`only a lane or a group has a way out`; not
  found for the master, as for every intent) and for a way out that is **given back** where the audio would go round with the cables there are (`SessionModel::loop_if_route_is`, the controller's and the
  engine's `route_problem`, which logs it and keeps the way out as it was): a cable from a group's output to an effect of a lane that is in the group is a loop only through the lane's link into the group, so that
  with the lane's way out cut it is drawn, and the way out is then not given back; cutting one cannot close a loop. It is told as a snapshot (the links of the graph change: the graph has no link out of the chain of
  a lane or a group that is cut, and its node says `route`: `NodeKind::Instrument` and `Group`), and it is a step of the history ("cut the way out of the lane of Oscillator", "give Pad its way out"; none for
  a way out that is as it is asked to be, and what could be redone is still to redo), which a lane or a group that is taken out and put back by undo has whole.
- The wiring is changed **at once**, as a lane's group is, and not over a buffer, as a mute and a cable are: a note that sounds when its lane is cut stops with a click (a mute is the way to silence a lane without one).
- The terminal has `w` (see the key table) and says `[no out]` at the end of the line of a lane or a group that is cut.
- **The page** draws the link as a cable (step 88b; see "Links are cables"). The end of a lane's chain, or of a group's, into the group's bus or the master, which is the link that a lane has by itself, is an amber cable (`AudioEdge` in `app/patch/edges.tsx`:
  the line and the colour of a send at level 1, `.tang-cable--audio`, 3 pixels in `--tang-send`, with **no slider**, since a link has no level) with a **×** at its middle named `Remove cable Filter → Master`, and `Remove cable Oscillator 2 → Master` for a lane that has no effect,
  the words of every cable. A press asks `Editor.setRoute(owner, "none")` for the lane or group whose way out it is and puts the focus on the menu that sends the audio of the node the link started at (`SendMenu`, `Send audio from Filter`), which offers the end again:
  choosing it, or dragging a cable between the two ends, is a cable between the ends of a cut link, which gives the way out back (`link_or_cable`); Undo does it as well. **The "Mute / Solo" cell has no button for the way out any more** (`WayOutButton`, `wayOutOf` and `.tang-way-out` are gone:
  where a node's audio goes is its cables, and a node that has none sends nowhere); the cell is the mute and the solo. `toFlow` says whose the link is (`ownerOfWayOut`: the lane or group that a link into a group or the master starts at, or that the effect it starts at is in;
  `wayOut` and `label` in the link's `data`, and nothing in the data of a link that is not one), and the canvas draws no link from the end of a chain whose way out is cut (the graph has none), so that a cut is seen as the cable going. A link inside a chain (an instrument to its first effect, an effect
  to the next) is the input of the effect it goes into and has a × of its own, which cuts that (step 86: see "The input of an effect"). The mock engine does what the controller does (`setRouteOutcome`, the cut links and the loops that `routeLoops` in `client/wiring.ts` looks for, the history's words),
  pinned to it by the `routes` list of the fixtures' shapes (`shape_of`, `shapeOf`) and by the script of `history.json` that cuts and gives back ways out and takes a lane that is cut out and puts it back.
TODO: a way out that is cut over a buffer, as a cable is (the link would stay in the plan for the buffer that it goes down in, and the check for a loop would have to look at the wiring that is to be); and the
action bar of the terminal has no `w` (its hints are in about a hundred snapshots; the Help tab lists it).
TODO: a way out that is cut has nothing on the canvas to press to give it back (the menu that sends from the node, where the focus is after the ×, offers the end again, and so do a drag between the two ends, Undo and, in the terminal, `w`); the × is at the middle of its link, which can be over a node, or over the middle of a send between the same two places (a lane that has no effect and a cable from its instrument to
the same input: the same curve), as the level of a send can.
A way out has two words and not a place: where the end goes instead (a lane to the mix beside its group, to another group's bus) is a cable at the end of the chain (`from = "instrument[0].effect[1]"`).

### The input of an effect (`feed`)

An effect is fed by the stage before it in its chain: the lane's instrument for the first effect of a lane, the effect before it for the others, and the sum of the group's members for the first effect of a bus. That
link of the audio path is the way that a chain is wired by itself, and `feed = "none"` on the table of an effect (`[[instrument.effect]]`, `[[group.effect]]`) cuts it (step 86 of `docs/web-ui.md`, an extra asked
for on 2026-10-23: "all instruments, fx etc are free standing modules, that can be/or not be connected with cables"): the effect is then fed by the audio cables that end at it and by nothing else, and the chain goes on from
it all the same (what it puts out still goes to the effect after it, or to the lane's way out). `"chain"` is what it is without, and is not written; a word that is neither refuses the session, with its place
(`instrument[1].effect[1].feed is "None": a feed is "chain" or "none"`). With the way out (`route`, which cuts the end of a chain) it makes every link of a lane's chain one that can be cut: an oscillator can be
heard through the second effect and not the first, and an effect that no link feeds is a module of its own that cables go into and out of.

- In the model it is `PluginSlot::fed` (`true` unless the file says `none`), in the engine `InstrumentLane::cut_feeds` and `Group::cut_feeds` (the *ids* of the effects whose link is cut, and not their places: the cut goes with the
  effect when it is moved, a lane is swapped, or a lane or a group is taken out and put back by undo, and is forgotten when the effect is taken out) from `GraphCommand::SetEffectFeed { node, fed }` (the loader sends what a file
  cuts, after the effect is in and before the cables, so that a cable that only a cut feed keeps from closing a loop is drawn). In the plan it is the link that is not there: `build_patch` makes no cable into a cut effect and
  carries on from it (`LaneWiring::cut` and `GroupWiring::cut` carry it to the model's check for a loop, and the plan is made again when it changes: `Shape` has a mark for it, `CUT_FEED`, only for an effect that is cut,
  so that a session that cuts none has the shape it had). The effect runs all the same; what is put into it is the sum of its cables, or silence if it has none (the kernel's rule for an input with no cable), and a reverb's tail rings out.
- The intent is `SetFeed { node, feed }` (`session::Feed`: `Chain` and `None`): an effect of a lane's chain or of a group's bus; refused, with nothing changed, for any other node (`only an effect has a feed`; not found for the
  master, as for every intent) and for a feed that is **given back** where the audio would go round with the cables there are (`giving that effect its input back would make the audio go round, by the cables there are`:
  `SessionModel::loop_if_feed_is`, and the engine's `route_problem`, which logs it and keeps the feed cut): a cable from an effect to one before it in its chain is a loop only through the link into the later one, so that
  with that link cut it is drawn, and the link is then not given back; cutting one cannot close a loop. It is told as a snapshot (the links of the graph change: the graph has no link into a cut effect, and the node says
  `feed`: `NodeKind::Effect { plugin, mix, feed }`), and it is a step of the history ("cut the input of Filter", "give Filter its input back": never merged with another, none for a feed that is as it is asked to be, and what could
  be redone is still to redo). An effect that is taken out and put back by undo is cut still (`Inverse::Effect`: the journal sends `SetEffectFeed` again after the effect is in), and so is a lane or a group that has one.
- The wiring is changed **at once**, as a lane's group and a way out are, and not over a buffer, as a mute and a cable are: what is sounding in the effect when its link is cut stops going into it with a click (a cable that is turned
  down, or a mute, is the way to do it without one).
- The engine is held to the model by `LaneFacts::cut_feeds` and `GroupFacts::cut_feeds` (the cut effects in the order of the chain: `testutil::model_lanes` and `model_groups` have the model's, and the oracle compares them after every block),
  and the plan to the loop that it replaced by every scenario that cuts no feed (`Scenario::has_cut_feeds`: the loop has no way to leave a link out, as it has none for a cable or a cut way out).
- The terminal has `f` on an effect (see the key table: a lane's or a bus's) and says `[no in]` at the end of its line, after the format (`├─ fx Filter  [Built-in]  [no in]`; for a bus effect, after `(bus)`). The Help tab lists it, the action
  bar does not.
- **The page** (step 88b; see "Links are cables"). *The link is a cable with the button that cuts it.* Every link of the audio path is an amber cable with a **×** at its middle, and a link into an effect is that effect's input: the button is named `Remove cable Oscillator → Filter`
  (`Remove cable Filter 2 → Delay 2`, `Remove cable Group 1 → Filter 1` for the first effect of a bus; `RemoveLinkButton` in `app/patch/edges.tsx`), asks `Editor.setFeed(effect, "none")` and puts the focus on the menu that sends the audio of the node the link started at
  (`Send audio from Oscillator`), which draws it again: a cable between the two ends of a cut link is the input given back (`link_or_cable`). `toFlow` says whose the link is (`feedOf`: the effect that a link of the audio path goes into; `label` is the link's words, as for a way out).
  *There is no button on the effect that says where it is fed from any more* (`FeedButton`, `inputOf` and `.tang-feed` are gone: an effect is fed by the cables that end at it, and by nothing else if its link is cut). The canvas draws no link into an effect that is cut, so that a cut is
  seen as the cable going; the chain's link out of it stays. The mock engine does what the controller does (`setFeedOutcome`, the loops that `feedLoops` in `client/wiring.ts` looks for, the history's words), and its chain operations (an effect added, taken out or moved) make the links again
  from the nodes (`relinked`, over `linksOf`, which lists them as the controller's graph does and leaves out a cut one), so that the cut is the effect's wherever it is; pinned to the controller by the `feeds` list of the fixtures' shapes
  (`shape_of`, `shapeOf`) and by the script of `history.json` that cuts and gives back feeds, takes a cut effect out, moves one and undoes it all.
- **The file.** `feed = "none"`, written only then (`SaveEffect::feed`, `EffectOut::feed` skipped for the chain); `session::Feed::parse` reads `chain` and `none` and nothing else.
TODO: the Patch tab (the matrix) shows no link of a chain and so not its cut (`f` on the Session tab and the page have it); a lane that has no effect has only its way-out link, which `route` cuts; an effect that is cut has no way to say,
on the Session tab, where it would be fed from when it is given back (the key is a toggle); and `f` is not in the action bar of the Session tab (its hints are in about a hundred snapshots; the Help tab lists it).
TODO: a cut is at once and not over a buffer (as a way out's): a link that is cut while a note sounds in the effect stops it with a click; a cable's fade over a buffer would do it.

### An effect that stands by itself (`AddFreeEffect`)

An effect with no instrument in front of it: what the page's "+ Effect" adds (step 87 of `docs/web-ui.md`, the second half of the answer to "in my view there shouldnt be no "lane" anymore, all instruments, fx etc are free
standing modules, that can be/or not be connected with cables", asked for on 2026-10-23). To the controller and the file it is **a lane that has no instrument** and the effect for its chain (the lane is the effect's holder:
it has the mute, the solo, the way out, the modulators and the place among the lanes that every lane has); to the page it is the effect alone, a module that is fed by the audio cables that end at it and by nothing else (nothing
is in front of it that has a sound), and that **sends nowhere until a cable says where**: it is made bare, with its way out cut (step 88a, see below and "Links are cables"). The engine and the file have nothing new for it: a lane with no `plugin` and effects is what step 85
made of a lane whose instrument is taken out, and the effects of such a lane run.

- **The intent** is `AddFreeEffect { plugin }` (`apply.rs`: `add_free_effect_intent`, told as a snapshot). `Controller::add_free_effect` loads the plugin from the catalog as `add_effect` does and refuses what it refuses (a plugin
  the catalog does not name, an instrument, one that does not load, one whose outputs are not the chain's channels) **before anything is made**, and then makes the lane (`add_lane`: at the end, full range, in no group, with a meter of its
  own and nothing in it, and **its way out cut**: `route = "none"` in the model and `SetLaneRoute` to the engine before the effect goes in, so that the module is bare: step 88a) and puts the effect in it (`insert_effect`), the lane's node first and the effect's second, so that a refusal leaves no lane and uses no id (`add_free_effect_loaded`, held by
  `an_effect_that_stands_by_itself_and_is_turned_away_leaves_no_lane_and_uses_no_id`, which compares the model with its ids).
- **It is a step of the history** in the words of the button that adds an effect (`add Reverb`): a `Plan::Added` whose `takes_out` is `RemoveNode { node: lane }` for the two nodes it made (the lane goes with the effect in it, whole: `Inverse::Lane`),
  and redo makes the lane and the effect again under the ids they had, in that order. **A lane that holds no instrument is called in the words of a step by what it holds** (`journal::lane_words`, `laneOrGroupWords` in `client/history.ts`): `Reverb`
  for its one effect, `a chain of effects` for more, `an empty lane` for none (`mute Reverb`, `cut the way out of Reverb`, `remove Reverb`; a lane that has an instrument is `the lane of Oscillator`, as it was). The words do not look at what the page's rule
  looks at (below: a key range, a group, a pattern): a lane that has one is called by what it holds too.
- **The terminal** has `N` on the Session tab (chain focus, whatever entry the cursor is on; see the key table): it opens the effect selector ("Select Free Effect", `SelectorMode::FreeEffect`) and adds the one that is chosen as the intent does. The
  terminal's tree is a tree of lanes, so it shows the lane as `♪ (empty)` with the effect under it (a module list is P5). The Help tab lists it, the action bar does not.
- **The page.** *A lane that only holds effects has no card* (`holdersOf`, `client/holders.ts`): a lane that has no instrument, has effects, and has nothing else of its own that its card shows (no key range, no pattern, no group, no cable of notes into
  it and no audio cable from its instrument) is a **holder**. `toFlow` lays the canvas out, remembers its places and draws it without the lane and without the links at it (the link from the lane into its first effect, the links by which the lane hears every
  source); the effects are modules, in a row from the first column (`stripOf`: the row of a lane without a card is its effects), with the modulators over them as always (`carryOver` goes by the first effect for a strip that has no card, so that what is added to
  a strip that stayed goes over it), and a lane that is given a key range, a pattern, a group, an instrument or a cable gets its card back with the next graph (`sameNodeStructure` knows whether a lane has a range or a pattern: they decide it). The strip of a lane is laid out again when its card goes or comes (the effects of a lane whose instrument is taken out alone close up to the first column, where a holder's strip starts, and the other strips stay), by the effects' slots (`effect 0 of 3` for a holder's, `effect 0 of instrument` for a lane with a card). A lane whose
  instrument is taken out alone (step 85) is drawn so too when it has nothing else: its filter and its LFO stand by themselves. *What the card said of the lane is on the first effect* (`PatchNodeData.heldIn`: the lane, whether this is the first
  effect and whether it is on its own): the "Mute / Solo" cell (`MuteSoloCell` of the lane's node: one control more for the module, `controlCount` and `nodeHeight`'s `carriesLane`) and the "+" that adds a modulator to the lane
  (`AddModulatorMenu`). *It begins bare* (steps 88a and 88b: see "Links are cables"): no cable leaves it, nothing on the module says so, and a cable from its output to the master, drawn or chosen from its send menu, gives it its way out. *The "×" of such an effect* takes the lane out with it when it is on its own (its only effect, and no modulator in the lane to be left
  behind: `heldIn.alone`; `RemoveNode` of the lane, "remove Reverb" in the history, the focus to the bar's "+ Effect"), and takes the effect out alone otherwise (a lane that keeps modulators is a card again, empty, when its last effect goes). *The
  names*: `displayNames` calls a holder by the first of its effects and numbers the lanes that are drawn without it (the cell's buttons are `Mute Reverb`, and the notices say "Reverb"). *The bar* has "+ Effect" (`app/AddFreeEffect.tsx`,
  `AddFreeEffectMenu`, the button `Add an effect`, a menu of what is installed and can be an effect): it asks `Editor.addFreeEffect`, and the canvas is shown the effect (the lane has no node on it). `RemoveLaneOrGroupMenu` gives the focus, after
  the instrument of a lane is taken out alone, to the bar's "Add an instrument" when the lane has no card then. The menu of a source, "Send notes from X", does not offer a holder (`noteToItems`). *The mock engine* does what the controller does
  (`addFreeEffectOutcome`, `planOf`, `takesOut` and `laneOrGroupWords` of `history.ts`), pinned by two scripts of `history.json` (an effect added, its lane muted, its way out cut, a second effect added, the lane soloed, and Undo and Redo through all
  of it; and the refusals, which leave no step and use no id).
- **The oracle** has a session (`free`: an oscillator whose way out is cut, and a lane that has no instrument and a reverb, fed by a cable from the oscillator: it is heard through the reverb alone, and with the cable taken out the render is
  silence to the bit) and a scenario (`edited:free`, `FREE_EDITS`, 14 edits: a reverb is added in a lane of its own, which is made bare (its way out is cut: step 88a), and a cable feeds it, its lane is muted and unmuted, is given its way out, and a cable from the reverb to the mix is drawn beside it,
  the lane is soloed and not, a filter is added after the reverb, the lane is taken out with its two effects and two cables and put back whole by Undo, the filter is taken out by Undo and put back by Redo and out again) with
  `Edit::AddFreeEffect`; the older renders are the same bits.
TODO: a holder's effects are still its lane's: an effect cannot be moved from one lane to another, nor made a chain of free effects from the page (a holder has no `fx` menu, since it has no card: more effects in one are cables, or a chain
that a lane had when its instrument was taken out), and a modulator that hears the holder's lane is still that lane's by its ear (a modulator of the session's own reaches any module since step 94, and the "×" of an effect that is on its own takes the lane out and leaves the modulators that heard it, hearing every note).
TODO: the lane is a rule of the picture, not of the model: the terminal's tree shows it as `♪ (empty)`, the Patch tab's matrix has it as a lane (`L3 (an empty lane)`), and `lane_words` calls a lane by what it holds even where the page draws its card; the file
has a lane with no `plugin`. P5 dissolves the lane in the model and the file (a netlist of nodes and cables).
TODO: the bar is full again: with "+ Effect" it needed 1,444 pixels in a row with the field of Save as open, with "+ Modulator" (step 92) 1,555 pixels, and 1,467 since the page lost "+ Group" (step 103; `e2e/saveasbar.spec.ts`): an `Add ▾` menu (instrument, effect, modulator, source) would take the four buttons back to one.
TODO: `N` is not in the action bar of the Session tab (its hints are in about a hundred snapshots; the Help tab lists it).

### Modules: effects and instruments, with the cables between them, saved to a file (`SaveModule`, `SaveSelectionAsModule`, `module:<name>`, steps 104 to 106)

The user's "In the future, I want to be able to design a set of modules, save them as a stand-alone module that you can then load inside another instrument, like a submodule doing FX etc, (imagine input->reverb->delay->output) and you can load that as a module in another graph. Think thats doable?" (2026-10-29), taken in the order that
the user agreed to ("3. order is ok": the limiter, then the groups hidden from the page, then this, then P5c). **Step 104** built the first part: **the chain of effects of a lane**, saved whole to a file, listed in the catalog, and added whole, as a lane of its own, to this session or another. The user, shown that, with a picture of three effects that are each in a lane of
their own and joined by two cables: "hmm, not sure I understand, the goal is to save the graph of fx or instruments, like this [Image #4], into its own module? is that possible?" It was, and step 104 had tied it to P5c more than it needed to: the lane is only how the model holds a plugin, an effect that stands by itself is a lane that has no instrument, and a session file can hold any number of
such lanes with cables between their plugins. **Step 105** makes a module **any plugins that are chosen** (the instruments and the effects, from any lanes) **with the audio cables that are between them**, saved as a file and added again as a lane for each of the module's. **Step 106** makes a module **the modulators that are chosen with them** as well (an LFO, an envelope, a macro, a MIDI control, a step sequencer), with the cables that they have to what is in the module: the user's "yes agree LFOs should be next!" (2026-10-30). What is not built is a module as one node with ports in another graph, which needs the lane to be dissolved (P5c) and a library of references
(P6 of `docs/patch-graph.md`); every part of this (the library, the catalog's entries, the intents, the history, the file) is what it needs as well.

- **What a module is.** A session file of **lanes that hold plugins** (`app/module.rs`: `parse` refuses a file that has a group, a source of notes, no lane, or a lane that has neither an instrument nor an effect, and a modulator that names what the module has not: see below): each lane has an instrument or none, and its effects in the order of its chain, each plugin as a session file says one (`plugin`, `preset` by its name,
  the `params` that are not the plugin's own once the preset is loaded, the `mix` of an effect and the `volume` of an instrument, `pinned` by name, `feed = "none"` where the stage before an effect does not feed it), and the `[[cable]]` tables between those plugins (`from = "instrument[0]"` for what an instrument puts out or `"instrument[0].effect[0]"`, `to = "instrument[1].effect[0]"` or `….effect[0].side`, a `gain`):
  a cable that has an end outside the module, or goes to the mix or to a group's bus, is the session's and not the module's, and where the audio comes into the module and goes out of it is the cables that a person draws once the module is in a session. The lanes of the file are the lanes that the module adds (`Module`, `ModuleLane`, `ModuleCable`: three effects that were each in a lane of their own, with two cables between them, are three
  lanes and two cables, and effects that were in one lane are one lane, as in step 104). No place (`ui`), key range, pattern, mute or way out is kept: a module is laid out where it is put, and is added bare. The text is `module::to_text_of` (the same writer as a session's, from the model, behind a comment line that says what it is), so that the file can be read, kept in a repository and edited by hand. **The modulators (step 106)** are `[[modulator]]` tables, the session's own as a session file says them (the kind and its settings: `save::source_to_save`, which the session's saver uses too): `hears = "instrument[N]"` for the lane of the module that the modulator hears and nothing for every note, and a `[[modulator.target]]` for each cable, which drives a parameter of a plugin of the module (`to` is the plugin and `param` the parameter by its name), the volume of a lane's instrument or the mix of an effect (`gain = "volume"` or `"mix"`, with `to`), a setting of another of the module's modulators (`mod_rate`, `mod_attack`, `mod_decay`, `mod_sustain` or `mod_release`, by its place among the file's) or the depth of another cable (`mod_depth = [m, c]`: the cable `c` of modulator `m` among its file's). `parse` looks for every place among the module's own and refuses the file, with the place (`modulator[1].target[0] names "instrument[3]", which is not a plugin of the module`), for a `hears` that is not a lane of the module, a `to` that is not a plugin of it, a volume of an effect or a mix of an instrument, a modulator that is not there or is itself, a cable that the other has not and a cable that drives nothing (`ModuleModulator`, `ModuleTarget` and `ModuleDrives`: `Param`, `Gain`, `Setting` and `Depth`); a parameter that the plugin has not is not refused: it is left out when the module is added.
- **The library.** One directory, `~/.config/tang/modules/` (`TANG_MODULES_DIR` says another, `main.rs`'s `modules_dir`; the tests of the real program, `tests/serve.rs` and the browser specs, each give a program a directory of its own, so that none reads or writes the person's own), one file `<name>.toml` for each module, the name being a plain file name
  (`module::clean_name`: `session::plain_file_name`, without the extension, and not a name that ends in `.toml`, which the listing would give another name). The controller reads it at the start and again when a module is saved (`Controller::refresh_modules`, `module::list`: a file that is not a module is left out and said in the log, a directory that is not there has none); it does
  not watch the directory, so a file that is put there by hand is listed when the program is started again. Every program that has a session has it (the terminal, `play` and `serve`); a controller whose `modules_dir` is none (the tests', unless they say) keeps no library and says so (`this program keeps no library of modules`).
- **Saving: `Intent::SaveModule { node, name }`, told as `Event::ModuleSaved { name }`.** `node` is a lane or an effect of its chain (the chain of that lane is saved), `name` the name as it was typed. Refused, with nothing written and no revision used: a node that is not there (the master too: `there is no node N`), a name that is no name (`a file name is needed`, `con is a name that
  Windows keeps for itself`, the rule of Save as, in the same words), a group, a group's bus effect or any other node (`a module is the chain of a lane: a group's bus is not saved`, `a module is made of the chain of a lane`), a lane that has no effect (`a lane that has no effect has no chain to save`), a program with no library; the node is looked at first, then the name, then the
  chain. A module that has the name is written over. It **changes nothing of the session**: not marked (and not cleaned: a session that was unsaved is as unsaved after it), no step of the history (`Plan::Nothing`: there is nothing to undo; the controller of a front-end that is told `ModuleSaved` changes nothing in its copy but the revision, `Snapshot::apply`), no command to the audio thread;
  it uses a revision, as a save does, and the catalog is told again (`Catalog::set_modules` bumps the catalog's revision, which `web::owner` compares) if what it lists is not what it listed: a module saved again with as many plugins is the same entry.
- **Saving what is chosen: `Intent::SaveSelectionAsModule { nodes, name }`** (step 105; told as `ModuleSaved` too). `nodes` are the plugins that make the module: the instrument of a lane (the lane's node) or an effect, any of them, from any lanes, and (step 106) the modulators that go with them. Each lane that has one of them is a lane of the module, with its instrument if that was chosen and the effects that were, in the order of their
  chains, each **as it is now**, and the audio cables that have **both ends among them**, in the order of their ids, at their levels (`module::to_text_of`; `SaveModule` is the same for the effects of one lane: `module::chain_of`). What joins them to what is not chosen is left out: the cables that go in and out, the way out of a lane, and **a link from a stage that was not chosen**: an effect that is the first of its lane in the module and was not
  the first in the session, or follows one that was left out, is not fed by the chain and is written `feed = "none"` (the first effect of a lane that has no instrument in the module has nothing before it either way and keeps what it had, and a link that was cut stays cut). Refused, with nothing written and no revision used, in the order of the looking: the first node that is not there (`NotFound`, the master too), a name that is no name, then what a module has none of
  (`nothing is chosen: …`, `a lane that has no instrument has none to save: choose its effects`, `a module is made of effects and instruments: a group's bus is not saved`, `a source of notes is not part of a module: …`, and, for a modulator and nothing for it to go with, `a modulator alone is not a module: choose the effects or instruments that it goes with`), and a program that keeps no library. Like `SaveModule` it changes nothing of the session: it is not marked, and is no step of the history. **The modulators that are chosen (step 106)** are kept as they are now (their kind and settings), each with **the lane of the module whose notes it hears, if it heard a lane that is a lane of the module** (the lane's place in the module: a modulator that heard a group, a lane that is not in the module or every note hears every note), and with the cables **whose ends are in the module**: to a parameter of a plugin that is chosen (by its name), to the volume of an instrument that is chosen and the mix of an effect that is chosen, to a setting of another modulator that is chosen and to the depth of another cable that is kept; a cable to anything else (a plugin that is not chosen, a group's volume or pan, a bus effect, a modulator that is not chosen) is left out, and **a cable that drives the depth of another goes with that other** (the cables are looked at again until none more goes: `modulators_of_text`), so that what the file says of a cable is the place that it has among the cables of its modulator that stay. A modulator that is chosen and has no cable that stays is in the module all the same (its cables are the person's to draw). The cell's ⤓ (`SaveModule`, the chain of a lane) saves no modulators.
- **Listing and adding.** The catalog lists the modules with the plugins: `module:<name>`, format `Module` (`format_from_id`), no instrument, `params` is how many plugins it has (the instruments and the effects) and `presets` is 0, by name without regard to case, a plugin before a module that has its name (`Catalog::entries`). They are kept apart from the plugins (`Catalog::modules`, a list of its own, which a scan and the loading of a plugin
  do not see). A module is added by the intent that adds an effect that stands by itself, **`AddFreeEffect { plugin: "module:<name>" }`**: a lane of its own at the end for each lane of the module, bare (its way out cut, as every module that stands by itself is: `route = "none"`), with the instrument that the module's lane has, if it has one (`Controller::add_bare_lane`), and the effects of its chain in their order, each plugin with the preset by its name, the values over it, the mix or the volume, the knobs pinned, the
  link into an effect cut where the file says so, and then the cables that the module has between the plugins, a second input included; the lanes are made in order, each lane's node first (an instrument has the lane's node and none of its own) and its effects' after it, and then the cables, so that a redo gives all of them back under the ids they had. **Every plugin is loaded before anything is made**: a module that names a plugin that is not installed (or one of the wrong kind: an instrument where an effect goes, an effect where the instrument does, or one that does not load) is refused (`the module Room
  cannot be added: clap:com.example.nothing is not a plugin that is installed`) and leaves no lane and uses no id; what the file says of a preset or a value that the plugin has not is logged and left out, as the loader leaves it out of a session, and so is a cable that would make the audio go round. **The modulators (step 106) are made after the lanes and their audio cables** (`make_module_modulators`): all of them first, each hearing the lane of the module that it heard (`Controller::add_modulator_hearing`, the one place that makes a modulator's node, its ear and its slots), then the cables that drive a parameter, a volume, a mix or a setting, in the order of the file (`module_cable`: `param_cable`, `gain_cable` and `field_cable`, as the intents make them, at the depth that the file says, held to the range of a depth), and then the cables that drive the depth of another cable, in sweeps, each as soon as the cable that it drives is made; a cable that cannot be made (a parameter that the plugin has not, a value that a modulator drives already, a setting that the other has not, a depth of a cable that is not there) is logged and left out, as the loader leaves it out of a session, and nothing is allocated for it. It is a step of the history in the words of the button that adds an effect (`add Room`: `Plan::Added`; `takes_out` takes **every lane and every modulator that
  was made** out, the last first, each with all that is in it and the cables at it, where for a plugin it is exactly the lane and one effect and no cable: a step has the intents that undo it, a list, `Step::added`), undo takes the lanes out with their plugins and cables, and redo reads the library again and makes them again under the same ids (a module that was saved over in between is made as it is now). A module is a **copy**: what is changed in the file later changes no session that has it, and what is changed in a session
  changes no file until it is saved. **A module is no plugin that goes in a chain**: `AddEffect`, `AddInstrument` and `ReplaceInstrument` refuse its id in words (`a module is added as an effect that stands by itself, in a lane of its own: it is not a plugin that goes in a chain`; `Controller::load_from_catalog`), and the lane's and the bus's lists of effects do not list it.
- **The terminal.** `K` on the Session tab (chain focus, the cursor on a lane, one of its effects, its pattern or a modulator that hears it) opens the popup "Save Chain as Module" (`SaveModuleState`, a field for the name, the reason a name is refused in red under the title and cleared by typing, Enter asks `SaveModule` and Esc leaves) for the lane's chain; nothing opens for a lane that has no effect, a group, the other tabs or the parameter pane. `N`
  lists the modules first, with the format `Module` and their number of plugins where the plugins have their number of parameters, and then the plugins (`Catalog::listed`; only the selector of `N` lists them: `a` and `n` and `i` do not); a module that is chosen is added by `AddFreeEffect` after a frame that says `Loading <name>…`, as a plugin that is not built in is, since its file names the plugins to load. `N` adds a module that is a graph as it adds any (a lane for each of its lanes, with its instrument, the cables between the plugins, the modulators, which the tree lists under the lane that each hears, one step that Ctrl+Z takes out whole). The terminal has no way to choose the plugins to save: `K` keeps the chain of a lane, and choosing a graph is the page's (see the TODOs). The Help tab lists both keys; the action bar does not (its hints are in about a hundred snapshots).
- **The page.** *Saving:* a button `Save the chain of Oscillator as a module` (⤓) in the "Mute / Solo" cell of a lane that has a chain of effects (a lane's card, and the first effect of a lane that only holds effects, which has the lane's cell: `chainLengthOf`, the selector; none for a group, an empty lane or a lane that has no effect), which opens a small field for the name under it, in the page and not in the node (`app/patch/SaveModule.tsx`: a dialog with a
  text field `Module name`, `Save` and `Cancel`; the field has the focus when it opens, Enter or Save asks `Editor.saveModule` for one name at a time and never for one of nothing, Escape or Cancel leaves and the focus goes back to the button, a press anywhere else, a wheel or a resize closes it and leaves the focus where the person put it); a name that the server refuses is said among the notices and the field stays with what was typed, and a module that is
  saved is said as well (`Saved the module "Room": + Effect adds it`). *Adding:* the bar's "+ Effect" lists the modules first, under a heading `Module`, and the plugins after (`freeEffectItems`; `effectItems`, which the lane's and the bus's "fx" menus use, leaves them out). *The mock engine* has a library in memory (`client/modules.ts`, `MockServer`'s `#library` and the option `modules`): `saveModule` and `addFreeEffect` of a `module:` id do what the controller
  does (`saveModuleOutcome`, `addModuleOutcome`, `moduleEffectNode`, the catalog told again when a module is new or has another number of effects), pinned to the controller by `fixtures/modules.json` (a session with a chain of three effects, a preset, a value, a mix, a pin, a cut link, two cables and one at a second input, saved, added, undone, done again, saved again under another name and added: every step's events, nodes, cables, layout of the telemetry, history and modules listed; and
  the 21 refusals in the controller's words), and `takesOut` of `client/history.ts` is the controller's. *The real program's browser specs* give each program a library of its own (`TANG_MODULES_DIR`: `own.ts` for a program of a test's own, `playwright.config.ts` for the shared one); `e2e/modules.spec.ts` runs it on the mock and on the real program (the tracer's chain is saved from the lane's cell, the file is in the library with the filter and no oscillator,
  the module is added bare and is silent until cables are drawn and then the note is heard through it and through nothing else, the saved session has the lane that the module made and the cable into it, a page that is opened again and a second page list the module, which is a file and not something that the page has).
- **Choosing, on the page (step 105).** The corner of the canvas has a button **Select** (`Choose nodes to save as a module`, `aria-pressed`) beside Arrange. With it on, a click on a node chooses it, a click with Shift (or Control or Command) held adds one to what is chosen, and a box drawn on the canvas chooses the nodes that are all in it (React Flow's selection: `elementsSelectable` and `selectionOnDrag`, with the middle button panning, and the right button the menu's since step 107); a node that is chosen is outlined, a node is moved by a drag as it is with it off (the nodes that are chosen with it, since step 107), and a cable is not chosen.
  **With it off (step 107)** a box drawn with Shift held chooses all the same, and a click with Shift (or Control or Command) held adds a node to what is chosen: Shift is the mode for as long as it is held; a plain click chooses nothing (see "Right-click menus, choosing with Shift and moving what is chosen"). The bar says what is chosen (`3 chosen`, `1 chosen, 1 that a module cannot hold left out`, or `Drag a box round the nodes to keep` with none: `modulePluginsOf` in `app/patch/selection.ts` holds the instruments of lanes that have one, the effects that are in a lane and (step 106) the modulators, and leaves out a group,
  a bus effect, a source of notes, an empty lane and the master; `Held.plugins` counts the instruments and the effects) and has the button **Save as module…** (`Save the chosen nodes as a module`, out of reach while no instrument or effect is held: a modulator alone is not a module, and the bar says `, and a module needs an effect or an instrument`), which opens the field for the name under it (the dialog of the cell's ⤓: `ModuleNameButton` in `SaveModule.tsx`) and asks `Editor.saveSelectionAsModule` for the plugins that are held; what is chosen stays when it is saved. Pressing Select again, or Escape (unless something that
  is open took the key: a menu closes with it), turns choosing off and forgets what was chosen. *The mock engine* does what the controller does (`moduleOfNodes`, `saveSelectionOutcome`, `addModuleOutcome` for lanes and instruments, `takesOut` giving a list of intents), pinned by `fixtures/moduleGraphs.json` (a session of an oscillator with a chain of three effects, a lane of a math effect and a drive with cables between the lanes, an empty lane, a group, a modulator and the keys:
  a script of saves of what is chosen, adds, undos and redos, 25 refusals in the controller's words, and a module that a hand wrote that has an instrument and what a plugin cannot take, `OddVoice`).
- **Decisions** (D104 of `docs/web-ui.md`; the user may overrule them). (1) A module is the chain of effects of a lane, and not yet a set of nodes with cables of their own, which P5c and P6 make: it is what can be built and used now with nothing taken back later. (2) A module is a session file of a lane that has no instrument, so that the reader and the writer of sessions are its, and a person can read and edit one. (3) One directory, one file for each module, named by the
  person; `~/.config/tang/modules/` unless `TANG_MODULES_DIR` says. (4) The catalog lists the modules with the plugins and `AddFreeEffect` adds one, so that the page's "+ Effect" and the terminal's `N` need no menu of their own. (5) A module that is saved under a name that has one replaces it, with no question (a file is a file; the terminal's Save as does not write over one, and the question is for a front-end to ask). (6) A module is added bare, like every effect that stands by itself; the
  cables are the person's. (7) The modules come first in the lists, since they are the person's own and a list of plugins that is long would put them last. (8) A module is a copy and not a reference. (9) Modulators are not part of a module (no cable of modulation is kept: a module is a chain and its cables of audio; since step 106 the modulators that are chosen are).
- **Decisions of step 105** (D105 of `docs/web-ui.md`; the user may overrule them). (1) A module is the plugins that are chosen and the audio cables that have both ends among them, as lanes: the lane is how the model holds a plugin, so that a module is no new kind of thing and the file is a session's (a module of one lane of effects is what step 104 saved, to the letter). (2) It is added as a copy, as a lane for each of its own and **bare**, with the cables that were between its plugins and
  none that went in or out: an instrument that is added sounds when a cable is drawn from it, or from the end of its chain, to the mix (one drag), as every module that stands by itself does. (3) What is not chosen is left out with what joins it, and an effect after one that was left out is not fed by the chain (`feed = "none"`), since the link is the template's and not the module's. (4) Choosing is a mode of the canvas that a button turns on, so that the canvas is as it was with it off (a click on a node and a drag on the canvas have the meanings they had); a box
  and a click choose, and the mode has no key but Escape (step 107 made Shift held the mode for as long as it is held, and Escape gives up what is chosen whenever any is, with choosing on or not). (5) `SaveModule` is as it was, for the cell's button and the terminal's `K`; the new intent takes nodes, which is what P5c has as the only kind of thing. (6) A module that is not a chain is listed with how many plugins it has, and is added by the same intent as a plugin that stands by itself, so that the bar's "+ Effect" and the terminal's `N` need no menu of their own: a module that has an instrument is in
  the menu of effects too (a `+ Module` or an `Add ▾` menu would say what it is).
- **Decisions of step 106** (D106 of `docs/web-ui.md`; the user may overrule them). (1) A module holds the modulators that are **chosen** with its plugins and no others: no modulator is taken along because it drives a plugin that is chosen (that would make a module of a session), and a modulator that is chosen and drives nothing that is in the module is saved all the same. (2) A cable is kept when **both its ends are in the module** and left out otherwise, as an audio cable is; a cable that drives the depth of another goes with that other. (3) The ear is kept when it is a lane of the module (as the lane's place in the module) and is **every note** otherwise: a group is never kept (a module has none), and a module is added bare, its notes coming from whatever the session's sources send. (4) A modulator alone is not a module (the bar's Save is out of reach, with the words, and the intent refuses in words): a module is made of plugins, and the modulators go with them. (5) The cell's ⤓ (the chain of a lane) saves no modulators: choosing is how a module gets them. (6) What cannot be made when a module is added (a parameter that the plugin has not, a value that a modulator drives already, a setting that the other lacks) is logged and left out and nothing is allocated for it, as the loader leaves out a cable of a session's file. (7) The modulators are made after the lanes and their audio cables, all of them first and then the cables, those that drive a depth after the cable that they drive, so that a redo gives the same ids; Undo takes the modulators out before the lanes. (8) The catalog's number for a module is its plugins (the modulators are not counted among them).
- **Held by tests.** Rust 31 and one of the real program: `module.rs` 4 (the id and the name, the name of a file, the reader of a module and what it refuses, the library's listing), the controller's 8 (`apply.rs`: saved and listed with the session as it was, added bare and undone and redone under the same ids, what leaves no lane and no id, what is refused with nothing written, the second input and the level of a cable, what a plugin cannot take of what a file says, the library's directory made, and `fixtures/modules.json`), the catalog's 3, the controller's 1 (the event that neither marks the session nor cleans it), the history's `takes_out` (the test that was there, with the lane and the cables of a module), the terminal's 15 (`tui::golden_tests::modules`: `K` saves and `N` lists first, a module added as one step and undone, a name that is refused and what clears the reason, blanks, the keys that edit the name, a letter with Control held, a click outside, which lanes `K` is for, the selectors that do not list modules, the instrument selector's first row, the frame that says a module is loading, no library, the two snapshots and the Help lines) and `tests/serve.rs` (the real program: a browser saves a chain, a second program on the same library adds it). The page 38: `modules.test.ts` 11 (the mock held to `fixtures/modules.json` step by step and to a module that a hand wrote, the catalog told as it changes and to whom, the refusals in the controller's words, no id used by a refusal, the names and the entries, `moduleOf`), `PatchView.modules.test.tsx` 12 (the button, the field for the name, the bar's list), the editor's 4, the menus' 3, the history's 2, the reducers' 2 and the selector's 4; and `e2e/modules.spec.ts` in the browser suite (2, on the mock and on the real program). Rust 2,563 tests (2,749 with the `web` feature, 2,557 without the default plugin formats), web 2,970 in 128 files, browsers 211. **Held by mutants** (step 104): 203 mutants of the logic of the step, in two runs, one at a time against the unit tests that should catch them: 119 of the page (the mock engine's library and `moduleOf` 70, the menus 6, the editor 5, the selector and the reducer 5, the history 4, the bar's list and the cell's button 4, the field for the name 25) and 84 of the Rust (the module file 33, what the controller makes of a module 19, the catalog 8, the history and the snapshot 6, the terminal 18): 167 were caught by the tests that were there (3 of the page only by the type check: a variable that the mutant left unused) and 36 survived: **34 gaps, each a test now** (19 of the page: what a module file that a hand wrote says that a plugin cannot take, who is told that the catalog changed, and the field for the name's focus, selection, Escape, wheel, resize, place, submit and Save button; 15 of the Rust: the module file's refusals and its listing, the library's directory, a catalog entry replaced by another with as many effects, and the terminal's popup), **one dead guard, out of the code** (`lane.modulators` in the module's reader: `session::parse` has moved a file's lane modulators to the session's own, so that a module that has any has `free_modulators`) and **one that was a filter of the run** (the snapshot's mark after a save: the older test that holds it was not in the run's filter, and catches it); and the **3 that only the type check had caught** (the mix held to 1, the Save button out of reach while a request waits, the submit that is held back) are tests now as well.
- **Held by tests and mutants** (step 105). Rust 2,574 tests (2,760 with the `web` feature, 2,568 without the default plugin formats; 9 of the command line and 32 on the real program as before): `module.rs` 6 (4 before), the controller's 7 graph tests (`apply.rs`: effects from two lanes saved with the cable between them and added as two lanes in one step; an instrument with its values and its cables, the file of what was added and saved again, which is the same; an instrument alone; the feeds; the cable to the mix and the cables to and from what is not chosen, with the place that an effect has in a module after a gap; what is refused with nothing written; what cannot be loaded, whichever lane the fault is in) and the fixture `moduleGraphs.json` (17 steps, 25 refusals and a module that a hand wrote), the history's `takes_out` (a list, the lanes of a module the last first), the terminal's `N` with a graph. The page 2,995 in 130 files (`modules.test.ts` 25: the mock held to `moduleGraphs.json` step by step, the catalog's news, 25 refusals, the refusals over the wire with their codes, a module that a hand wrote that has an instrument and ones whose plugins cannot be loaded, the feeds, the places of the effects in a module, what a module is made of; `selection.test.ts` 4; `PatchView.selection.test.tsx` 7: choosing, what the bar says, what is asked of the server, Escape, an Escape that something else took, a cable that a click does not choose; the takes-out tests of the history, the audio cables and the sources, which give lists), the browser suite 214 (`e2e/modules.spec.ts`: a box round three effects joined by cables, saved and added as one step that Undo takes out and Redo gives back, a click and Shift-click and Escape, and on the real program the file of the library and the session's own). **Held by mutants** (step 105): 106 mutants of the logic of the step, in two runs, one at a time against the unit tests that should catch them: 56 of the Rust (the module file 35: the reader 12, the chain 2, the writer 20 and the count 1; what the controller makes of a module 14: the save 4, the loading 2, the lanes and cables it makes 4, the values it gives 4; the bare lane 3; the history's `takes_out` 4) and 50 of the page (the mock engine's library and `moduleOfNodes` 20, the mock's outcomes 12, the history 3, the selection 3, the editor 2, the field for the name 2, the canvas 8). The first run caught 95 (one of them only by the type check) and let 11 survive: **five were gaps, and a sixth was the one that only the type check had caught: six tests now** (a lane that has an effect left out between two that are chosen, with the cable into the one after the gap: the cable names the place that the effect has in the module and not the one it had in the session, which the fixture has two steps for, `Skip` saved and added, and a unit test; the master, and a node that is not there, are *not found* where what a module cannot be made of is *invalid*: `apply` throws the words alone, so that the test reads the answer over the wire; a module that a hand wrote whose instrument slot names an effect, an effect slot that names an instrument and an instrument that is not installed, with a fault in the second lane, in the controller's words and with no lane made: the one that only `tsc` had caught, since Vitest never ran the mock's adding of an instrument that is of the wrong kind; an effect that is in no lane, which no server makes; an Escape that something else took, which must not leave choosing; and a click on a cable, which must choose none), **three were the run's own filter** (the test that holds them, `a_cable_to_the_mix_or_to_or_from_what_is_not_chosen_is_the_sessions_and_not_the_modules`, is not the name that 44 of the list's filters said, `…the_mix_or_from_what…`: a filter that matches no test cannot catch a mutant, and three survived on it; caught with the right name, and the other 41 were caught by something else) and **three were code that said more than it needed, out of the code**: the first arm of the feed rule said `lane_chosen.instrument && index == 0` where the cut's own guard says `before.is_some() || lane_chosen.instrument` (the Rust and the page: the mutant that dropped the first half is the same function), and the mock's look, in `#keepModule`, for a node that the intent had been refused for already (a dead guard). The 15 that were run again (6 of the Rust, 9 of the page) are caught.
- **Held by tests and mutants** (step 106). Rust 2,582 tests (2,768 with the `web` feature, 2,576 without the default plugin formats; 9 of the command line and 32 on the real program as before): `module.rs` 8 (6 before: a module that has modulators is read, with the lane that each hears and what its cables drive; a modulator that names what the module has not is refused, with its place), the controller's 5 new tests (`apply.rs`: modulators chosen with the plugins that they drive, saved with their cables and added with them in one step; the cables to what is not chosen and an ear that is not a lane of the module; a cable that drives the depth of another going with that other and naming the place it has among those that stay; a cable to the setting of a modulator that is not chosen; a module that a hand wrote that has modulators, added with the cables that can be made) and the fixture `moduleGraphs.json` (a second scenario, `modulatorSteps`: a rack of modulators saved with the plugins that they drive, added, undone and redone, and a second module saved and added; the refusals of a modulator alone, of one with a group and of one with the keys; and a hand-written module with a macro and an LFO), the history's `takes_out` (the modulators go first, the last first), the terminal's `N` with modulators (the tree lists them under the lane that each hears, and Ctrl+Z takes them out with the lanes). The page 3,008 in 130 files (`modules.test.ts`: the modulators that a module is made of, 5 tests, and the modulators that the mock engine makes of one, 3, with the fixture's second scenario; the selection's tests and `PatchView.selection.test.tsx`: modulators held and asked for in the canvas's order, a modulator alone that cannot be saved and says why, the master alone; the history's takes-out), the browser suite 216 (`e2e/modules.spec.ts`: a modulator chosen with the plugins that it drives and saved, on the mock and on the real program, whose file has `hears` and the cables). **Held by mutants** (step 106): 118 mutants of the logic of the step, in two runs, one at a time against the unit tests that should catch them: 59 of the Rust (the module file's writer 22, its reader 18, what the controller makes of a module 16, the history's `takes_out` 3) and 59 of the page (the mock engine's maker 27, the modulators of `moduleOfNodes` 23, the selection 3, the bar's words 3, the history 2, the Save button 1). The first run caught 111 and let 7 survive: **five were gaps, each a test now** (a cable to the setting of a modulator that is not chosen, in the Rust and in the page, and a cable to the volume of an instrument that is not chosen and one to the depth of a cable of a modulator that is not chosen, in the page: each of them goes, and the cables that name others by the place they have among those that stay must not count it, which the rack of the fixture has no cable to tell: two new tests draw cables to the depths of cables, one of the Rust and one of the page; and the master alone, which the bar must not tell that a module needs an effect, since it holds nothing: `held.nodes.length > 0 &&`) and **two were one dead guard, out of the code** (the writer's check of the kind of a value that the host keeps: `kind_of` has said that the node has it, and a plugin that is chosen is an instrument, which has a volume, or an effect, which has a mix, so that the check said nothing: the two arms are one, and the mutants of the merged arm are caught). The 7 that were run again, and 2 more on the merged arm, are caught. **Two mutants of the fixed-point loop never end** (one in the Rust and one in the page: the cables are looked at again as long as one more goes, and the mutant says that one goes for ever): a test that hangs is a mutant that is caught, and the runners' limits of 15 and 10 minutes were too long on a machine that others use (the loop took a core or two): they were killed by hand after a minute and after five, and the limits are 150 s and 120 s now.
TODO: a module has no source of notes and no cable of notes (a modulator of a module hears a lane of it or every note, and a modulator that heard a group hears every note, since a module has no group; a cable of notes into a modulator is not built either: see "Free modulators", the TODOs of the ear).
TODO: the bar does not say how many cables of the modulators that are chosen are left out (it counts the nodes that a module cannot hold, and not the cables that go to what is not chosen): an LFO that is chosen with the filter that it drives and not the oscillator whose volume it drives too is saved without the second cable, and a person finds that out when the module is added.
TODO: the cables that drive the depth of another are made after the others when a module is added, so that a cable to a depth that comes before another cable of its modulator in the file is made after it, and the model's order of the modulator's cables is not the file's (the sound is the same, and a module that is saved again from that session says them in the model's order).
TODO: the cell's ⤓ (`SaveModule`) keeps the chain of a lane and no modulators: the modulators that hear the lane and drive its effects could go with it (choosing them with Select does it now).
TODO: the terminal has no way to choose the plugins that a module is made of (`K` keeps the chain of a lane, and the page's Select is the only way to choose a graph): a mark on the Session tab's tree, or a choice of rows in the Patch tab's matrix, would do it; `N` adds a module that is a graph as it adds any.
TODO: a module that is added has no place chosen for it: its lanes are added at the end of the session, a row each below the others, its box is a strip of its own under the lanes (step 108), and the canvas lays them out as it lays out any new lane (a node that is added after an arrangement is put by the rules of the rows, `carryOver`), so that a module that is added is not where its cables would have it; Arrange puts the cables short, and arranging only the new nodes with the others pinned (`fixed`) would do it as it is added. The `ui` of the nodes that were chosen is not kept in the file (the place of a box is, in `[[module]]`).
TODO: a module that has an instrument is in the bar's "+ Effect" menu with the modules that are effects, under the one heading `Module`: a `+ Module` or an `Add ▾` menu would say what it is.
TODO: a module is a copy that is read from the file when it is added: a module that is changed in the file does not change the sessions that have it, and a session that has the module twice has two chains that are no longer one thing (a reference, with the module edited in one place, is the library of P6).
TODO: the library has no way to take a module out, to rename one or to look at one from a front-end (a file is taken out and renamed in the directory, and the page and the terminal list what is there when the program is started again: the directory is not watched), and `tang enumerate` does not list them (`tang enumerate modules` is the small change).
TODO: a module that is saved under a name that has one replaces it with no question; the page and the terminal could ask first (the controller has no way to say "is there one" but to try).
TODO: the terminal's popup closes when a module is saved and says nothing of it (the page says so among its notices; the next `N` lists it); a word in the status bar would do it. The selector's column of "Params" is the number of plugins (instruments and effects) for a module.
TODO: a module is added by the effect that stands by itself (`AddFreeEffect`) before the scan has found its plugins: it is refused as a plugin that is not installed is (`the module Room cannot be added: builtin:filter is not a plugin that is installed`), which the page shows among its notices; the controller's own scan finds the built-ins first.
TODO: the plugins of a module, its instruments and its effects, are loaded when it is added, in the order of the lanes and of each chain, one at a time, with the loop blocked for as long as they take (the terminal says `Loading <name>…` first, the page says `Working…` for a request that is slow): a module of heavy plugins takes the sum of what each takes.

### A module that is added is one box (`ModuleInstance`, `[[module]]`, `NodeKind::Module`, step 108)

The user's goal for a module that is added again: "the goal when we re-add a module is that it gets rendered like just a single box if you understand?" (stated after step 107). Steps 104 to 106 added a module as a copy: its lanes, plugins, modulators and cables were in the session like any others, and the canvas drew them one by one, so that a module looked like what it was made of. **Step 108 remembers which nodes came together**: a record, a node of its own, of the lanes and the modulators that one add of a module made. The engine is told nothing of it and runs the plugins flat, as it did (the record is no part of the sound: nothing is sent to the audio thread, and no file of the engine is touched: the oracle's renders that `cargo test` holds to the stored hashes are the same bits (the whole corpus is `#[ignore]`d, run in release, and was not run for this step: no file of the engine is touched)); the page draws the record as **one box** and hides what is in it until a person opens it.

- **The record.** `ModuleInstance { node, name, members }` in `SessionModel::modules`, in the order they were made: a node of its own (`NodePath::Module(index)`, which `nodes()` lists after the modulators), the module's name in the library and the nodes that came with it: **the lanes** (a lane's effects are in the lane and are not listed) and **the modulators** that the module made, the lanes first, each in the order of the file. `AddFreeEffect { plugin: "module:<name>" }` makes it **last** (`make_module`), under a node that is allocated after every lane, effect, cable and modulator of the module, so that what the add made before it has the ids it had (the older fixtures and the oracle's scenarios that add modules are the same bits). A node is in at most one module (`SessionModel::module_of`; `check_ids` holds it, and that a record has a member and that every member is a lane or a modulator that is there). A module that is **saved** (`SaveModule`, `SaveSelectionAsModule`) is no record: the library's files hold no instance, and a module node is refused as a thing to save (`a module that was added is not a plugin: choose the plugins and modulators in it`).
- **It goes with its members, and they leave it.** A lane or a modulator that is taken out leaves its module (`SessionModel::leave_module`, called by `remove_lane` and `remove_modulator`; written down as `Inverse::Module { index, module }` **before** the member's own inverse, so that Undo puts the member back first and the record after it, as it was: `insert_module_at` puts it at its place, and replaces one that is there under the node), and a record with no member left is gone. `RemoveNode` of the module's node (`Controller::remove_module`) takes out **all that came with it as one step**, "remove Room": the record first, so that it comes back last, then its members, the last made first, each as it is taken out alone (a modulator by `remove_modulator`, a lane by `remove_lane`, with their cables), and it is refused, with nothing changed, no revision used and no step, when the lanes in it are all the lanes the session has (`a session keeps its last lane`). Undo of the add takes the record out with its last member, and Redo makes it again under the same node.
- **A box is no plugin and has no port of its own.** Every intent that names a plugin, a value, a cable or a lane's setting and is given a module's node is refused, with nothing changed, no revision used and nothing sent to the audio thread (`IntentError::Invalid`): mute, solo, route, feed, a gain, a parameter, pins, a preset, a range, an effect added to it, an instrument replaced or cleared, an effect moved, a cable from it or to it (`a module has no output of audio of its own: cables start at the plugins in it`, `a module has no input of audio of its own: cables end at the plugins in it`), a modulator made to hear it or to drive it, a lane put in it as if it were a group, `RenameGroup` and the two saves of a module. What it takes is `SetNodeUi` (a box is placed like any node, and keeps its place through its removal and the Undo of it) and `RemoveNode`.
- **The graph.** `NodeKind::Module { members }` (`{"kind":"module","members":[…]}` on the wire), a node with no parent and no ports, named for the module, listed after the modulators and before the master. The nodes of the members are in the graph as they were, with their ids, parents and edges: the page does the hiding, and the record is told as a snapshot (the structure changes).
- **The file.** A top-level `[[module]]` table for each record (`name`, `members`, and a `ui` for the box's place like any node's), written after the `[[modulator]]` tables and before the `[[cable]]` ones and only when there is one, so that every older file is saved as it was: `members = ["instrument[1]", "modulator[0]"]`, a lane by its place among the `[[instrument]]` tables and a modulator by its place among the `[[modulator]]` tables (`session::MemberPlace`: `instrument[N]` or `modulator[N]`, whole numbers). It is read leniently, as `pinned` and `ui` are (a layout is not worth refusing a session for; `app::load::load_modules`): a text that is no place, a place the session has not, and a node that is in a module already (or twice in this one) are left out and logged, and a module with no member left is not made. A name is the text without control characters and the spaces round it, `Module` (`DEFAULT_MODULE_NAME`) if that is nothing. The records are made last, after the sources and the cables, so that a session that has none uses no id for them; a save writes what a load reads (`modules_to_save`), the same record, place and file. The tracer and the examples have none.

  ```toml
  [[module]]
  name = "Room"
  members = ["instrument[1]", "modulator[0]"]
  ui = { x = 40, y = 520 }
  ```
- **The page: a module is one box** (`web/src/app/patch/boxes.ts`, `collapse(graph, names, open)`). *Closed.* The canvas does not draw what is in a module that is closed: `collapse` makes the page's own view of the session (the layout, the cables and the menus read it) in which the lanes, the modulators and the lanes' effects of a closed module are left out and its node is the box; a cable between two nodes that are inside is left out; a cable that has one end inside goes to the box at **a handle that says which port of which node it is** (`memberHandle`, in `handles.ts`: `audioIn@12`, `param-3@40`, `eventsIn@5`); and a modulator that hears a lane inside a box hears the box. *Open.* A module that is open has no box: its nodes are on the canvas as they are, and the lane's card (the first effect of a lane that has none) has a tag over it, "▾ Room" (`ModuleTag.tsx`, named `Close Room`), that closes it. Which modules are open is the viewer's own (`openModules.ts`, `OpenModulesContext`: as the nodes that show a scope are, it is not saved, not sent and not in the history; a module is closed when it is added and when the page is opened, and one that Undo puts back is open or closed as it was, since the set keeps its node), and opening or closing moves the canvas to what appeared (`Reveals`: the first lane that is on the canvas, or the box).
  *The box's ports* (`BoxPort` in `boxes.ts`, drawn by `ModuleBox.tsx`): a row for each port the module has to the outside, inputs on the left and outputs on the right, each named for the node inside (`Filter`, `Math: B`): the inputs of audio that nothing inside feeds and the outputs of audio that nothing inside takes (a link between two nodes inside is no port; the link that the session lists from a lane that has no instrument to the first effect of its chain carries nothing and does not count as a feed), the input of notes of each lane inside that has an instrument that takes notes (`♪ …`), and every port that a cable crosses the box's edge at, whatever kind it is; cables of modulation cross at the top (into the module) and the bottom (out of it, which is shown and is not a place to start a cable: open the box). A box is `44 + rows × 28 + 20 + 16` pixels tall (`nodeHeight` with `rows`, `boxRows`) and says what is in it (`boxDetail`: `3 plugins · 1 modulator`).
  *Cables.* A cable that is dragged to or from a handle of a box is the cable to or from the node inside that the handle stands for: `connection.ts` resolves `handle@node` to the node before it decides the kind of cable (`audioCableOf`, `notesCableOf`, `cableOf`), which inputs take it (`takersOf`, `kindOfDrag`) and what is asked of the editor (`drawCable`), so that the controller's rules judge it as they judge any cable (a loop, a second cable between two nodes, a link that is there) and a refusal is said as ever. A cable is drawn to the box while the module is closed and to the node inside once it is open (`toFlow` draws `collapse`'s list of cables, whose `real` edge is the session's, so that the words of a cable and the button on it are those of the real cable).
  *The layout* (`toFlow.ts`): a box is a strip of its own under the lanes (slot `module`), and a modulator that hears a box is above it as one that hears a lane is above the lane's strip. Names stay numbered over the whole session, so that what is hidden still counts (`Oscillator 1` is the first of two when the second is in a box).
  *Menus and choosing.* The box has an Open button and a × that asks before it takes the module out with all that is in it (`RemoveModuleMenu`: "Keep Room", "Remove Room for good: the 3 plugins · 1 modulator in it go with it"); the right-click menu of a box has Open and Remove, and that of a lane in an open module has Close Room above the lane's own items; **a box that is chosen is what is in it**, so that Save as module… from a chosen box saves the module's lanes, plugins and modulators again (`contentsOf` in `selection.ts`); and a module that the bar or the canvas's menu adds is put where the pointer was as one node (`Editor.addFreeEffect` places the box, `#placed`).
  *The mock engine* does what the controller does (`withModuleRecord` makes the record last under a node of its own, `removeModuleOutcome` takes all that came with it out as one step "remove Room" through the history's `planOf`, a member that goes leaves its module, `leftItsModule`, and the record is listed before the master), pinned to it by `fixtures/moduleBoxes.json`, which `the_web_clients_fixture_of_what_a_module_comes_to_as_one_box_is_current` writes (a module saved, added, placed, taken out whole, put back, taken out again, its members taken out one by one and put back, a second added and the first taken out: every step's events, nodes, cables, telemetry layout and history).
- **Decisions** (D108 of `docs/web-ui.md`; the user may overrule them). (1) The record is the controller's and the file's, so that the box is the same in every front-end and in the saved session; the engine does not know of it and the sound is what it was, bit for bit; the **box is the page's view of the record**. (2) A module that is added is a **copy that is remembered as one thing**, and not a reference: what is in it is edited as any node is, by opening it (D104's copy, kept). (3) The box's ports are **derived from the cables there are**, and not declared in the record (the plan had said the record would hold which ports it shows): an input that nothing inside feeds is a port, so is a port that a cable crosses at, and a person who wants a port opens the box. (4) Open and closed are the viewer's and not the session's, as the scopes are: nothing is saved or told, and a module is closed when it is added. (5) Taking a box out takes out what is in it, as one step, and a session keeps a lane; taking a member out leaves the module, which is gone with its last member. (6) A box has no mute, solo, volume or knobs of its own: it is a way of looking at what is in it, and a person opens it for those. (7) The box is called by the library's name, and no front-end renames it (a box that was collapsed, step 109, is called by what the person typed, and is not renamed either).
- **Held by tests.** Rust 2,791 tests with the `web` feature (2,768 before; 9 of the command line and 32 on the real program as before): `model.rs` 3 (the record is a node that comes after the modulators and is found by its id, and so is the module a node is in; a member that leaves takes itself out and the record as it was puts it back; `check_ids` notices a record with no member, one whose member is no lane or modulator, and a node that is in two), `apply.rs` 9 (a module that is added is one record under a node that is made last and one that is saved makes none; Undo takes the record out with its last member and Redo makes it again under the same node; a box taken out takes its modulators and its lanes as one step that Undo puts back whole; a node that is taken out of a module leaves it and Undo gives each back as the module was; two modules are put back in the order they were added, whether the second is taken out whole or by its last member; a module that has every lane is not taken out; the box is placed as any node is; 23 intents that a box refuses; and the fixture `moduleBoxes.json`), `load.rs` 3 (the record made last under a node of its own, saved as it is loaded, and what a file names that is not there or is in a module already), `session.rs` 4 (the places, the name, the file read and written the same, what is left out), `graph.rs` 1 (the node on the wire). The page 3,214 tests in 141 files (3,130 in 135 before): `boxes.test.ts` 16 (what `collapse` hides, the box's ports and rows, the phantom link, a modulator that hears a box, open and closed), `toFlow.boxes.test.ts` 20 (the layout of a box, its slot, its height, the tag over a lane of an open module, the cables as drawn), `connection.boxes.test.ts` 10, `handles.test.ts` 4, `ModuleBox.test.tsx` 9 (the ports, the Open button, the × and its words), `PatchView.boxes.test.tsx` 11 (the box on the canvas, opening and closing, the canvas moved to what appeared, the right-click menu, taking it out in one step and Undo), and more in the files that were there (`modules.test.ts`, the editor's, the history's, `contextMenus.test.ts`, `selection.test.ts`, `useLayout.test.tsx`); the browser suite 231 (`e2e/boxes.spec.ts` 4, three on the mock and one on the real program: the ports as tall as the layout says, a cable drawn to a box's input and from its output that Undo takes out, a box chosen and its right-click menu and its ×, and the saved `[[module]]` with its place, a page that is opened again, and the sound heard through cables drawn to the box's ports; `e2e/modules.spec.ts` draws through the box's ports now). **Held by mutants** (step 108): 183 mutants of the logic of the step, in six runs, one at a time against the unit tests that should catch them: 42 of the Rust (the record in the model 13, the controller's removals and what leaves a module 9, the journal 1, the intent's refusal and what `make_module` makes 4, the history and the graph 3, the loader 5, the saver 2, the file's places and names 4, the module file's refusal 1) and 141 of the page (`boxes.ts` 43, the layout 28, the connection 11, the handles 5, the selection 5, the menus 9, the editor 2, the layout's state 4, the canvas 6, the box and its tag 13, the mock engine's record and the history 10, and 5 of the stylesheet and the ports' offset, run in the browser). The first run of the Rust caught 38 and let 4 survive, **all four gaps, three tests now** (the place that a module that is not the first has when its last member goes, which the model's test could not tell from 0 because its module was the first, and the controller's Undo that puts it back there: two mutants and a test with two modules; and the words that a cable at a box is refused in, which the test did not read: two mutants and two assertions). The first run of the page caught 124 of 134 (one of them only by the type check) and let 10 survive: **six gaps, each a test now** (a box that has no port is one row tall, which `boxRows` says and no test had a box with none to ask; the room above a box for the modulators that hear it, which only the master's place shows, since the layout moves what overlaps afterwards; a handle that has something that is not a number at its end; the × of a box asks nothing for "Keep", which the test looked at before the request had gone out, a frame after the click, so that a "Keep" that took the box out passed all the same; the focus that goes to the bar's "+ Effect" after a box is taken out; and a first modulator added to a session that has a module record and none, which the mock listed after the record where the controller lists it before), **one dead guard, out of the code** (`input &&` on whether a box's port takes the cable that is dragged, in both of its places: the set of ports that take a cable has inputs only), and **three no fault** (the owner of a modulator read from the whole graph and not from the one that is drawn, which is the same since an ear that names a hidden lane is moved to the box, and the order, and the record first, of the mock's removal of a module: the mock tells the session whole and not the steps between). The numbers that the layout and the stylesheet share (a row of 28 pixels, the line of 20 and the room of 16 under it, the ports' offset) are held on both sides: the unit tests have the layout's, in plain numbers (a constant that is turned is caught), and the browser spec measures the page (the rows' height, and each port level with the middle of its row): of five mutants of the stylesheet and of the offset, two survived the first spec and are caught by the second. The first run was read before the second, and the sources were put back after each (the runners do it).
TODO: a box has no mute, solo or volume of its own and shows no knobs (a person opens it; the right-click menu and the button do): a module-level gain-and-pan built-in, which P5c needs for the groups anyway, would give a box a volume.
TODO: the terminal shows a module's lanes flat (the tree and the Patch tab know no box): `N` adds a module whole and the tree lists its lanes and modulators as it lists any.
TODO: a cable of modulation cannot be started at a closed box (its outputs at the bottom are shown, and a cable is started at the modulator inside: open the box), and a cable of modulation drawn to a closed box ends only at a port that a cable crosses at already; the other knobs are inside it.
TODO: the nodes that an opened module shows are laid out by the rows, and the canvas is moved to them, and not drawn where the box was (a node that is put on one that a person placed goes clear of it); arranging puts them with their cables short.
TODO: a module that is added is a copy that is remembered as one thing: there is no "update from the library" and no reference (P6's library of references), and the box is called by the library's name (or by what a person typed, for one that was collapsed: step 109) until a person edits the file.
TODO: the keyboard's menus that draw a cable (`SendMenu`, `NotesFromMenu`) are not on a box's header: a person at a keyboard opens the box, and the box's right-click menu has no items that send.
TODO: a name that a hidden node shares with a visible one is numbered over the whole session (`Oscillator 1` is the first of two when the second is in a box): a number that counts what is on the canvas would be 1 for the one that is seen.

### Collapsing what is chosen into one module where it is (`CollapseIntoModule`, `UngroupModule`, step 109)

The user's "I think one should be able to compact a selection of instuments etc into a module, not only save it but to compact/collapse them into a module in place if you understand what I mean? to clean up the ui a bit. Then in the future we can add more options to a module-box (i.e expose internal parameters maybe or similar)" (2026-11-03, after step 108). Step 108 drew a module that was **added** as one box, from a record of what one add made. **Step 109 makes that record from what is already in the session**: the nodes that a person chooses on the canvas become one box, where they were, which opens into them where they were and can be taken apart again. Nothing is added to the session but the record, no command goes to the audio thread (the engine runs the plugins flat, as it did: the sound is what it was, bit for bit, and no file of the engine is touched: the oracle's renders that `cargo test` holds to the stored hashes are the same bits), and the file is the file of step 108 (`[[module]]`: `name`, `members`, `ui`). Exposing a module's internal parameters on its box is the user's next idea and is not built (see the TODOs).

- **The intents.** `CollapseIntoModule { nodes, name, ui }` (`{"type":"collapseIntoModule","nodes":[2,5],"name":"Room","ui":{"x":40.0,"y":120.5}}`; `ui` is null for no place) and `UngroupModule { node }` (`{"type":"ungroupModule","node":9}`). Collapsing is worked out by `Controller::collapse_into_module_intent` in this order, and **a refusal changes nothing, uses no id and leaves no step**: (1) every node is looked for (the first that is not there is not found; the master is no node of the model); (2) what each is: **the lane is the unit**, so that an instrument of a lane, an effect of its chain and the lane's node are all the lane, which goes into the module whole (a lane that has one plugin chosen is in it with the instrument and every effect it has), a modulator is itself, and a group or an effect on a group's bus (`a group is not collapsed into a module: a module holds lanes and modulators`), a source of notes (`a source of notes is not collapsed into a module: …`) and a module's own node (`a module cannot hold another module yet: ungroup it first`) are refused; (3) that something is chosen (`nothing is chosen: choose the lanes, plugins and modulators to collapse into a module`); (4) that none of what it comes to is in a module already (`the lane of Oscillator is in the module Room already: ungroup it first`, `an LFO is in the module …`: `member_words`, in the words of a step); (5) the name (`module_title`: the spaces round it go, at most `MAX_NAME` (64) characters, no control character, as a group's name is, and **a name of nothing is `Module`** (`session::DEFAULT_MODULE_NAME`)); (6) the place (`NodeUi::tidy`: a number that is not finite is refused, `(NaN, 1) is not a place`, and one that is finite is held to 100 000 either way and a tenth of a pixel, as any node's is). Then the record is made **last**, under a node of its own (so that a refusal uses no id and no other node has another id), with the lanes in the order of the session and then the modulators in the order of their rack, whatever the order they were chosen in, and the place in the model's table of places (`SessionModel::ui`). `UngroupModule` takes the record away and touches nothing it holds (`only a module can be ungrouped` for any other node).
- **The history.** Collapsing is an add, in the words `collapse into Room` (`Plan::Added`, whose `takes_out` is the one intent `UngroupModule { node }`: `made.nodes` is the record's node alone), so that Undo takes the box away and **leaves the nodes where they were**, and Redo makes the record again under the same node, with its place (the place is **in the intent**, so that collapsing and placing the box are one step: step 107's way of putting what a menu adds, an add and then a move, is two). Ungrouping is a removal in the words `ungroup Room` (`Plan::Removed`, `Inverse::Module { index, module }` as for a member that leaves, step 108): Undo puts the record back at its place among the modules, under its node, with its place. `RemoveNode` of a box that was collapsed is step 108's: it takes out all that is in it, as one step.
- **The page.** *What it takes, as the controller works it out* (`collapsibleOf` in `app/patch/selection.ts`, pure, held to the controller's rules by the same refusals in its tests and in `fixtures/moduleCollapse.json`): `nodes` (those of the chosen that a module can take, each once, in the order they were chosen in), `lanes`, `modulators`, `brought` (the plugins of those lanes that were not chosen and come along), `left` (a box, a node that is in a module already, a source of notes, a group and its bus, the master: counted and left out) and `hides` (every node that is in the module once it is made: the lanes, the effects in them and the modulators), with `collapseWords` (`1 lane and 2 modulators`), `collapseWhyNot` (`Choose the nodes to collapse into a module`, or `None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module`) and `collapseTitle` (what the button's title says). *The bar* has **Collapse…** (`CollapseSelectionButton` in `SaveModule.tsx`, next to Save as module…, named `Collapse the chosen nodes into a module`): it cannot be pressed with nothing that a module can take and says why in its title, and says what it takes when it can (`Collapse 1 lane and 1 modulator into one box where they are, with 2 more plugins from their lanes: open it to see them again, or ungroup it`); it opens the field of the save, which is the same component with other words (`NameOptions`: the button is `Collapse`, the field starts as `Module` and selected so that a letter replaces it and Enter keeps it, and says that the name is what the box is called and not a file), and Enter asks `Editor.collapseIntoModule(nodes, name, place)`, which resolves with the id of the box or null if it was refused (the page says why, among the notices, and the field stays). Choosing stays as it was and the canvas is not moved: the box is where the nodes were. *The place of the box* is the top left corner of the nodes it hides, where they are on the canvas (`collapsePlace`: the nodes that are not drawn, such as a lane that has no card, are not asked for; none if none is). *The canvas's right-click menu of what is chosen* has **Collapse the 1 lane and 1 modulator into a module…** above Choose none (`collapse-chosen`: the field opens where the pointer was, and the box is where the nodes were and not where the pointer was; shut with the reason as its title when nothing can be taken). *Taking a box apart*: the menu of its × is Keep, **Ungroup Room: keep the 2 plugins · 1 modulator, take the box away** and Remove for good (`RemoveModuleMenu`), a right click on a box has Ungroup (`moduleMenu`), and a right click on a lane of a module that is open has **Ungroup Room** under Close Room (`nodeMenu`); the focus goes to the bar's "+ Effect" when the box is gone, as it does for a removal. *The nodes go back where they were drawn*: the layout used to forget the place of a node that is not on the canvas, so that a module that was opened, or ungrouped, laid its nodes out again by the rows; `toFlow` keeps the places that it remembered for the nodes that a closed module hides (`places.set(node.id, before)`, except for a node that a person placed, which has its own in the session, and for a node that is drawn), so that Open, Ungroup and Undo give each node back the place it had. *The mock engine* does what the controller does (`collapseIntoModuleOutcome` with its refusals in the controller's order and words, `ungroupModuleOutcome`, `memberWords`, `tidyName` and `boxTitle`, shared with `RenameGroup`; `withModuleRecord` takes the place; the history's `planOf` and `takesOut`), pinned to it by `fixtures/moduleCollapse.json`, which `the_web_clients_fixture_of_what_collapsing_into_a_module_comes_to_is_current` writes (13 steps: the filter and the LFO collapsed as `Room` with a place, undone and redone, ungrouped and undone, the lane of a reverb collapsed under a name of nothing, the empty lane and a macro collapsed, a box ungrouped and undone, taken out whole and undone and redone and undone; and 19 refusals, each tried at the step of the script where it was meant to be, with the controller's words: every step's events, nodes, cables, layout of the telemetry and history).
- **Decisions** (D109 of `docs/web-ui.md`; the user may overrule them). (1) **The lane is the unit**: a lane that has any of its plugins chosen is in the module whole, with the plugins that were not chosen counted and said (the record of step 108 is of lanes and modulators, a lane's effects are in the lane, and a module that took half of a lane's chain would need the lane to be dissolved, which is P5c). (2) **A module cannot hold another module, and two are not merged**: a box, or a node that is in a module already, is refused in words (`ungroup it first`), since a node is in at most one module (step 108's check); nesting is a decision for when a box gets parameters of its own. (3) **Ungroup is mine, not asked for**: the inverse of collapsing, so that a person can take a box apart without taking out what is in it (the × menu, the right-click menus); remove the items to overrule it, and the intent stays for Undo. (4) **A name is asked for, and starts as `Module`**, so that Enter alone collapses; the name is only what the box is called, with a group's rule, and there is no rename yet. (5) **The place is in the intent**: one step, not two. (6) **The box is where the nodes were** (the top left of what it hides) and the nodes go back where they were drawn when it is opened or taken apart. (7) **Choosing and the view are left alone** by a collapse. (8) **The record only**: no command for the audio thread, no new file format, no node of the engine: the sound is what it was.
- **Held by tests.** Rust 2,801 tests with the `web` feature (2,791 at step 108, and one test that came with `main`'s `c` popup; 9 of the command line and 32 on the real program as before): `apply.rs` 9 (nodes that are chosen collapsed in one step that Undo takes away and Redo makes again under the same node, with nothing sent to the engine; a lane is in the module whole whatever of it is chosen, and the members are in the order of the session and then of the rack; what cannot be collapsed is refused in words with nothing changed, no id used and no step; a name of nothing, a box with no place and one that has a place; ungrouping, its Undo at its place among the modules and its Redo; a node that is no module is refused; a box that was collapsed is taken out whole in one step and a member that goes leaves it; a box that was collapsed is saved with its members by their places and its own and is loaded as it was, and one that was ungrouped is not in the file; and the fixture `moduleCollapse.json`), `intent.rs` (the wire of the two intents). The page 3,248 tests in 141 files (3,214 in 141 before): `selection.test.ts` 12 (what collapsing takes: an effect is its lane, a lane that only holds effects, a modulator, what a module cannot take and what is in a module already, no session; the words, the reasons and the title; the place of the box), `modules.test.ts` 4 (the mock engine held to `moduleCollapse.json` step by step, its refusals at the place where they were tried, a place that is not a place, the name tidied), `contextMenus.test.ts` 4, `toFlow.boxes.test.ts` 6 (the places of what a closed module hides, of a node that is drawn and of a lane that has no card), `PatchView.selection.test.tsx` 6 (the field, the request, the places, the button's reasons and title, and a refusal), `PatchView.menus.test.tsx` 1 (the field that the canvas's menu opens) and `PatchView.boxes.test.tsx` 2 (ungrouping from the × and from the right click); the browser suite 235 (231 before): `e2e/collapse.spec.ts` 4, three on the mock (the box at the top left of where the nodes were, opened and closed, Undo and Redo, ungrouped; the canvas's menu with the field where the pointer was; the open lane's menu and the button that cannot be pressed) and one on the real program (the saved `[[module]]` with its members and the place of the box, a page opened again, ungrouping takes the record out of the file), and `e2e/boxes.spec.ts` reads the menu that has the new item. **Held by mutants** (step 109): 112 mutants of the logic of the step, one at a time against the unit tests that should catch them: 23 of the Rust (what collapsing takes and refuses 12, ungrouping 2, the name 4, the words of a refusal 1, the history 4) and 89 of the page (`selection.ts` and the button's title 29, the mock engine 15, the history and the names 7, the editor 3, `toFlow`'s memory of the places of hidden nodes 4, the menus 12, the canvas 5, the field and the button 11, the box 3). The Rust caught 22 and let 1 survive, **no fault** (the plan of an `UngroupModule` of a node that is no module: the intent is refused before the plan is looked at, as the arm for `RemoveNode` is). The page's first run, of 80, caught 76 (one of them only by the type check) and let 4 survive: **five gaps, each a test now** (a node that is drawn has the place that it is drawn at and not what was remembered of it for another slot, which only `tsc` held, since the variable would have been unused; a lane that has no card keeps no place; the words of the field that the canvas's menu opens, which the bar's button had a test of and the menu had none; what the field says it is for; and the plugins that come along in the button's title, where a mutant that said the number of lanes for the number of plugins survived the first test because the tracer's lane has one of each: the wording is a function of its own now, `collapseTitle`, with a unit test of other numbers). The 9 mutants that were written for what the gaps changed and the 5 that survived are caught.
- **Found while building it.** (1) **The fixture's refusals need to say when they are tried**: a refusal of a name, tried after the first collapse, would have hit `is in a module already` before it looked at the name, which is the order of the controller's checks and not the case that was meant: each refusal carries the step of the script it follows (`after`), and the two of the name are tried at the start. (2) **`NodeUi::tidy` holds a place, it does not refuse a big one** (only a number that is not finite is refused): the tests and the mock say so. (3) **A removal that is undone needs its engine handles drained first**: the stub engine of a Rust test has to be read (`sent(&engine)`) before an Undo of a lane's removal, or the lane is not handed back (`was not handed back by the audio thread`). (4) **`build.rs` bakes `env!("CARGO_MANIFEST_DIR")` into the build script's own binary, so a build script that was compiled in a git worktree is reused by a build in another tree** (cargo's fingerprint does not see the path) and the linker looks for `Info.plist` in the worktree: `touch build.rs` makes cargo build it again; reading the variable at run time (`std::env::var`) would not have the fault. (5) **The layout forgot the places of the nodes that a closed module hides**, so that Open laid them out again by the rows and not where they were drawn: the same carry-forward that keeps a node where it was while a cable is drawn, extended to the nodes that are not on the canvas.
TODO: a box cannot be renamed (the name is what it was collapsed under, or the library's for a module that was added; `RenameGroup`'s rule would do, a ✎ on the box's header as a group's name has), and the name has no check that it differs from another box's.
TODO: a module cannot hold another module, nor take a node that is in one (`ungroup it first`), and two boxes cannot be merged: a record of modules in modules is a decision for when a box has parameters of its own.
TODO: **the box has no parameters of its own, which is the user's next idea for it** ("expose internal parameters maybe or similar"): a box could carry a list of the knobs of what is in it (a node of the record, `members` and `exposed`, drawn as knobs on the box and driven as the knobs inside are), with the volume, mute and solo that it has none of; it needs the model to say which parameter of which node a knob stands for (`Driven` is already that), the file to keep the list, and the page to draw the knobs on a closed box: not built.
TODO: collapsing keeps a lane whole, so the chain of a lane cannot be split between two boxes (a lane that is chosen by one effect is in the box with its instrument and the effects that were not chosen, which the button's title says): P5c, which dissolves the lane, would let a module be any plugins and cables, and the record's members be nodes.
TODO: the box that was made is not chosen, and the canvas does not move to show it (it is where the nodes were: a person who had the view somewhere else sees it when they look); and there is no key for Collapse (the bar's button and the menu are the ways), and no Collapse in the terminal, whose tree has no box.
TODO: the places of the nodes that a closed module hides are the page's own (`toFlow`'s memory of them): a page that is opened again with the module closed lays out the nodes inside by the rows when it is opened, since it never drew them where they were (the box's place is in the file, and the nodes' places are the layout's, as every node's is that no person placed).

### Audio cables (`[[cable]]`)

Besides the way that the audio goes by itself (down a lane's chain and into the lane's group or the mix, down a group's bus and into the mix), a session can have **audio cables**: sends from one place of
the audio to another (P3c-1 of `docs/patch-graph.md` §11.6, step 75). Two oscillators into one reverb is two cables that end at the same effect.

```toml
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
mix = 1.0               # all wet: it puts out its tail only

[[instrument]]          # instrument[0]
plugin = "builtin:osc"

[[instrument]]          # instrument[1]
plugin = "builtin:osc"

[[cable]]
from = "instrument[0]"      # what the lane's instrument puts out, after its volume and before its chain
to = "group[0].effect[0]"   # the reverb on the group's bus: added to what its chain gives it
gain = 0.6                  # optional, 1.0 if none; held to 0 to 2

[[cable]]
from = "instrument[1]"
to = "group[0].effect[0]"
gain = 0.4
```

- A place is `instrument[N]` (the Nth `[[instrument]]`), `instrument[N].effect[M]` (the Mth effect of its chain), `group[N]`, `group[N].effect[M]` (the Mth effect of its bus), either of those two with `.side` after it (the second input of an effect that has one: see "An effect with a second input") or `master`; the numbers count from
  0 in the order of the file. `CablePlace` (`src/session.rs`) is the one rule for reading and writing them (a text that is none is refused with the cable's place: `cable[0].from: "…" is not a place of the session`).
- **A cable starts** at a lane (`instrument[N]`: what its instrument puts out after the lane's volume, before its chain), at an effect (`….effect[M]`: after its mix, a lane's or a bus's) or at a group (`group[N]`:
  after its effects, its pan and its fade), and never at `master`. **It ends** at an effect (what is put in is *added* to what its chain gives it), at a group (`group[N]`: its bus, added to its members) or at
  `master` (the mix), and never at a lane, which has no input; an effect that has a second input is ended at either (`.side` says which). In the engine they are `CableFrom::{Instrument, Effect, Group}` and `CableTo::{Effect, EffectSide, GroupBus, Master}`, named by the model's ids (see "The engine
  on the kernel": a cable is a node of the plan).
- **Heard at what its owner is heard at**: a cable that starts at a lane's instrument goes with what the lane is heard for by itself, one that starts at an effect with what its lane or group is heard at either way (what
  is sent to it too), and the output of a group has been let in or out already, so it is not gated twice. A muted lane sends nothing; a lane in a muted group keeps its sends (the group's mute cuts the bus). A cable
  carries into a lane or a group that is not soloed while something is, when it comes from something that is heard (P3c-2b, step 78: see "Mute and solo": the lane is heard for what it is sent, and none of its
  own sound goes into its chain). A cable comes in over one buffer from nothing, goes down over one and is turned over one, with no click.
- **The ids** are those of the modulation cables' series (`SessionModel::alloc_edge`), given in the order of the file after the modulation cables of the file; the cables are kept in the order of their ids
  (`SessionModel.cables`) and written after the `[[group]]` tables in that order, `gain` only when it is not 1, so that a session that has none is the file it was (`app::save::cables_to_save`). A level is held to
  `0 ..= CABLE_GAIN_MAX` (2) when it is loaded, as the intent holds it.
- **What refuses the session** (with the cable's place in the file, `cable[1].to: …`; `app::load::load_cables`): a place that is not in the session (`instrument[5]`), a start that is `master` or an end that is a lane, a
  cable between two places that another cable is between already, and a cable that would make the audio go round (a loop: the wiring is compiled on the side with it, as the audio thread does when it is sent one,
  `SessionModel::cable_problem`). A cable that is left out would change the sound without a word.
- **What keeps the audio from going round** once the cables are there: putting a lane in a group (`SetLaneGroup`) and moving an effect (`MoveEffect`) are refused when the new wiring, with the cables there are, does
  not compile (`SessionModel::loop_if_lane_goes_to`, `loop_if_effect_goes_to`; the controller's `set_lane_group`, `move_effect` and `move_bus_effect` return false for one, and the intents say why: "… would make the
  audio go round, by the cables there are"). Nothing else changes the wiring in a way that can close a loop: an effect that is added goes last in its chain with no cable of its own, and what is taken out takes its
  cables with it.
- The intents are `AddCable { from, to, gain }` (the ports of the graph: an instrument's, an effect's or a group's `AudioOut` to an effect's, a group's or `NodeId::MASTER`'s `AudioIn`; a gain of none is 1, and the
  level is held to 0 to 2; refused, with nothing changed and no id used, for what the file refuses and for a level that is not a number; a cable at a level of none or 1 between the two ends of a link of the audio path is that link, given back where it is cut and refused where it is there: see "Links are cables") and `SetCableGain { edge, gain }` (told as `CableGainSet`, which a `Snapshot`
  applies), and `RemoveEdge` takes an audio cable out as it does a modulation cable (`SetEdgeDepth` of an audio cable and `SetCableGain` of a modulation cable are refused, in words that say which has what). The
  graph a client gets has a cable as an edge `kind: "cable"` with its `id` and `gain`, from the output's port to the input's, after the links of the audio path (`audio` and `member`, which are derived and stay as
  they are) and before the modulation cables.
- **Undo** (`docs/web-ui.md` step 75): drawing a cable is a step ("draw an audio cable") that undo takes out and redo draws again under its id and at its level; its level is a step that a drag makes one of ("set the
  level of an audio cable", `Key::Level`); taking one out is a step that undo puts back as it was ("remove an audio cable", `Inverse::AudioCable`); the cables at a lane (its instrument and its effects), a group (its
  bus, its output and its bus effects) or an effect that is taken out go first, each written down, and come back after the node, whole, under their ids.
- **The oracle** has the cables: `Engine::facts` gives the engine's cables (`Facts`, `CableFacts`) and the oracle holds them to the model's after every block, the corpus has two sessions with cables (`sends`: two
  oscillators into one reverb, `patched`: a cable of every kind of start and end) and a scenario that edits them while it plays (`edited:cables`: `Edit::{AddCable, CableGain, RemoveCable}`, a lane taken out and put back
  by Undo with its cables; and `edited:solos`, the solos of lanes and groups that cables carry between, step 78); the loop that the plan replaced (`process_legacy`) has no cables, so a scenario that has some is not held to it (`Scenario::has_cables`).
- **The terminal draws them** on the Patch tab (P3e, step 79: a matrix of outputs against inputs, see "Patch tab"). **The page draws them** (P3d, step 76, `docs/web-ui.md` §9.3): each
  audio cable of the session is an edge `send:<id>` (`SendEdge`, `app/patch/edges.tsx`), amber (`--tang-send`) and as thick as it is loud (`sendWidth`: 2 to 4 pixels for a level of 0 to 2), over the links of the audio path and under the cables of
  modulation, with a slider for its level at its middle (`CableLevelControl`: `Level of Oscillator 2 → Reverb`, 0 to 200%, a hundredth a step, turned as a depth is, `Drive.cableGain`) and the button that takes it out beside it (the focus goes to the menu of
  the node it started at). Every lane's instrument, effect and group has an output of audio on its right edge and every effect, group and the master an input on its left (`AudioPort`: a handle that starts a cable or ends one and not both; a lane has no
  input); a cable is dragged from an output to an input, the inputs that take it grow to a strip along their node's left edge while it is dragged, and its line is amber and plain where a cable of modulation's is magenta and dashed
  (`takersOf` and `canConnect` in `connection.ts`: the controller's rule, `cableTakers`, read from the graph; `data-connecting="audio"` on the canvas, which the stylesheet draws the line by). For a keyboard, every node that sends has a menu on the
  badge's line of its header (`SendMenu`, `⇢`: `Send audio from Oscillator 2`, the places that take a cable from it under Effects, Group buses and Mix, `client/sends.ts`; the canvas hands the menus the structure of the session once, `StructureContext`,
  so that a value that is turned does not make each of them compare the graph). The cables are left out of the layout's idea of what changed (`toFlow`, `sameStructure`: a level is a value). **The page's mock engine** does what the controller does with them (step 76): the intents `addCable`,
  `setCableGain` and `removeEdge` of an audio cable, in the controller's words and order of refusals, the cables that go with a node that is taken out, the loops refused in `setLaneGroup` and `moveEffect`, and the
  history ("draw an audio cable", "set the level of an audio cable", "remove an audio cable"). `web/src/client/wiring.ts` is the plan's wiring read from the graph a client has (`wiringOf`, the head and the tail of
  a lane and of a group as the stages of `build_patch`; `cableEnds`, `cableLoops`, `laneLoops`, `effectLoops`, and `cableTakers`: the nodes that a cable from an output can be drawn to now, for the canvas), pinned to
  the controller by `fixtures/audioCables.json` (every start tried with every end, the levels, the removals, every lane put in the group, every effect moved: 268 rows; `testutil::PATCHED` is its session) and by
  the audio cable scripts of `fixtures/history.json`. The ids of both kinds of cable are one series in the mock's `Counters`, as in the model.
TODO: a chain's links are not cables (they have no id and no level: they are the template's, and an effect is taken out or moved, or the link into it is cut, to change them: step 86, "The input of an effect"); the way out of a lane and a group can be cut (`route = "none"`, see
"The way out") and what a cable carries into a lane that is not heard is heard (see "Mute and solo").
TODO: a cable has no meter (a light that shows what a cable carries). (Its latency is no longer one: what is added where a cable ends is put in step by the latencies of the plugins, see "Latency".)
TODO: the slider of an audio cable's level is at the middle of the cable, which can be over a node (the cable is drawn over the nodes it passes, as a cable of modulation is), and two cables with one middle put theirs over each other;
a cable that is beside the link of the audio path that it is a second one of (a lane's instrument to its own first effect: since step 88a only a file, or an intent at a level other than 1, can make one) runs along it.

### Links are cables (`link_or_cable`, steps 88a and 88b)

The audio links that a lane and a group make by themselves (a lane's instrument to its first effect, an effect to the next, the end of a chain to its group's bus or to the mix, a group's head to its first bus effect) are the template's, and a client drew them as links and the cables a person drew as cables:
two things. The user's direction of 2026-10-23 ("I dont think we should have the "no master" tracking per instrument/fx. if it has a cable (or more cables) it sends to those and if there is no cable than thats ok") makes them one: **every audio link is a cable**, and a node that has none sends
nowhere. Step 88a (P5.0 of `docs/patch-graph.md` §11.6) builds it **at the level of what a client asks and is told** and leaves the engine, the oracle and the file as they were: a link is still the template's (the model has `route` and `feed`, the file `route = "none"` and `feed = "none"`, the engine
`SetLaneRoute` and `SetEffectFeed`: see "The way out" and "The input of an effect"), and a cable drawn at its two ends *is* the link and not a second cable beside it. A link that is a cable with an id and a level, and a lane that is a template of nodes and cables and nothing in the model, is P5c and is not built. Step 88b is the page's picture of it (see the last bullet).

- **The rule** (`Controller::link_or_cable`, which `apply` runs before the history or the engine see an intent; the page's is `linkOrCable` in `client/wiring.ts`, which the mock engine runs): an `AddCable` between the two ends of a link, at a level of none or exactly 1, is
  - **the link given back** when the link is cut: the intent is made `SetRoute { node, route: Out }` or `SetFeed { node, feed: Chain }` and carried out as that, so that it is a step of the history in the words of the link (`give the lane of Oscillator its way out`, `give Reverb its input back`; Undo cuts it again and
    Redo gives it back) and is refused where giving the link back would make the audio go round, by the link's rule (`loop_if_route_is`, `loop_if_feed_is`) and not by a cable's: a group's link into its first bus effect starts at the group's head, which as a cable would always be a loop
    (`a_cable_drawn_between_the_ends_of_a_link_that_is_cut_gives_the_link_back_and_makes_no_cable`, `a_link_that_a_cable_gives_back_is_refused_where_the_audio_would_go_round_as_the_intents_are`);
  - **refused** where the link is there (`there is a cable between those two already`, the words of a second cable between two places: `a_cable_drawn_between_the_ends_of_a_link_that_is_there_is_refused_as_one_between_the_two_is`);
  - and every other `AddCable` is judged as a cable as before: a level other than 1 (a cable beside the link, at a level of its own), a cable that is there between the two already (a file's), an end that the controller does not take, the ports of events
    (`a_cable_at_another_level_than_the_links_is_a_cable_beside_the_link`, `an_end_that_the_controller_does_not_take_is_judged_as_a_cable_in_the_cables_words_whether_or_not_a_link_joins_the_nodes`).
  The file is not read by the rule: a session that has a cable beside a link loads as it did.
- **The ends of a link** are `SessionModel::template_links()` (`TemplateLink { of: LinkOf::WayOut(node) | Feed(node), from, to, present }`, the lanes' and then the groups'; `template_link(from, to)` looks one up): a lane's way out goes from the last effect of its chain (the instrument of a lane that has none) to the bus of its
  group, or to the mix when it has none or its group is gone; an effect's feed from the stage before it. A link that is cut is listed with `present: false`, so that the cable that gives it back can be told from one that is not a link.
- **A link has no level and no id** (a cable has both): a client draws it with neither and cuts it as it did (`SetRoute`, `SetFeed`), and what a cable has that a link has not, a fade over a buffer when it comes in or goes, a link has not either: it is cut and given back at once (see the TODOs of "The way out" and "The input of an effect").
- **A module that stands by itself is bare**: `AddFreeEffect` makes its lane with the way out cut (see "An effect that stands by itself"), so that it sends nowhere until a cable is drawn from it; a cable from it to the mix is the link given back, and it is heard at the master.
- **The terminal** draws every link at its crosspoint on the Patch tab (see "Patch tab"): a `●` where it is there, a dot where it is cut and can be given back, a cross where giving it back would make the audio go round. `Enter` cuts a link that is there and gives back one that is cut (the `AddCable` above), `d` cuts, and `+`, `-` and `e` say that a link has no level.
- **The page** (steps 88a and 88b). *The logic* (88a): `cableTakers` (`client/wiring.ts`, which the canvas's strips, the send menus and the drop of a dragged cable read) leaves out the end of a link that is there and takes in the end of one that is cut, unless giving it back would make the audio go round
  (`routeLoops`, `feedLoops`); the mock engine runs `linkOrCable` and is pinned to the controller by the `cutRows` of `fixtures/audioCables.json` (every start tried with every end on a session whose every link is cut: 169 rows) and by the refusals among its older rows, and the page's takers by the same rows.
  *The picture* (88b): every link is drawn as a cable like the ones a person draws, amber and 3 pixels (`.tang-cable--audio`, in the colour of a send at level 1, which a link is), with **no slider** (it has no level) and a **×** at its middle named `Remove cable A → B`, as every cable's is (`RemoveLinkButton` in `AudioEdge`,
  `app/patch/edges.tsx`). A press cuts the link by the intent of before (`SetRoute none` for the end of a chain, `SetFeed none` for a link into an effect: the history keeps the link's words, `cut the way out of the lane of Oscillator`, `cut the input of Filter`) and puts the focus on the send menu of the node the link started at
  (`Send audio from A`: `SendMenu`, `⇢`), which offers the end it joined again, since a cable between the two ends of a cut link is the link given back. The buttons that said where a node's audio goes and where an effect is fed from (`WayOutButton`, `→ Master` or `no out`, in the "Mute / Solo" cell; `FeedButton`,
  `← Oscillator` or `no in`, on the badge's line of an effect; with `wayOutOf`, `inputOf`, `.tang-way-out` and `.tang-feed`) are gone: **a node that has no cable sends nowhere, and nothing on it says so**. The "Mute / Solo" cell is the mute and the solo, and the node's send menu is how a cable is drawn from the keyboard
  (it is disabled where it has nothing to offer). The browser specs of the routes, the feeds, the free effect, the groups, the instruments and the solo are written for it (a link is a `.tang-cable--audio`, its button `Remove cable A → B`, and a cut link is given back by a drag or the send menu).
- **Held by mutants** (after step 88b): 79 mutants of the logic of 88a and 88b (the controller's `link_or_cable`, the model's `template_links` and `template_link`, the bare free effect, the Patch tab's `Cell::Link`, its keys and its refusals; the page's `templateLinks`, `linkLoops`, `linkOrCable`, `cableTakers`, `RemoveLinkButton`, `AudioEdge` and the mock engine's run of the rule): 75 were caught by the tests that were there, and the 4 that were not were one gap, a cut link offered (the page's menus: `cableTakers`) or shown as one that can be given back (the Patch tab) where giving it back would make the audio go round, for an effect's input as well as for a way out: a test each now (`wiring.test.ts`, "do not have a link that is cut where giving it back would make the audio go round…", and the Patch tab's "the input of an effect that is cut is refused…"). One more was caught only because the function it left alone was no longer read (`noUnusedLocals`): a variant that compiles is caught by the same test.
TODO: a cable beside a link (a second path between the two ends of a link that is there) can no longer be drawn from the page or the terminal, which draw at a level of 1; a client that sends the intent at another level, or a file, can make one (the cable runs along the link and the two are added where they end).
TODO: the history's words for a link that a cable gives back, or that a × cuts, are the link's (`give the lane of Oscillator its way out`, `cut the input of Filter`) and not a cable's (`draw an audio cable`, `remove an audio cable`), though the button says `Remove cable …`: a client that draws or removes a link is told of another step than the one it thinks it made; they are a cable's when P5c makes a link a cable with an id.
TODO: a node that no cable leaves says nothing (the `no out` and `no in` buttons that did are gone): a dim output port or a faint mark on the node would say it, and what a session of nodes with no cables looks like to someone who loads it is not designed.

### An effect with a second input (`side`, step 95, the small half of P3g)

An effect can have **two inputs of audio**: the user's idea of 2026-10-27, after step 94 ("can we make a FX that takes one or two audio cables as input and then do math on the inputs to create a new output?"), and the small half of
P3g of `docs/patch-graph.md` §11.6 (the ports that plugins have). **A** is what an effect's one input always was (what its chain feeds it, and the cables that end at it); **B** is a second input that **only cables end at**. Only the built-in
Math (`builtin:math`: see "Built-in plugins") has one; every other effect, a CLAP, VST3 or LV2 plugin included, has one input as before (a sidechain input of such a plugin still gets silence: see "Plugin I/O handling").

- **The plugin says it.** `Plugin::side_input() -> Option<SideInput { main, side }>` (none by default; the names of the two inputs, `"A"` and `"B"`) and `Plugin::process_side(midi, audio_in, side, audio_out)` (the default calls `process` and leaves `side` out).
  `side` is `None` while **no cable is patched to B**, and the plugin then makes a B of its own (the Math's carrier, so that a module that has one cable already sounds of something), and else it is what the cables at B bring: their sum, channel by channel,
  silence where a cable has not come in yet. `tang describe` says it (`Audio inputs:  two: A (what the chain feeds) and B (cables only)`).
- **The engine** (`src/plugin/chain/nodes.rs`). `CableTo::EffectSide(NodeId)` is a cable that ends at B. The effect's node in the plan (`Stage::Fx(id)`) has a **second input port** (`PortSpec::single`, as the first is: one cable is the buffer of its sender, two or
  more are a sum) only when the wiring says that its plugin has a second input (`LaneWiring::sides`, `GroupWiring::sides`, from `PluginSlot::side`), and `CableTo::port()` is 1 for `EffectSide` and 0 for every other end. The `Shape` that says when the plan is made again has a word
  for such an effect (`SIDE`) and one for a cable that ends at B (`TO_EFFECT_SIDE`), written for those only, so that a session that has none has the shape it had. `Schedule::sided` (the effects that a cable is patched to B of, worked out in `compile` from the cables that are in the
  plan) tells `run_effect` whether to give the plugin its B or none; a cable that is going down still counts as patched for the buffer it goes down in, so that B does not jump to the carrier a buffer early. The kernel needed nothing (the inputs of a node are put in step together, so that B is in step with A:
  see "Latency"; an input with no cable is silence), and neither does the law of mute and solo for what cables carry (see "Mute and solo": a cable is heard at what its start is heard at, and what it ends at is heard for what is sent to it, at whichever input).
  Held by `a_cable_that_ends_at_the_second_input_is_what_the_plugin_is_given_there_and_is_added_to_nothing_else`, `a_cable_that_is_taken_out_goes_down_over_a_block_and_then_the_plugin_is_told_that_nothing_is_patched`,
  `the_paths_that_end_at_the_two_inputs_of_an_effect_are_in_step`, `a_muted_sender_sends_nothing_to_the_second_input_and_the_plugin_is_still_told_that_a_cable_is_patched`, and no allocation (`a_block_with_a_second_input_and_a_cable_at_it_asks_the_allocator_for_nothing`).
- **The model, the graph and the file.** `PluginSlot::side` (read from the plugin when it is loaded, kept with the slot: an effect that undo puts back has it, and so has a plugin that is loaded again). `SessionModel::cable_ends` is the controller's rule: the port `AudioInSide` of an effect whose slot has a second input is
  `CableTo::EffectSide`, and of any other node is refused (`that effect has no second input`, `the mix has no second input`, `a group has one input, its bus`); `audio_cables_at` has the cables at either input, so that they go with the effect. The graph has the port `Port::AudioInSide`
  (`{"type":"audioInSide"}`) and says what a node's two inputs are called (`NodeKind::Effect { side: Option<SideInfo { main, side }> }`, left out of the wire when there is none): a cable is the edge from an `audioOut` to an `audioIn` or an `audioInSide`. The file names the input after the effect's
  place, `to = "instrument[1].effect[0].side"` (`group[0].effect[0].side` for a bus effect: `CablePlace::{EffectSide, BusEffectSide}`); a cable that **starts** at a place with `.side` is refused (`is an input: a cable starts at an output of audio`), and so is one that ends at the `.side` of an effect that
  has none (`that effect has no second input`). The saver writes what the loader reads.
- **Cables.** `AddCable` with `Port::AudioInSide`. A cable at B and one at A between the same two nodes are two cables (each only once: `there is a cable between those two already`), a loop through B is refused as every loop is (`that cable would make the audio go round: it would feed what it comes from`: a cable from an effect to its own B is one),
  and a level is a cable's (0 to 2). **B has no link**: it is fed by cables and by nothing else, so there is nothing to cut or give back there (`link_or_cable` leaves a cable at B alone), and a Math that stands by itself is bare as any module is. The cables at either input of an effect that is taken out go with it,
  each written down, and Undo puts them back with it (`Inverse::AudioCable`, `Inverse::Effect`).
- **The terminal.** The Patch tab has a **column for B** after the column of an effect that has one (`↳B` under the tag of its lane or group; `L1 Math (B)` and `G1 Math (bus, B)` in the foot): `Enter` there draws the cable at B (`AddCable` with `Port::AudioInSide`) and takes it out when it is there, a cross says that the audio would go round, as at any input, and there is no link there.
  The Help tab lists it. The Session tab draws no cable (see its TODO).
- **The page.** A node of an effect that has a second input has **two inputs on its left edge**, `audioIn` at a third of the node's height and `audioInSide` at two thirds, with their names beside them (`AudioPort`'s `pair`, `PatchNodeData::side`: A and B); while a cable is dragged each of the inputs that take it is half of the edge. `client/wiring.ts` has the controller's rule (`cableEnds`: the same
  words in the same order, `CableEnd::effectSide`; `audioTakers`, which answers for both inputs; `cableIsBetween`, which tells a cable at B from one at A); `sendItems` lists `Math (A)` and `Math (B)` in the keyboard's menu of every node (the item's id is the node's, and `<id>:side` for B: `sendTarget`); the words of a cable say the input where there are two (`Oscillator → Math: B`: `portName`);
  `Editor.addAudioCable(from, to, gain, side)`. The mock engine does what the controller does (its duplicate check, the effect nodes of its fixtures carry `side`), pinned by `fixtures/sideCables.json` (218 rows of a session with a math effect in a lane and one on a bus and three cables at the lane's B, with a free LFO: every start tried with both inputs of every end, levels, the
  cables' levels and removals, every node taken out, every lane put in the group and out, every effect moved, and a math effect, a lane, a group and effects added) and a script of `history.json` (a cable at B is a step that undo takes out and redo draws again, and the effect is taken out with the cables at both its inputs and put back with them); the shape's `sends` lines say `side` after the nodes of a cable at B (the others are as they were).
  The browser specs are `e2e/math.spec.ts` (on the mock: two inputs, a cable to each, the keyboard's menu, Undo; on the real program: the Math multiplies by its own carrier until a cable is patched to B, a muted lane at B silences it, the cable taken out brings the carrier back, the file keeps `.side`, a page opened again draws it).
- **The oracle.** A corpus session (`math`: a group with a math effect on its bus in `fold`, a saw through a math effect in `ring` whose B has a cable from a sine seven semitones above, which has no way out of its own, and a triangle in the group) and a scenario (`edited:math`, `MATH_EDITS`, 15 edits: a math effect is added in a lane of its own, a cable to its B and one to its A,
  its mode is turned, the lane is given its way out, the sender is muted and soloed, the cable at B is taken out and put back by Undo, the pitch of the carrier is turned, the lane is taken out with its cables and put back whole by Undo, the mode is turned to a logic) make six renders (**1017** in all; the 1011 that were there are the same bits), `Edit::cable_ends` has the
  second input of an effect among the ends, and `two_lanes_are_a_ring_through_the_second_input_of_an_effect_and_with_the_cable_taken_out_the_effects_own_carrier_takes_its_place` renders the session through the real engine.
- **The examples.** `ring-bell.toml` (two sines into A and B), `growl.toml` (a saw into A, B its own carrier) and `pump.toml` (a pad into A, a ghost kick into B): see "Example sessions".
- **Held by mutants** (step 95): 113 mutants of the logic of the second input and of the Math, in two runs (73 of the Rust: the engine's 19, the model's 9, the graph's 2, the loader's 6, the saver's 2, the places of the file 4, the Patch tab's 3 and the Math's arithmetic 28; 40 of the page: `wiring.ts` 14, the drag 5, the keyboard's menu 4, the words 2, the layout 2, the port's handle and its labels 4, the node 2, the mock engine 5, the editor 1 and the fixture's shape 1), one at a time against the unit tests that should catch them: 104 were caught by the tests that were there (none only by the build) and 9 survived, each a gap and a test now. **Eight of the Rust**: the engine's list of the effects that a cable is patched at the second input of is searched by id and has to be in the order of the ids and not of the cables (the first cable drawn going to the later effect: that effect was told that nothing is patched and made its own B); the shape's word for a bus effect that has a second input; the reason that the engine gives for a cable at the second input of an effect that has none (the end is not there, and not that the audio goes round: the wiring with a cable at a port that is not there does not compile either, so the cable was refused all the same, with the other reason); the loader's words for the same cable at a bus effect; the Math's `compare` between its soft and its hard end (the fourth power of the knob); the follower's cap at 1 (a B louder than full scale, which a cable at a level of 2 makes, turned A over); its reading of the louder of B's two sides; and the glide from the carrier to a cable (the test looked at the steps inside a block, and a gain that is switched at once jumps between the last frame of one block and the first of the next). **One of the page**: an edge at the second input of a group, which the controller never draws, is left out as one with a node that is not there is.
TODO: only the Math has a second input. A plugin of the three formats that has a sidechain bus (a compressor's, a vocoder's) still gets silence there: `process_side` is what an adapter would implement from the plugin's second bus, and the ports that a plugin names for itself (more inputs, ports of control, events) are the rest of P3g.
TODO: B has no level of its own and no meter (a cable has its level, 0 to 2, and the carrier is the effect's own).
TODO: the Session tab of the terminal shows no cable and no second input, a second input has no light for what it carries, and the Patch tab has a column for it and no link (there is none).

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

### The master limiter and volume (`[master]`, `SetLimiter`, step 102)

The output mix has a **volume** and a **limiter**: the user's "Also I think we need a limiter or a limit-option on the master, as we often reach the clip-lvl and its hard to control" (2026-10-29) and, asked whether it should be in the sound from the start, "1. yes limiter default on". The limiter is a look-ahead peak limiter at the end of the signal chain: after the lanes and the groups are added up, after the master volume, before the master's meter, its scope and the sound card, so that a mix that is too hot is **brought down and not clipped**.

- **In the sound by default, for every session** (the user's word): a file that has no `[master]` table has the limiter in the sound at a **ceiling of −1 dBFS** and a master volume of 1 (`MasterNode::default`). A mix that stays under the ceiling is not changed (the gain is 1 and the same samples come out), but while the limiter is in the sound **the whole output is its look-ahead late: 95 frames, 2 ms at 48 kHz** (`Limiter::latency`); taking it out (or `limiter = false`) gives the mix back at once, with no delay. The delay is at the very end of the chain and is not a latency of a path (the compensation of "Latency" has nothing to put in step after it).
- **The DSP** (`src/plugin/chain/limiter.rs`, `Limiter`). For each frame the loudest sample of either channel says what gain would bring it down to the ceiling (`ceiling / peak`, or 1 for a frame under it; the two channels are linked, so that the stereo image does not move when one side is limited). The sound is held back by `window − 1` frames (`LOOKAHEAD_SEC`, 2 ms: `round(0.002 · rate)` frames, from 2 to 512), and each frame comes out with the gain that was worked out for it a window later: the **least** gain wanted over the frames from it to the look-ahead after it (a sliding minimum over a monotone deque, so that the gain is down when a peak arrives), which **goes up again no faster than the release** (`RELEASE_SEC`, 80 ms, an exponential) and is then **averaged over the window** (so that it comes down in a ramp the length of the look-ahead and not in one frame, which would click). Every gain in the average is at most what the frame itself wanted, so **a frame comes out at the ceiling or under it**; the clamp at the ceiling at the end is for the last bit of rounding. The gain is kept in 64 bits (a one-pole in 32 stops short of 1 by the step of its number over twice its coefficient, so that a sound under the ceiling would never come out as it went in again: see the notes on a one-pole follower), a sample that is not a number is a silence to the limiter, only `+ − * /`, `min` and `max` run for each frame (the same frames in are the same bits out, whatever the blocks, `the_result_is_the_same_bits_whatever_the_blocks_it_is_given_in`), and nothing allocates (the lines are made for the longest window).
- **The master node** (`Master` in `src/plugin/chain/nodes.rs`, run by `Stage::Master`; the rest of the engine is as it was). The mix is turned by the volume, put through the limiter, copied to the output and measured. A mix with a volume of 1 at both ends of the block and the limiter out of the sound at both ends is **copied as it always was, bit for bit** (a negative zero and a `NaN` stay what they are), and the limiter goes on running on the mix without being heard (`keep_limiter_with`), so that it is where it should be when it is put in. Otherwise (`shape`) every frame is the mix times the volume, which has gone to its goal by a **linear ramp over the block** (as a mute's gain does), and the blend of the mix and the limited sound, which a switch moves by a **linear crossfade over the block** (the limited sound is a look-ahead later). The meter and the scope read what comes out, not the mix. `GraphCommand::SetMaster { volume, limiter, ceiling (dB), sample_rate }` always carries the whole state (the engine starts at a volume of 1, the limiter out of the sound and a ceiling of −1 dB, which is the model's `MasterNode` with the limiter off: so that every test of the engine that is not about the master is what it was); a volume is held to 0 to 4 and a ceiling to 60 dB under full scale and to full scale (`ceiling_level`) in the engine, and a number that is not a number leaves the master as it was.
- **How far it works.** The engine publishes, in a telemetry cell of its own, the most that the limiter has brought the sound down by in the last blocks, in decibels (`Master::reduction`: it falls 40 dB a second, as a meter's peak does, and is held to 0 under 0.05 dB: `telemetry::MASTER_REDUCTION`). The cell is **the last of the 4,096** and is never handed out (`HANDED_OUT` is `CAPACITY − 1`), so that no other slot moves and, while the limiter is out of the sound, the hash of the telemetry in the oracle is what it was; the layout has it as `SlotKind::Reduction` beside the master's peak and clip count, and the pusher sends it as a float.
- **The model and the file.** `SessionModel::master: MasterNode { volume, limiter, ceiling }` (`MASTER_VOLUME_MAX` 2, `CEILING_MIN` −24 and `CEILING_MAX` 0: what the intents hold them to; a decibel ceiling is **below full scale**, so that the knob goes from −24 dB to 0 dB). The file has a top-level `[master]` table, after `bpm` and before `[keys]`, with three keys that are all optional (`volume`, `limiter`, `ceiling`; `session::MasterConfig`, `SessionConfig::master`, `SaveSources::master`), and a save writes **only the keys that are not what a session has without them** (a volume other than 1, `limiter = false`, a ceiling other than −1) and no table at all for a session that has none (`SessionModel::master_to_save`), so that every file that was written before there was a master is saved as it was. A number that is not finite, or too big for the 32-bit numbers, and a key of the wrong type refuse the session with its place (`master.volume is not a finite number (NaN)`); a value over what the intents allow is held to it when the file is loaded (`master_of`). The loader tells the audio thread **once**, and only when the master is not what the engine starts at (so a file that says nothing sends `SetMaster` for the default limiter, and one that says `limiter = false` sends nothing).
  ```toml
  [master]
  volume = 0.8          # the output mix's gain before the limiter, 0 to 2 (default 1)
  limiter = false       # take the limiter out of the sound (default true: it is in)
  ceiling = -3.0        # where it holds the output, dB below full scale, -24 to 0 (default -1)
  ```
- **The intents** (all steps of the history, in the words of what they do). `SetLimiter { on }` (told as `LimiterSet { on }`: "turn the limiter on", "turn the limiter off"; none for a limiter that is as it is asked to be, which leaves what could be redone). The volume and the ceiling are two more `Gain`s, `MasterVolume` (0 to 2) and `LimiterCeiling` (−24 to 0), which `SetGain` takes for `NodeId::MASTER` (the controller looks for the master **before** it looks for a node in the model, `set_master_gain`; every other node refuses them and the master refuses every other gain, in words: `Volume does not apply to that node`), held to their ranges and refused for a value that is not a number, with nothing changed: "set the master volume" and "set the limiter's ceiling", merged as a drag is (`Key::Gain(node, gain)`). The graph a client gets has the master as `NodeKind::Master { volume, limiter, ceiling }`, and a `Snapshot` applies the events (`Snapshot::change`). Each of the three tells the audio thread the whole master (`Controller::tell_master`, at the sample rate the controller has). Undo puts the model **and the engine** back.
- **The terminal.** On the Session tab (chain focus, whatever the cursor is on): `L` puts the limiter in the sound or takes it out, `C` sets the ceiling (a value popup, "limiter ceiling (dB below full scale, −24 to 0)") and `V` the master volume ("master volume (1.0 = unity)"); the status bar says `LIM −3.2 dB` in yellow, left of the CLIP light and the tempo, **while the limiter is in the sound and has brought the sound down by 0.1 dB or more lately** (`LIMIT_LIGHT_FROM`), so that a person sees that the limiter is working and what it costs (it gives way, as the web label does, to the CLIP light and the tempo when it would touch the tabs: a narrow terminal keeps those two); the Help tab lists the three keys (the action bar does not: its hints are in about a hundred snapshots).
- **The page.** The master's card has, under its meter and its scope, a row of three cells: the **master volume** knob (`Master volume`, 0 to 2, a double click puts it back to 1), the **ceiling** knob (`Ceiling`, −24 to 0, read as `−3.0 dB`, a double click puts it back to −1) and the **Limiter** button (`On` or `Off`, `aria-pressed`: it shows what the session has, and a press asks `Editor.setLimiter`), and under them **how far the limiter brings the sound down**: a bar and a number (`ReductionMeter`, the meter `Limiter reduction`, 0 to 18 dB, read in the DOM on the frames with no React render). The master card is 373 pixels tall as a browser draws it (`MASTER_HEIGHT`, 378; 223 before this step). The volume is written `Volume` under its knob, which is all that fits (`Knob`'s `caption`; its name to a screen reader and to a test is `Master volume`). The reduction's number is written to the page only when its words change (a write on every frame made a test of the page that waits for it to settle go round for ever: found by the first run of the whole suite). The two knobs are `GainControl`s with no port and no `~` button: **a modulator does not drive the master's volume or its ceiling** (`GAIN_PORTS` has none for them; a person sets them), and are called `Master volume` and `Ceiling` so that the page's one `Volume` for every lane and group is still one for each. The mock engine does what the controller does (`setLimiter`, the master's gains in `masterGainOutcome`: the ranges and the words of the refusals, the history's words, the master's level held under the ceiling and the reduction in the telemetry it serves), pinned by a script of `history.json` and by the generated fixtures.
- **The oracle.** The whole corpus is rendered with the limiter **out of the sound** unless a session says otherwise (`Rig::new` puts `[master] limiter = false` in the config of a scenario that has no `[master]`, as every session of it was written before there was a limiter and what the engine renders is held to the bit): **the 1,029 renders that were there are the same bits** (the stored files have six lines and two lines more, and nothing else changed). New: a session (`master`: three oscillators at a volume of 4, far over full scale, with `[master] volume = 0.8, ceiling = −3.0`: the loudest sample of its chord is exactly 0.7079458, −3 dB, which the oracle's fingerprint holds) and a scenario (`edited:master`, `MASTER_EDITS`, 14 edits with `Edit::{Limiter, Master}`: the ceiling brought down to −12 dB and the volume up to 2 while a chord is held, the limiter taken out of the sound and put in again, Undo and Redo through all of it): six renders more, **1,035 in all** (69 always on). The engine's master is held to the model's after every block (`Facts::master`, `MasterFacts`, against `testutil::model_master`); the loop that the plan replaced copies the mix as it is, so a scenario that has a `[master]` or sets the master by an edit (`Scenario::has_master`) is not held to it, and the whole corpus's no-allocation test holds the master too (`a_master_that_shapes_the_sound_asks_the_allocator_for_nothing`).
- **What else the default changed.** The tests of `src/audio.rs` and the terminal's that measure a clip (a session far over full scale) say `[master]` with `limiter = false`, and one test of each is new where the limiter is in the sound and the light stays dark (`a_session_whose_limiter_is_in_the_sound_does_not_clip_though_it_is_far_over_full_scale_and_the_light_stays_dark`); the test helpers that list what the loader sent (`testutil::described`, `drain`, `try_load`) keep the `SetMaster` of a session that has the default master apart (`Run::master`), so that the lists of the other tests are what they were.
- **Held by tests.** Rust: the limiter's 14 (`limiter.rs`: the bits that go in come out 95 frames late and the gain is exactly 1 under the ceiling; nothing comes out over any of four ceilings at four block sizes; the ramp that ends at the peak and is as long as the look-ahead; the release; a steady sine that is turned down and not distorted; the same gain for both channels, and for a side that is louder than the other whichever side it is; a sound that is not a number; the same bits whatever the blocks; the window at each rate; a ceiling that is moved while it runs; the sum of the ring after a long run, and one that has drifted; and the gain frame by frame against a reference that is worked out in 64 bits from its description), the master node's 11 (`nodes.rs`: a copy that is bit for bit what it was, the volume's ramp and its way back to 1, a limiter that is the limiter alone, the crossfade, the reduction published and falling and its floor and the most of a block, the meter and the scope, no allocation, a ceiling that is not a number; and an ignored benchmark, `the_master_with_a_limiter_against_its_deadline`: with the limiter in the sound and a mix that is over its ceiling in every frame, which is the dearest it is, the master costs 2.9 µs a block of 256 frames in a release build, 0.055% of the 5.33 ms that a block has (Apple M4 Pro)), the controller's 5, the history's 2 and the history fixture's script, the loader's 5, the file's 3, the telemetry cell's 1 (and the layout's, the snapshot's and the wire's, extended), the terminal's 4 (`L`, `C` and `V`, the light, the narrow terminal) and a clip test that says the limiter is in, and the real program's 1 (`tests/serve.rs`: a chord up to three times over full scale is held at the ceiling and never over it, with the CLIP light dark and the reduction up, let through to clip with the limiter out, held at -12 dB, the volume in front of the limiter, the file's `[master]`, and a program started on that file and on one that says `limiter = false`). The oracle: the session `master` and the scenario `edited:master`, and every older render the same bits. Rust 2,531 tests (2,717 with the `web` feature, 2,525 without the default plugin formats), the page's 2,928 tests in 126 files (`PatchView.master.test.tsx` 8, the mock engine's master level and reduction, the reducers, the selectors, the counters, the knob's caption), the browser suite 207 (`e2e/master.spec.ts` 2, on the mock and on the real program). **Held by mutants** (step 102): 78 of the Rust (the limiter's arithmetic 16, the master node 23, the controller 8, the history 4, the model 6, the loader 5, the file 2, the telemetry and the layout 3, the snapshot, the graph and the intents 5, the terminal 5, the oracle's own default 1), one at a time against the unit tests that should catch them: 70 were caught by the tests that were there (none only by the build) and 8 survived: **seven were gaps, each a test now** (the minimum's window one frame too long, a side that is louder than the other, a sum that has drifted, a volume that goes back to 1, the least gain of a block and not its last, the floor of the reduction and a reduction above it) and **one is no fault** (the explicit comparison of the peak with the ceiling: a gain over 1 never gets past `held <= 1`). 51 of the page (the reducers 5, the selectors 3, the editor 1, the history 7, the mock engine 12, the cell 4, the gains 5, the readout 6, the card 4, the knob 2, the counters 1, the layout 1): the first run of them was void (all 51 "caught" in no time: the runner was given test paths with `web/` in front and Vitest 5 found no file, which is an exit status of 1), and the second caught 47, two of them only by the type check, and let 4 survive: **four gaps, each a test now** (the mock's refusal of a number beyond 32 bits for the master's gains, the fall of the reduction in the mock, the readout's threshold, nothing while there is no reading) and, of the two that only `tsc` caught, **one gap, a test now** (the mock's floor of a twentieth of a decibel) and **one no fault** (a limiter set on every master node, of which there is one).
- **Decisions** (D102 of `docs/web-ui.md`; the user may overrule them). (1) On by default for every session, including files that were written before it, at **−1 dBFS** (headroom for the sound card's converter and for the inter-sample peaks that a sample-peak limiter does not see) and a look-ahead of 2 ms and a release of 80 ms that are not settings (a limiter that is for catching a peak and not for a sound). (2) A peak limiter with **linked** channels and a gain that is smoothed over the look-ahead and is not a clipper: the sound is brought down by a gain that the ear does not take for distortion, at the price of 2 ms. (3) A **master volume** with it (0 to 2, in front of the limiter), because a person who has a limiter wants to bring the mix up into it. (4) The volume and the ceiling are not modulated by a cable. (5) The readout is the limiter's own (how far it brings the sound down, not the output level, which the master's meter has).
TODO: the limiter limits the **sample peak**, not the true peak: the reconstructed wave between two samples can be a decibel or two over the ceiling (a ceiling of −1 dB is the headroom for that); an oversampled detector would do it, at the price of more look-ahead.
TODO: the look-ahead (2 ms) and the release (80 ms) are constants of `limiter.rs`, and the ceiling has no unit but decibels below full scale: a release that follows the program (a fast and a slow stage) and a knob for either would be small changes.
TODO: putting the limiter in the sound or taking it out is a crossfade over one block between the mix and the limited sound, which is 95 frames later, so that for a block the two are mixed with that offset (a comb, a click's worth of it); a delay that is kept on the mix while the limiter is out of the sound (so that the two are in step) would end it, at the price of the same 2 ms with the limiter out.
TODO: the limiter is the master's only: a limiter or a compressor on a lane or a group is an effect that a person adds (there is none built in), and the master has no "soft clip" or a limiter that is "off" because a file says so for a session of an older version: the key is `limiter = false`.
TODO: the master volume and the ceiling have no port, so no cable drives them (a macro on the master's volume would be a fade-out); and the terminal has no bar for the reduction (the status bar's `LIM` says it in numbers).

### Latency (`Plugin::latency`)

A plugin that makes its output later than its input (the look-ahead of a limiter, the filters of an oversampler, a linear-phase equaliser) says by how many frames: `Plugin::latency` (P3f
of `docs/patch-graph.md` §11.6, step 80). The built-in drive says 23 (`builtin:drive`: the two half-band filters that its oversampling is made with), the other built-ins say 0; a CLAP plugin
says it on its `latency` extension and a VST3 plugin by `getLatencySamples`, both asked once, when the plugin has been activated at load, and an LV2 plugin on its `lv2:reportsLatency` port,
which is read every block (so that it follows the plugin, and is 0 until the plugin has run once). `tang describe <plugin>` says it (`Latency: 23 frames (0.48 ms at 48000 Hz)`, or `none`). The engine
holds a plugin's number to `MAX_LATENCY`, 131,072 frames (2.7 s at 48 kHz): a delay is a line of that many frames for each channel, and a plugin that says what it was not asked would make one
of gigabytes. The latency of an instrument is that of the head of its lane.

What the host does with it is what a mixer with plugin delay compensation does. **Where cables are added at an input, the paths are put in step**: a group's bus, where its members are added; the
mix; an effect's input where a cable ends at it, with what its chain gives it; every input of a node together, so that a sidechain is in step with the main input (the second input of a Math too: step 95). What arrives at a node is as late
as the latest of the cables that end at it, the cable that is the latest is not delayed, and each of the others is by the difference, so that nothing is made later than it has to be and the latency of
the session is that of its longest path (`Schedule::latency`, test builds; it is not shown anywhere yet). A cable that is alone at its input has no delay, whatever the latency is upstream of it. **The dry
part of an effect that has latency is put in step with the rest of it**: the effect's `mix` blends the sound that was put in with the sound that the plugin made, which is `latency` frames late, so the
dry sound goes through a line of that many frames (the effect's own, kept under its id) before the blend, and the line is run whatever the mix is, so that a mix that is turned down finds what was put
in. A drive at half its mix is the sound 23 frames late; where the dry sound was not delayed it was a comb (0.5 x(t) + 0.5 x(t − 23)), 39% of the height of a sine of 261 Hz off the sound that is 23 frames late, and at 1 kHz a sine had 6% of its height left (`a_drive_that_is_blended_with_the_sound_that_went_in_does_not_make_a_comb_of_the_two`).
A session in which nothing has latency has no delay and no line, and is what it was, bit for bit.

*How.* `kernel::align` (`kernel/src/latency.rs`) finds, from the latency of each node, how late what arrives at each node and what it makes is, and by how many frames each cable is to be delayed
(`Alignment`; the order is `compile`'s, which `layout` now makes for both), and `kernel::with_delays` makes the patch that has a node on each cable that is to be delayed, with the rank of the node
that the cable starts at, so that what is added at an input is added in the order it was. The node is a `kernel::DelayLine`: a ring of frames for each channel through which a block of any length goes in
place (`Stage::Delay` in the engine: `Link`, which the schedule keeps under the two nodes that its cable joins, so that the same line, with what it holds, is in the next plan that has a cable
between them; a line whose length changes keeps the newest of what it has, and has silence before it if it is longer). The schedule works the latencies out when the plan is made (`latencies_of`) and
reads them into the `Shape` that the plan is made for, so that a plugin that says another latency makes the plan again (an LV2 plugin's port, an effect or an instrument that is put in or taken out): the
block that makes it may ask for memory, as every block that an edit was made before may. No other block does, with delays and dry lines in the plan
(`a_block_with_delays_and_dry_lines_in_the_plan_asks_the_allocator_for_nothing`). The lines run in a muted lane too, as everything of a lane does, so that what is brought back is in step.

The oracle holds the plan to the loop that it replaced with the compensation turned off (`oracle::engine_uncompensated`: the loop has none), and holds the compensation apart: where nothing has
latency the engine is bit for bit what it is without it (`Scenario::has_latency`), and `edited:latency` plays a drive into a lane and onto a bus, blends it, draws a cable from it and undoes all of it while
the session sounds.

TODO: a plugin that changes its latency while it runs and says so to the host (CLAP `clap_host_latency.changed`, VST3 `restartComponent(kLatencyChanged)`) is not followed: the plugin is on the audio thread
by then and both are the main thread's (an LV2 plugin is followed, by its port); a way for the adapter to read it from the audio thread, or to be told by the main thread, would end it.
TODO: a latency that changes, and a delay that comes into the plan or goes out of it, is a jump in the sound that is delayed (a line that is made longer has silence before what it holds, one that is made
shorter drops its oldest): a crossfade over a buffer would end the click, as the fade of a mute does; a DAW that makes up for latency has the same dropout when a plugin is put in.
TODO: the latency of a session (its longest path) is in the engine and not shown: the page and the terminal could say it, with the latency that each plugin says (the model has the plugins at load, and
the adapters' numbers are theirs then), and a build of Tang as a plugin would have to tell its host. And a session whose longest path is long answers the keys late by as much; no setting lets
the shorter paths be early instead (a "live" mode that does not delay what a person plays).
TODO: the adapters of CLAP, VST3 and LV2 are not tested against a plugin that says a latency (none that is installed here does: `tang describe` says `none` for Pianoteq 9, ROLI Airwave Player, ROLI Studio
Drums, Traveler and the two CLAP plugins of the rustsynth project); LV2's number is held by the pure function that turns a port's value into frames and nothing more.

### Plugin slot fields

Each plugin slot has:
- `plugin` — plugin path or URI (required)
- `preset` — preset name to load (optional)
- `params` — parameter overrides applied after preset (optional)

The instrument additionally has:
- `volume` — host-side output gain, applied before effects (default: 1.0, uncapped in the TOML; a modulator can drive it, `gain = "volume"`: see "The volume, the mix and the pan that a modulator drives")
- `transpose` — note shift in semitones (default: 0)

Effects additionally have:
- `mix` — host-side dry/wet blend, 0.0=dry 1.0=wet (default: 1.0; a modulator can drive it, `gain = "mix"`)
- `feed` — `"chain"` (default) or `"none"`: whether the stage before the effect feeds it (written only when none; see "The input of an effect")

Load order per plugin: load → preset → params.

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
is `~ Macro 0.50 → Filter: cutoff (50%)` in the tree and `Macro` (its value, the "Targets" separator and the depth of each cable) in the pane.
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

### Free modulators (`[[modulator]]`, `AddFreeModulator`, `ModulatorLocation::Free`, step 91, P5b-2)

A modulator that is in no lane and no group: an LFO that is nobody's, with a cable to a parameter of one lane and another to a parameter of a second (one LFO on two lanes: `examples/shared-lfo.toml`), or an
envelope that every key opens whatever it plays. A rack was a lane's or a group's, and a modulator could drive what its rack reaches and nothing else; P5b-2 of `docs/patch-graph.md` §11.6 made the session's own rack the
third place that a modulator can be (`ModulatorLocation::{Lane(i), Group(i), Free}`) and gave it the reach of the whole session. P5b-2a came first, as a refactor that the oracle held bit for bit (`e40873c`): the ten pairs of
commands that addressed a lane's rack and a group's (`InsertModulator` and `InsertGroupModulator`, `AddModTarget` and `AddGroupModTarget`, …) are ten that name their rack by `ModulatorLocation`, so that the third rack was a
variant and not ten commands more. **Since step 96 it is the only rack** (the lanes' and the groups' were taken out: see "The racks go (step 96)" below): `ModulatorLocation` has the one variant `Free`, which stays, with the commands that name
a `location` and an `index`, until P5c makes them address a modulator by its node (the word "free" in the names of this section is what is left of the three racks: a modulator is just a modulator now).

- **The model.** `SessionModel::free_modulators` (`ModulatorSlot`s, as a rack's) and `AudioGraph::free_modulators` (`Modulator`s). A free modulator has a node of the model like any other and **no parent**: the graph a client gets has it
  with `parent` null (a client reads that as a node of its own), after the lanes' nodes and before the master, its cables are edges from its output to the ports they drive, and the telemetry's layout lists its output after the groups'
  and the lanes' outputs, and names the value of a parameter that several cables drive once, by the first of them in the order of the rack (until step 96, by the one nearest the plugin: a lane's, a group's, then a free one's). `nodes()`, `racks()`, `locate_edge`, `check_ids`, `kind_of`,
  `param_targets`, `field_targets` and `modulators_in_reach` have the free rack. The ids: a modulator's node is made after the lanes' and the groups' (and before the sources'), in the order of the rack: a loaded session has the modulators that its lanes had first, then its groups', then the file's own (a session that has none uses no id,
  so that every other session has the ids that it had).
- **What it drives.** `ModTargetKind::FreeParam { plugin, param_index }` is the place that the file and the terminal's popups name a parameter by (`kind_of`, `param_targets`; the model's cable and the engine's are `Driven::Param { plugin, index }`, by
  node, as every cable's): any parameter of any plugin of the session (the instrument of a lane that has one, an effect of a lane's chain, an effect of a group's bus: a group has no parameter of its own, and a lane that has no instrument has no instrument's
  parameters) and, as `Driven::Setting` and `Driven::Depth`, a rate, a stage of an envelope or the depth of a cable of **another free modulator**, never its own; and, since step 97, the volume of a lane that has an instrument, the mix of an effect and the volume and the pan of a group (`Driven::Gain`: see "The volume, the mix and the pan that a modulator drives").
- **What it hears.** Every note and every source, unless it has an ear ("The ear", below) (`Events::all()`: the frame and the bytes of every message of the block whatever its source, since it has no key range, remap or transpose to pass them through and belongs to no lane whose sources could be chosen):
  an envelope opens on any note, a retriggering LFO and a step sequencer start again on any, a velocity or a key control follows the last note that anyone plays (a chord has its last note's), and a MIDI control learns and follows from any source on any
  channel (the member channels of an MPE controller are not told apart: the last message of the buffer stands).
- **When it runs and what adds up** (`process_events`, `apply_free_modulators`). First of all, before any lane renders: the modulators tick with the tempo, `apply_cross_mod` resolves the cables between them, their outputs
  are published, a control that learns is told to the main thread (`drain_learned`), and then each parameter that a cable drives is set. The plugin is found by its node anywhere in the graph (`locate_anywhere`: a lane's chain, by
  `slot_in_chain`, and then a group's bus; `Anywhere::{Lane, Bus}`); what the cables on one parameter offset it by is **added** (two LFOs on one cutoff add up: `two_free_modulators_on_one_parameter_add_up`), the sum is held to the
  parameter's range (`what_free_modulators_add_up_to_is_held_to_the_range_of_the_parameter`), and the parameter is set to the value the user set plus it. **No modulator wins over another** (until step 96 the nearer to the plugin did: a group's modulator
  wrote the parameter after a free one's, and a lane's after that, each from the user's value and its own offsets alone, so that the offsets were not added; step 94 made them add up when it moved the lanes' and the groups' modulators into the session's rack, and step 96 took the racks out). A cable whose
  plugin is not in the graph (it went) drives nothing and publishes nothing, as at step 90 (`a_free_target_on_a_plugin_that_is_nowhere_publishes_nothing_and_the_others_are_unaffected`). `AudioGraph::centre` and `targets_mut` reach the rack, so that a parameter that
  the user sets is what the cables on it are centred on, and so does `rack_mut`, which the commands of a rack go through. A session that has no modulator runs none of this (the list is empty: a check), so that every older session renders the same bits and
  costs what it did (the benchmark's stress session with a chord of six, which has none: 822 µs a block of 256 frames, against 814 µs for the loop that the plan replaced; 819 and 820 at step 90, which is the difference between two runs); nothing allocates with them
  (`publishing_modulator_telemetry_does_not_allocate`: an envelope, a step sequencer and a control that learns, with cables to a lane's plugin, to a bus effect and to another's rate).
- **The intents.** `AddFreeModulator { kind, hears }` makes a modulator of the kind (an LFO, an envelope, a macro at rest, an armed MIDI control, a velocity or a key control, a step sequencer) at the end of the session's own rack, hearing the lane or the group `hears` or every note (step 94): a step of the history
  (`add an LFO`, a `Plan::Added`, undone by `RemoveNode` and made again under its id), told as a snapshot (the structure changes). `AddModEdge { modulator, to }` takes a free modulator as it takes any: a cable to a parameter of a plugin anywhere in the session or to the
  setting or the depth of a free sibling's, refused for what is out of its reach (a group, a parameter or a setting it drives already, itself). `Modulate`, which makes a modulator for a parameter, makes one of the session's own that hears the lane of the plugin (step 94: see "Every modulator is global"). `RemoveNode` of a free modulator takes it out with the cables that drive its settings and depths (each written down: undo puts it back whole under its ids, `Inverse::Modulator` with the free rack for its place) and gives what its cables drove
  the value that the user set. **The cables of free modulators to a plugin go with the plugin:** `remove_effect`, `remove_bus_effect`, `put_instrument` and `clear_instrument` take them out first as they take any cable to it (`cables_into_plugin`), and
  `remove_lane` and `remove_group` take those that drive the plugins of the lane and of the group's bus effects (`SessionModel::free_cables_into`), each written down and put back by undo; the free modulator stays, and what its cables drove goes with the plugin, so
  nothing is given back for it.
- **The file.** A top-level `[[modulator]]` with the keys that a lane's older `[[instrument.modulator]]` has, written after the `[[group]]` tables and before the `[[cable]]` ones, and each cable a `[[modulator.target]]` that names the plugin it drives by its place in the session, as the audio cables name
  their ends, and the parameter by its name:

  ```toml
  [[modulator]]
  type = "lfo"
  rate = 0.5
  ui = { x = 0, y = 400 }          # optional, as every node's
  hears = "instrument[1]"          # optional (step 93): whose notes it hears, a lane (`instrument[N]`) or a group (`group[N]`); every note without

  [[modulator.target]]
  to = "instrument[0].effect[0]"   # the plugin: `instrument[N]` (its instrument), `instrument[N].effect[M]` or `group[N].effect[M]`
  param = "cutoff"
  depth = 0.2

  [[modulator.target]]
  mod_rate = 1                     # a free sibling's rate, by its place in the file (`mod_attack` …, `mod_depth = [m, c]`: as a lane's)
  depth = 0.3
  ```

  `to` is the one key that a free cable has and no other cable has (`ModTargetConfig::to`); a cable to a sibling has none. The loader (`load_free_modulators`, after every lane and group, so that a place can be any, and sharing `make_rack` with the lane's loader
  and the group's) makes the modulators' nodes first, so that a cable can name a sibling that comes after it, plans each cable (`plan_free_target`) and **loads without a cable that drives nothing, and the log says why**: a `to` that is missing, that is not a place of
  the session or names no plugin (`group[0]`, `master`, an instrument that the lane has not), a parameter that the plugin has not, a sibling that is not there or the modulator itself. A negative LFO rate and a step sequencer's steps that are none or too many refuse the
  session in a free modulator as anywhere (`modulator[1].rate is below 0 (-1): …`: `session::each_rack`, whose place for the file's own rack is empty). A save writes what a load reads (`modulators_to_save(Free)`, `plugin_place_of`: a lane's instrument, an effect of
  a lane's chain or of a bus) and the same file again for a session that was loaded from one (`a_session_with_free_modulators_is_saved_as_the_same_modulators_and_cables_and_the_same_file`), and writes no table for a session that has none.
- **The terminal.** The Session tab's tree has the free modulators first (since step 94 those that hear every note: the ones that hear a lane or a group are listed with it), above the first group and the first lane, in magenta and not indented, each saying what its cables drive by the lane or the group of the plugin, as the Patch tab tags them
  (`~ LFO 0.5Hz sine → L1 Oscillator: detune (30%), G1 Filter (bus): cutoff (40%), Mod 1 attack (25%)  (free)`, and `(free, hears L2)` or `(free, hears G1)` for one that has an ear: `plugin_tag`, `free_target_label`). `F` adds a free LFO from anywhere in the chain and puts the cursor on it (`m` on a free modulator adds another;
  the Help tab lists `F`), `t` opens the target selector (every parameter of every plugin, each labelled by the lane or the group it is in, and the settings of the sibling free modulators: `open_free_target_selector`, whose scope is `ModulatorLocation::Free`), `d`
  takes the modulator out (`RemoveNode`, a step of the history), `Enter`, `Left` and `Right` edit its rows (through the intents of the rack), and `m` and `l` on a row of its pane cross-modulate or learn it. `h` chooses whose notes it hears (see "The ear").
  The action bar of a free modulator is `F new free`, `t add target`, `h hears` and `d delete` (the other bars do not say `F`: their hints are in about a hundred snapshots, and the Help tab lists it). A free modulator has no signal for the node's scope (`o`).
- **The page** (step 92, P5b-3; `docs/web-ui.md` D92). *Making one.* The bar has **"+ Modulator"** (`app/AddFreeModulator.tsx`, `AddFreeModulatorMenu`, the button `Add a modulator`: a menu of the kinds that a lane's "+" has, `MODULATOR_KINDS` in `app/patch/nodes.tsx`), which asks
  `Editor.addFreeModulator(kind)` (the intent `AddFreeModulator`, in the history's words "add an LFO") and shows the canvas the node it made; the "×" of a free modulator gives the focus back to that button (`ADD_FREE_MODULATOR_ID`). *Where it is.* A modulator that has no parent and hears every note (since step 94 one that hears a lane or a group is above that strip: see "Every modulator is global") is a node of its own in a
  **row at the top, above the first lane's strip** (`toFlow`'s `layout`: the modulators with no parent, a column each from the first in the order of the graph; one that drives a setting of another is a row above it, as in a lane's rack, `tiersOf`; the lanes start below the room of the rows and of
  the gap that a cable needs to leave them, `CABLE_GAP`, so that a cable from one runs down to a knob; a session that has none has no room for them and lays out as it did), and when a canvas that has been laid out is given one it goes above the first lane **where that lane stayed**
  (`carryOver` goes by the first lane for what is in the session's own rack, `firstLane`), so that nothing else moves. *What it can be drawn to.* `client/reach.ts`: `reachFrom` of a modulator is every plugin of the session (the instruments that have one and every effect, a bus's too) and
  `modulatorsInReach` the other modulators (since step 96 every modulator is one of the session's own: `ownerOf` is gone), so that the list at the foot of its node (`DrivePicker`) offers every plugin's parameters and the settings and the cable depths of the others (it says `Nothing to
  drive` no more), a cable can be dragged from its output to any knob, and **the `~` menu of any knob offers it under "Attach"** (`driversOf`) unless it drives that knob already. *What a knob shows.* The ring of a parameter that several cables drive is the sum of theirs
  (`modulationsOn`: every driver adds up, as the engine's do; until step 96 it was the nearest rack's, `NEARNESS`: a lane's, else a group's, else the session's, whose cables added up). *The mock engine* does what the controller does (`addFreeModulatorOutcome`: a node with no parent after the last of the session's
  own, or before the master, whose output the layout of the telemetry lists after the others'; `Modulate` of a setting of one makes another that is in no lane; one that follows the notes, an LFO and a step sequencer hear every note; the value of a parameter is the sum of its cables' offsets, and a refusal says "this
  session"), pinned to the controller by three scripts of `history.json` (one modulator with cables to plugins of two lanes and a bus and to a sibling's setting, taken out and put back under its ids; the refusals of reach in the controller's words and a `Modulate` of a setting; what goes with a plugin, a lane and a
  group that are taken out) and by `fixtures/freeReach.json` (the controller's reach for every modulator of a session with groups, lanes and two that are in neither: the plugins and the settings and cable depths; the sessions of `reach.json` and `fields.json` are as they were). Browser specs:
  `e2e/freemodulators.spec.ts` (6: the row above the lanes and nothing else moving, one modulator on two lanes and the knob's `~` menu, taking it out and Undo, one modulator driving another's setting, "Arrange", and the real program: the knobs of two lanes move, the file keeps the table with
  the plugins named by their places, Undo, a page opened again).
- **The oracle.** `freemod` (a corpus session: a free LFO with cables to an oscillator's detune, a filter's cutoff and a reverb's damping on a group's bus, a free envelope with a cable to the filter's resonance and another to the LFO's rate, a free step sequencer
  on the second oscillator's detune, played with the chord, the wheel and the pedal) and `edited:freemods` (19 edits, each made: free modulators of three kinds made while a chord is held, their cables drawn to every kind of plugin, the cutoff set by the user,
  the filter taken out while a free cable drives it and put back by Undo, a lane taken out with the step sequencer's cable and put back, the LFO taken out and put back, the group taken out with the envelope's cables to its reverb and put back, taken out by Redo and put
  back again: `FREE_MODULATOR_EDITS`, `Edit::{AddFreeModulator, FreeCable, RemoveFreeModulator}`) make six renders: 1005 in all, the stored file has the six lines more and the 999 renders that were there are the same bits. The loop that the plan replaced
  does not tick them, so a scenario that has any (`Scenario::has_free_modulators`: a `[[modulator]]` table in its file or an `AddFreeModulator` among its edits) is not held to it (`is_in_the_old_loop`, which the always-on test and the whole corpus's both ask now: the ignored one had
  asked for cables only, and the first release run found it). The facts hold the engine's free rack to the model's after every block (`Facts::racks` and `bases` have it last; `testutil::model_racks`).
- **The ear** (step 93, P5c-1 of `docs/patch-graph.md` §11.6: the user's direction of 2026-10-27, "I think all modulators should be global like this"). A modulator of the session's own can hear **one lane's or one group's notes** instead of every event, which is the one thing that a
  lane's or a group's modulator has that a free one did not: a lane's hears what its instrument is given (the source filter, the key range, the remap, the pattern's own notes and the transpose, in that order, and the MPE routing of its member channels), a group's what its members are given.
  `Modulator::ear` (the engine's) and `ModulatorSlot::ear` (the model's) are an `Option<NodeId>`: the node of the lane (its instrument's node, which a lane that has no instrument has too) or of the group; **none is every event, as it was**, and so is a node that is neither a lane nor a group that
  is there (a lane that was taken out: the audio thread hears every event for it, `SessionModel::ear_of` tells the graph and the file the same, and the model keeps the node it was told, so that Undo of the removal gives the ear back). Every modulator has one
  (since step 96 there is no other kind: `check_ids` holds an ear to a node that was made). Its writes are added to those of the others on the same parameter (until step 96 a lane's own modulator wrote later and won, see "Application order": the one thing
  that moving a lane's modulator into the session's rack changed was that two on one parameter, a lane's and a free one, were added and not won over).
  - *The engine.* `InstrumentLane::prepare_events` is the lane's pipeline from the block's events to the notes that its instrument gets (`filter_midi`, the remap, the pattern, the transpose), its result kept in `heard`; `run_head` takes it ready when a modulator that hears the lane had it done ahead of
    the head (`prepared`) and does it itself when none did, so that **a lane that nobody hears runs exactly the code that it ran** (the oracle's 1005 older renders are the same bits: the whole corpus, in release), and a lane that two modulators hear is prepared once (a pattern that played twice would
    go twice as fast: `a_lane_that_a_modulator_hears_puts_out_the_bits_that_it_puts_out_when_nobody_hears_it`, which has two modulators on one lane, a pattern, a range and a transpose). `process_events` first prepares the lanes that a free modulator hears, and then ticks each free modulator with its lane's
    `heard`, the notes of its group's members (`build_group_midi`, as a group's own modulators are given them) or every event. **A block that ends early** (a plugin that fails) does not reach the heads after it, so the flag that says a lane is prepared is cleared for every lane after the plan has run, and the next block does not
    use the notes of the one before (`a_block_that_ends_early_leaves_no_lane_prepared_for_the_next_…`: found by reasoning, and proven by the test that failed before the clearing was there). `GraphCommand::SetModulatorEar { location, index, ear }` sets it, and **an envelope that is made to hear other notes lets go of the ones it held**
    (`Modulator::let_go`: it goes to its release and holds no note, so that it does not stay open on notes whose end it will never hear: a note held in the attack, the decay or the sustain). `Facts::ears` holds the engine's ears to the model's after every block (`testutil::model_ears`), and no block asks the allocator for memory
    (`modulators_that_hear_a_lane_and_a_group_do_not_ask_the_allocator_for_memory`).
  - *The model and the controller.* `Intent::SetModulatorEar { node, ear }` (`{"type":"setModulatorEar","node":N,"ear":M|null}`) is refused, with nothing changed, for a node that is no modulator (`only a modulator has an ear`; until step 96 `only a modulator that is in no lane and no group has an ear: a lane's or a group's hears its own`), for
    an ear that is neither a lane nor a group (`only a lane or a group can be heard`) and, as for every intent, for a node that is not there. **What it hears already changes nothing and tells the audio thread nothing**: a lane that was taken out and is still named by the model is not forgotten by a request for every note, so that
    Undo of its removal gives the ear back. It is told as a snapshot (`NodeKind::Modulator { source, hears }`: the graph says whom a modulator hears if that is a lane or a group that is there, and nothing for every note), and it is a step of the history ("make an LFO hear the lane of Oscillator", "make an envelope hear every note":
    never merged, none for what is as it is, and a step is undone to what the graph said when it was made, so that an undo never names a lane that is not there). The loader and the journal send the ear with the modulator (`modulator_slot_commands`), so that a modulator that Undo puts back hears what it heard.
  - *The file.* `hears = "instrument[0]"` or `hears = "group[0]"` on a top-level `[[modulator]]` (`ModulatorConfig::hears`, `SaveModulator::hears`; written only when there is an ear, for every kind of modulator); a place that is no lane or group of the session is no ear and the log says so (`modulator[1].hears: instrument[7] is not in the
    session: it hears every note`); the key on a lane's or a group's modulator is not read. A save writes what a load reads.
  - *The terminal.* `h` on a modulator opens the popup "Whose notes it hears" (`EarState`: every note, `L1 Oscillator` …, `G1 Group 1` …, the one it hears marked) and asks `SetModulatorEar` with the choice; the tree says `(free, hears L2)` or `(free, hears G1)`.
  - *The page* (`docs/web-ui.md` D93). Every modulator has a **♫ button** in its header (`HearsMenu` in `app/patch/HearsMenu.tsx`: named `Whose notes LFO 1 hears: every note`, lit while it hears a lane or a group) with a menu of every note and of the lanes and the groups of the canvas (`client/hears.ts`: `earItems`, the
    one it hears marked; a lane that only holds effects, which has no card, is left out); a choice asks `Editor.setModulatorEar`, and the page says why if the server refused it. The mock engine does what the controller does (`setModulatorEarOutcome`; `withoutLostEars`: the graph says nothing of an ear that names a lane that was
    taken out; a control that follows the notes hears its lane's or its group's, `#listened`), pinned by a script of `history.json` (a modulator made to hear a lane, a group and every note, the lane taken out and put back, what is refused and what is as it is: six steps, undone and redone).
  - *The oracle.* `freeear` (a corpus session: four modulators of the session's own, one hearing a lane that has a pattern, a key range and a transpose, one hearing a lane that has a key range and a key control, one hearing a group and an envelope that hears every note) and `edited:freeears` (18 edits, each made: a modulator is made to
    hear lanes and groups and every note while a chord is held, the lane that it hears is taken out and put back by Undo: `FREE_EAR_EDITS`, `Edit::Ear`) make six renders: 1011 in all, and the 1005 that were there are the same bits.
  - *Why.* Everything that a lane's or a group's modulator does, a modulator of the session's own can now do (a lane's hears the lane's notes, a group's its members': that was the one thing that was missing), so that every modulator can be global: step 94 made it so (the next bullet): what the page and the terminal make is one of the session's own with the ear of the knob's lane
    or group, and the modulators of a loaded session are moved into the session's rack with the ears they had; step 96 took the lane's and the group's racks out of the engine, the model, the controller, the terminal and the page (`docs/patch-graph.md` §11.6, P5c: see "The racks go (step 96)" below).
- **Every modulator is global** (step 94, P5c-2 of `docs/patch-graph.md` §11.6: the user's "I think all modulators should be global like this" of 2026-10-27 and, after "If I create an lfo from a parameter on an osc, I can still only connect it to that osc and its lane?", "Lets start on 94"). What
  the page and the terminal **make** is a modulator of the session's own, and what a program **loads** is made one: a lane's LFO, a knob's "New LFO" and a loaded file's `[[instrument.modulator]]` are the same kind of node, which hears one lane or one group (its ear) and can be drawn to any plugin of the session and to the settings of any other
  modulator. (Step 96 took the lanes' and the groups' racks out of the model and the engine: see "The racks go (step 96)" below; the bullets of this entry are as step 94 built it, with the places that step 96 changed said again.)
  - *What is made is global.* `AddFreeModulator { kind, hears }` makes one of the session's own that hears the lane or the group `hears` (refused, with nothing made and no id used, for an ear that is no lane or group: the words of `SetModulatorEar`); `Modulate` makes one that hears the lane of the plugin (a lane's instrument or an effect of its chain), the group of a bus effect, and for a setting of a modulator of the session's own what that
    modulator hears (the sibling that drives it). The "+" of a lane and of a group asks `AddFreeModulator` with the lane or the group, the knob's `~` menu asks `Modulate`, and the terminal's `m` does both (on a lane or a group, and on a parameter); `Controller::add_modulator_hearing`
    is the one place that makes the node, its ear and the commands (`add_modulator` is it with no ear). A step of the history in the words of the button (`add an LFO`, `modulate resonance`: one step for the modulator and its cable, which Undo takes out and Redo makes again hearing what it heard).
  - *A loaded session is moved* (`SessionConfig::move_modulators_to_the_session`, which `session::parse` calls, so that the terminal, `play` and `serve` alike have it; until step 96 `run_session` called it after `session::load`). Every modulator of a lane (and, for older files, of an effect: they were merged into the lane's rack) and of a group is moved to the session's own list, in front of the file's own `[[modulator]]` tables, the lanes' first in the order of
    the lanes and then the groups', each hearing what its rack heard (`hears = "instrument[N]"` or `"group[N]"`), and its cables are written as a free modulator's are: a plugin by its place in the session (`to = "instrument[0].effect[1]"`) and a modulator by its place in the new list (`mod_rate`, `mod_attack`…, `mod_depth = [m, c]`), a group's cable to a member (`member`) to the member's lane's place
    and one to the bus (`bus`) to `group[G].effect[B]`. A place that the loader would have left out is moved to one that no modulator is at (`NOT_THERE`), so that it is left out after the move as it was before it, and the random LFOs keep their seeds (the modulators are made in the order that they were). **The file that is saved is the new one** (`[[modulator]]` with `hears`, no `[[instrument.modulator]]` or
    `[[group.modulator]]`) and is read as it is, without a move: a file that was written with modulators in its lanes is the same session saved the new way (`tracer.toml`'s `[[instrument.modulator]]` becomes `[[modulator]]` with `hears = "instrument[0]"` and `to = "instrument[0].effect[0]"` on its cable); the old form is read for ever. Since step 96 every load is moved (`session::parse`; `session::parse_raw` reads a file as it is written, which the tests of the move itself use, and
    the loader refuses a lane or a group that still has modulators of its own: there is no rack for them to go to).
  - *What it changes, on purpose* (D94 of `docs/web-ui.md`: the user may overrule them). **(1) A lane's, a group's and a free modulator on one parameter add up** (a lane's won over a group's, which won over a free one's, each from the value that the user set: they were racks, and the nearer one wrote after the others; every modulator is a free one now, and the session's own are added). **(2) A group modulator's cable to a member's
    modulator acts a block later** (the cables between the session's own are resolved after all of them have ticked, `apply_cross_mod`; a group's were applied before its members' ticked). **(3) Taking a group out, or a lane out of one, no longer takes the cables of what were its modulators to the members with it** (a cable names its plugin by node, and a modulator of the session's own keeps it wherever the lane is): a group that is
    taken out leaves the modulators that heard it, hearing every note then (the model keeps the group's node as the ear, so that Undo gives it back), and a lane that is taken out leaves the modulators that heard it, with no cable to the lane's plugins (those go with them, as every cable to a plugin does) and hearing every note (modules stay, as an effect does when its instrument goes). Held (at step 94) by
    `a_moved_session_adds_up_a_lanes_modulator_and_one_of_the_sessions_own_on_one_parameter_where_the_lanes_won_in_the_one_that_was_not` (a lane's LFO and a free one of a quarter of the range each sounded as one of a half, bit for bit, and as the lane's alone where the session was not moved), `a_moved_session_has_a_group_modulators_cable_to_a_members_modulator_take_effect_a_block_later_…` and
    the three controller tests of a group taken out, a lane leaving its group and a lane taken out (`app/apply.rs`); the first two compared a moved session with the one that was not, which went with the racks at step 96: the sum is held by `two_free_modulators_on_one_parameter_add_up`, the block later by the stored hashes of the renders that have it, and the third decision by the controller's tests, which are still there.
  - *The oracle.* `Scenario::moved` (the corpus was moved, as the program loads a session; `unmoved()` was the session as it is written, which the loop that the plan replaced and the benchmark ran: `is_in_the_old_loop` was false for a moved one; **both went with the racks at step 96**: every scenario is loaded as the program loads a session, and the loop that the plan replaced is held to the four always-on scenarios that have none of what it lacks, see "The oracle"). The edits do what a front-end does: `Edit::AddModulator` is `AddFreeModulator` hearing the lane, and `Edit::RemoveModulator` takes out the modulators that hear it.
    **A moved session rendered what it was moved from** (step 94): `a_moved_session_renders_what_it_was_moved_from` rendered the always-on corpus both ways and compared every sample and what every modulator publishes, bit for bit, block by block (with the cables that the decisions change pointed at nothing in both, the edits that depend on whose a modulator is left out, and what the modulators publish not compared where a lane is taken out, which leaves the modulators that heard it), and the whole corpus in
    `the_whole_corpus_of_moved_sessions_renders_what_it_was_moved_from` (`#[ignore]`d: release, 1011 renders in 18 s); with no rack left to render it from, the two tests are gone, and what they held is what the stored hashes hold. What the stored files hold changed all the same: the cells of the telemetry are in another order (the cells of a modulator are listed with the session's own: 648 of the 1011 renders differ in the hash of the telemetry and in nothing else) and **30 renders in 11 sessions sound other than they did**:
    `session:cross` and the generated sessions 38, 70, 82, 135, 204 and 279 have two racks on one parameter or a group's cable to a member's modulator (decisions 1 and 2), and 202, 209, 241 and 276 have a group taken out or a lane put in or out of one among their edits (decision 3). `src/oracle/exact/macos-aarch64.txt` and `src/oracle/fingerprints.txt` are written again with them (the edits' counts are what they were: 1769 applied, 238 skipped, 36 refused).
  - *The terminal.* The tree lists a modulator under the lane it hears, as a lane's rack was (`├─ ~ LFO 0.5Hz sine → Oscillator: detune (4%)`, a plugin of its own lane by its name and the others tagged `L2 Filter: cutoff`), one that hears a group after the group's bus effects as a group's was (`M0 Oscillator: detune`, `Bus Reverb: damping`, tagged `(bus)`), and the ones that hear every note
    first, ending `(free)`; an entry is addressed by its place among the session's own (`TreeAddress::FreeModulator`; the entry belongs to the lane or the group that the modulator hears: `State::heard_by`, `lane_of` and `group_of`, so that `v`, `M`, `S`, `w`, `R` and `i` on it go to that lane or group). `m` on a lane or a group adds an LFO that hears it, and `m` on a parameter opens the popup with a new modulator that hears what the parameter is of (`ModulateState::rack` was where the "attach" choices were, the one rack since step 96) and with every modulator to attach, each with what it hears (`attach → LFO 0.5Hz sine (hears L2)`). `m` on a modulator adds one that hears what it hears (every note, if it has no ear: step 96; at step 94 it was every note).
  - *The page.* The "+" of a lane and of a group and the "New …" of a knob's `~` menu make a modulator that hears the lane or the group, **laid out above that lane's or group's row, as a lane's modulator was** (`ownersOf` in `toFlow.ts`: a modulator is in the strip of its parent, or of the lane or group that it hears, and in the row above the first lane only if it hears every note). The places that the canvas remembers keep a modulator where it is
    while it hears the same, and it moves above the strip of the other when it is made to hear one (the id of what it hears is in its slot). A session as the program has it, once loaded, is laid out exactly as it was with the modulators in their racks (`toFlow.test.ts`, over the fixtures and 400 graphs made up). A group's modulators keep below the row of the ones that hear every note, where they are in the column of one of them (the group is level with
    its members, which can be as high as the first lane: found by a graph made up, which put a modulator of the group over one that hears every note). Every modulator has the ♫ menu, lit for what it hears; the "×" of a lane or a group says that the modulators that hear it stay and hear every note (`Removal.hearing`), and the module of a lane that only holds effects does not take the lane out with it when it is the last lane of the session, which a session keeps (`heldIn.alone`:
    it was refused). The mock engine's scenes are the sessions as the program has them once loaded (`client/moved.ts`: the modulators of a lane and a group are the session's own and hear them; the ids and the slots are the scene's own), so that a page in front of the mock is what a page in front of the program is, and the specs against the mock and against the program say the same things.
  - *Held by tests.* Rust: the move's (`session.rs`: six on the `RACKS` fixture; `app/load.rs`: five: every kind of rack, the file that is saved and read again, a file read without the move), the controller's (`AddFreeModulator { hears }` and its refusals, `Modulate` for a lane's, a group's and a free modulator's, a step of the history, a group taken out, a lane leaving its group and a lane taken out), the terminal's (the tree,
    the popup, `m`, `h`; the snapshot of the modulate popup is written again) and the oracle's (above). The page: `moved.test.ts`, `toFlow.test.ts` (the layout, over the fixtures and graphs made up), `modulate.test.ts`, `editor.test.ts`, `selectors.test.ts`, `PatchView.groups.test.tsx` (the words), `engine.test.ts`; `history.json` is written again from the controller. Browsers: the specs that made a lane's modulator and drew a cable from it to a modulator of another lane or to its
    knobs are the specs of modulators that reach all of them now, the real program's read the file the new way, and two are new (a modulator made from the "+" of a lane and from a knob hears that lane and is drawn to a knob of the other, on the mock and on the program, whose knobs move).
- **Held by mutants** (step 94): 99 mutants of the logic of step 94, in two runs: 55 of the Rust (the move's 24, the controller's 12, the terminal's 18, the program's call of the move one more, by hand against `tests/serve.rs`) and 44 of the page (the layout by the ear 14, the mock's scenes 11, the mock engine's 8, the editor's, the removal words' and the "+"'s 11), one at a time against the unit tests that should catch them: 91 were caught by the tests that were there and 8 survived. **Five gaps of the Rust, each a test now**: the stage of an envelope (`mod_release`) of a lane's second modulator
  (the fixture had the rate only, and a cable to the first modulator's is where the unmapped number is as well), which lane is a group's member when there are two groups (the second group's first member is the lane that is in the second group), the attach list of the modulate popup of a bus effect's parameter (the popup's `rack`), the same for the settings of a modulator in a lane's rack (a session that was built by intents), and `m` on a modulator of the session's own, which hears every note and not what the first hears. **Two of the page**: the slot of
  a modulator that hears a group (its column goes with the group's, which a test of a lane growing past the group shows) and the focus after the "×" of a modulator that hears a lane (to the "+" of that lane, which made it); and **one is no fault** (`removalOf` counts the modulators that hear a lane or a group among those of the session's own, which have no parent: a modulator in a rack has no ear, as `check_ids` says).
- **Held by mutants** (step 91): 87 mutants of the logic of step 91, run one at a time against the unit tests that should catch them (the engine's free section and what it finds 23, the model's reach and searches 18, the controller's removals and words 6, the loader's places 10, the saver's and the file's 8, the graph's and the layout's 8, the terminal's labels and keys 14): 77 were caught by the tests that were there, one only by a hang (a test that never finishes, killed after 200 s: the free rack taken out of `modulators_mut`) and 9 were gaps, each now a test: the sum of the cables on a parameter held to its range, two parameters of one plugin each by its own cable, the tempo that a free LFO goes by, the free modulators' nodes after the sources', a cable to `instrument[1]` and one to `group[1].effect[0]` (two mutants: the loader read both places as the first of their kind), what a free modulator is aimed at being a plugin that is there, and `d` and the pane of the second free modulator (two mutants: the tests had the cursor on the first); six tests and one assertion, and each of the nine is caught by the one that was written for it. The run was stopped once by a full disk: cargo's incremental caches had grown to 33 GB over the 87 builds, and a runner that was killed by SIGTERM left a mutant in `model.rs` (found by checking every mutant's text against the files, put back by hand); the runner puts the source back on SIGTERM now and the run script prunes the caches between areas.
- **Held by mutants** (step 92): 34 mutants of the page's logic, in two runs, one at a time against the unit tests that should catch them (`reach.ts` and `modulation.ts` 12, the mock engine 9, the editor 2, the layout 8, the "×"'s focus 1, the bar's menu 2): 29 of the first 33 were caught by the tests that were there and 4 survived, three gaps and one that was no fault. The gaps, each a test now and caught by it: the mock's node for a modulator that is in no lane goes after the lanes' nodes and before the master and the next after the last of its own, whether or not the last lane has a modulator; a control of the session's own that follows the notes hears every note, whatever the lane's keys and transpose are; the node that the editor answers with is the one that has no parent. The no fault, which is out of the code: the carry-over's anchor for a modulator that has no parent and for nothing else, where the server sends no other node with none (it goes by the first lane for any node with no parent now). The mutant that was added with the editor's tests, the last new modulator whatever its parent, survives and is no fault: the graph lists the modulators that are in no lane after those of the lanes, so the last new one is the one that was made
- **Held by mutants** (step 93): 117 mutants of the logic of the ear, in two runs (74 of the Rust: the engine's 23, the model's 11, the controller's 6, the history's 8, the journal's 1, the loader's 6, the saver's 2, the file's 5, the graph's 2 and the terminal's 10; 43 of the page: the items and the words of `hears.ts` 11, the menu's 8, the layout's 1, the editor's 3, the history's 5 and the mock engine's 15), one at a time against the unit tests that should catch them: 110 were caught by the tests that were there and 7 survived. **One** (the flag that says a lane is prepared, which the head clears) was caught as soon as the whole engine's and the oracle's tests ran with it: the ear's own tests all have a free modulator, whose clearing of the flags after the plan hides the head's, and the head's matters for every session that has none. **Three are no fault**: the menu's check that a modulator is in no lane, which `earItems` makes again (a lane's modulator has no items, so no button); `Number("every-note")`, which is `NaN` and so `null` on the wire; and the early exit of `withoutLostEars`, which only keeps the snapshot the same object. **Three were gaps**, each caught now: the `id` of the ♫ button (a test), and two that showed that the shape that the history fixture writes did not say whom a modulator hears, so that the mock's undo of an ear was not pinned (`ears` is in the shape, in Rust and in the page, and the history test compares it).
- **The racks go** (step 96, P5c-3 of `docs/patch-graph.md` §11.6: the user's "Yes, les do the dimming and then 96", the plan agreed at step 94). A lane has no modulators of its own any more, and a group has none: **every modulator is the session's own** (`SessionModel::free_modulators`: the one rack) in the
  model, the engine, the controller, the terminal and the page, and what a file's `[[instrument.modulator]]` and `[[group.modulator]]` say is read and moved when it is loaded (`session::parse`, as step 94 made it) and not written. Nothing that a person did changes but where it is said below, and the oracle's whole corpus renders the same bits
  (the stored hashes are untouched: `the_whole_corpus_is_what_it_was_and_the_same_twice`, in release).
  - *Gone.* From the model `InstrumentNode::modulators` and `GroupNode::modulators` and `ModulatorLocation::Lane` and `Group` (the enum keeps the one variant, `Free`, as the place that the ten commands that address a rack by `location` and `index` name, until P5c makes them address a modulator by its node),
    `ModTargetKind::{PluginParam, GroupMember, GroupBus, GroupMemberMod}` (what a rack reaches is `FreeParam` or a sibling's field, and the file names a plugin by its place: `to = "instrument[0].effect[1]"`), `group_cables_into_lane`, and the loader's `load_lane_modulators`, `load_group_modulators`, `plan_lane_target` and `plan_group_target`; from the engine
    the lanes' and the groups' racks and the group-modulator pass of `AudioGraph::process` (with the publishing of the members' cables that went with it); from the intents `AddModulator { parent, kind }` (`addModulator` in the generated `Intent.ts`: no front-end asked it any more, and `AddFreeModulator { kind, hears }` is the one that makes a modulator); from the saver
    the writers of `[[instrument.modulator]]` and `[[group.modulator]]`. In `src/` (the tests included) 9,293 lines went and 2,434 were written (`model.rs` 5,912 to 3,427 lines, `chain.rs` 14,997 to 13,444, `controller.rs` 5,274 to 4,539, `tui/mod.rs` 5,226 to 4,834, `session.rs` 4,389 to 4,058, `app/load.rs` 3,923 to 3,618), and on the page (the tests included, and not the fixtures) 1,513 went and 1,114 were written
    (the mock engine's rack branches, `client/membership.ts` and `client/moved.ts` with their tests, `ownerOf`, `NEARNESS`, `Removal.modulators`), apart from the fixtures that Rust writes again (the ids of a loaded session's modulators come after its lanes' now).
  - *What changed that a person can see or do* (D96 of `docs/web-ui.md`; the user may overrule them). **(1)** `m` on a modulator, in the terminal, adds one that hears what it hears (a lane, a group, or every note if it has no ear); at step 94 it added one that hears every note. **(2)** The keys that are of "the lane or the group that the selected entry is of"
    (`M`, `S`, `w`, `v`, `R`, `i`, `g` …) go, on the entry of a modulator, to the lane or the group that it hears (`State::heard_by`, `lane_of`, `group_of`); on one that hears every note they have nothing to go to and do nothing, as `a` on a modulator did not do anything before. **(3)** The terminal's selectors name a plugin by its lane or group
    (`L1 Oscillator: cutoff`, `G1 Filter (bus): cutoff`) and a cross-modulation target by its place in the session's rack (`Mod 1 rate`), where a lane's rack's were named by the plugin's own name and a place in the rack. **(4)** `SetModulatorEar` is refused for a node that is no modulator in the words `only a modulator has an ear`
    (it was `only a modulator that is in no lane and no group has an ear: …`), and for an ear that is neither a lane nor a group in the same words as before (`only a lane or a group can be heard`). **(5)** `SetLaneGroup` leaves the cables of the modulators that hear the group where they are (a cable names its plugin: D3 of step 94, for a lane that goes to a group
    or leaves one) and is always a step of the history (`take a lane out of its group`, `put a lane in a group`: it was a barrier where the lane's group drove it); `AddGroup` of a lane that is in a group is still a barrier. **(6)** The page's "×" of a module that only holds effects takes the lane with it when it is on its own, whether or not modulators hear the lane
    (they stay, hearing every note: `Removal.hearing`; `Removal.modulators` is gone, since no modulator is in a lane to be taken out with it).
  - *The oracle.* The loop that the plan replaced had racks of its own (the engine's were its) and has none now, so that it can run the sessions that have no modulator only: 4 of the always-on scenarios and the same share of the whole corpus (see "The oracle", "Two engines agree"); the rest, which have modulators, are held to the stored hashes
    (and the plan was held to the loop for them at steps 70 to 94, when the loop could still run them). `Scenario::moved`, `unmoved()` and the two tests that compared a moved session with the one it came from went with the racks they compared. The benchmark's stress session, whose 32 modulators are all the session's own now, takes 833 µs a block of 256 frames with a chord of six
    (15.6% of the 5.33 ms), 365 µs with nothing played and 833 µs with every lane muted, and no block asks the allocator for memory (at step 94 the moved session took 811 µs and the one that was not moved 806 µs: the difference between two runs, on a machine that other programs share).
  - *The page.* Every modulator has `parent: null` and, if it has an ear, `hears`; `reachFrom` is every plugin of the session and `modulatorsInReach` every other modulator, for any modulator (`client/reach.ts`); `modulationsOn` adds every driver; the mock engine has no rack to put a modulator in (`withModulator` puts it after the last one, or before the master; a lane is put before
    the modulators, `endOfLanes`; `Modulate` makes one that hears the lane or the group of the knob or, for a setting of a modulator, what that modulator hears); `Editor.addModulator` is gone (`addFreeModulator(kind, hears)` is the "+" of a lane and of a group and the bar's "+ Modulator"); the ♫ menu is on every modulator.
  - *Held by tests.* Rust: about 200 tests were about the racks themselves and are gone, about 35 were written and the rest were ported in place (a lane's modulator is one of the session's own that hears the lane); new ones hold the decisions above: `a_lane_that_leaves_its_group_keeps_the_cables_of_the_modulators_that_hear_the_group`, `taking_a_group_out_leaves_the_modulator_that_heard_it_and_its_cable_to_the_member_and_undo_gives_the_ear_back`,
    `m_on_a_modulator_adds_another_that_hears_what_it_hears_and_one_that_hears_every_note_gets_one_that_does`, the loader's refusal of a lane's, an effect's and a group's modulators (`a_session_that_still_has_the_modulators_of_a_lane_an_effect_or_a_group_is_refused_with_the_place_and_the_same_file_moved_loads`) and a test that pins every modulator of six moved sessions (`every_modulator_of_a_moved_session_is_the_sessions_own_and_drives_what_it_drove_and_hears_what_its_rack_heard`: whom it hears and
    what each cable drives, in the order of the rack, as the file said it before the move). The page: the same tests through the session's own modulators, the fixtures written again by the controller. Rust 2420 tests (2606 with the `web` feature, 2414 without the default plugin formats, 2600 with `web` and without them), web 2770 in 120 files, browsers 195, the oracle's 1017 renders (the stored hashes untouched).
- **Held by mutants** (step 96): 76 mutants of the logic of step 96, in two runs, one at a time against the unit tests that should catch them: 67 were caught by the tests that were there (none only by the build) and 9 survived. **Rust (40):** the model's reach and searches 18 (`kind_of` 5, the lists of targets 4, `locate_edge`, `free_cables_into`, `cables_into_plugin`, `depth_cables_of`, the cables that address a modulator 2, the ear 3), the controller's removals and `set_lane_group` 5, the ear's intent 2, the loader's refusal 3, `session::parse`'s move 1, the terminal's `heard_by`, `lane_of`, `group_of`, `selected_lane_or_group` and `m` 7 and the engine's ears and cross-mod 4: 35 caught and 5 survived, **each a gap and a test now**
  (`kind_of` for the instrument of a lane that has none and for the depth of a modulator's own cable, `modulators_in_reach` leaving the modulator itself out, `cables_into` for a cable of the modulator's own, the boundary of the check that an ear is a node that was made: three tests; the rerun of the five is 5 caught). **The page (36):** `reach.ts` 5, `modulation.ts` 2, `hears.ts` 2, `selectors.ts` 3, the "×" words and the ♫ menu 5, the layout's `alone` 3, the history 4, the editor 2 and the mock engine 10: 32 caught and 4 survived with no test to write: **two are guards that only the type check holds** (`hears` on a node that is not a modulator, in `hearsOf` and in `removalOf`:
  `tsc` says `Property 'hears' does not exist on type 'Node'`, and Vitest does not type-check) and **two are equivalent** (the editor's added node is the one node that the intent adds, and the mock's `lanePutIn` is idempotent for a lane that is in the group already).
TODO: a loaded session is moved and saved the new way, so that the first save of a file that has `[[instrument.modulator]]` or `[[group.modulator]]` changes it (the old form is read for ever): a person who keeps one in a repository may not want that, and a flag or a key of the file that says "do not move" would be small.
TODO: `ModulatorLocation` has one variant and the word "free" is in the names of what was the third rack (`Free`, `free_modulators`, `AddFreeModulator`, `FreeParam`, `FreeModulator` in the tree's address, `load_free_modulators`, `e2e/freemodulators.spec.ts`), and the commands that address a modulator still name `location` and `index` (the order of the rack is the layout, and the oracle holds the engine's rack to the model's): P5c, which takes the lane out, makes the commands address a modulator by its node and takes the enum and the word out. A few test names and comments still say `rack` of what a lane had or `free` of what is every modulator (`the_rows_of_a_free_modulators_pane_are_edited_as_a_lanes_are_and_the_audio_thread_is_told_by_the_free_rack`).
TODO: the page's `ownersOf` in `toFlow.ts` still falls back to `node.parent` for a modulator that has a parent, which no server makes since step 96 (every modulator has none), and the made-up graphs of `toFlow.test.ts` still build modulators in the old form with `modulator(id, parent)` (the layout of a modulator that is in a lane's strip): the fallback and those cases can go together.
TODO: a lane or a group that is taken out leaves the modulators that heard it, with no cable if they drove its plugins and hearing every note (modules stay, as the effects of a lane do when its instrument goes): the "×" says so, and a modulator that has nothing to drive and nothing to hear is not offered for removal with the lane ("remove what only this lane used" is not built).
TODO: the row of free modulators is above the first lane, and a cable from one to a knob of a lane far down runs a long way over the nodes between (a cable is drawn over the nodes it passes, as every cable of modulation is); a free modulator can be put where it is wanted like any node (it has a `ui`), and "Arrange" puts them back in the row.
TODO: a free modulator hears every source and every note unless it has an ear (step 93), and an ear is a lane or a group, not a source: one keyboard's notes only (a cable of notes from a source into a modulator, as a lane has) and a key range of its own are not built; and a free MIDI control that has no ear does not tell the member channels of an MPE controller apart (it follows the last message of any channel; one that hears a lane is given the lane's, as a lane's own is).
TODO: the ear is a menu on the modulator's node and a key of the file: the canvas draws no cable from a lane to the modulator that hears it (a derived edge, as the link by which a lane hears every source is, would show it, and a cable dragged to the modulator would draw it), and the Patch tab has no row for it.
TODO: a modulator that is made to hear something else while the lane that it heard is taken out is put back by Undo hearing every note (what the graph said then), and the Undo of the lane's removal then gives it no ear back; the mock engine, which puts back the whole graph, gives the ear back: the one sequence that the two do not agree on (the removal and its Undo with nothing between agree: the model keeps the ear). A journal of the ears that a lane or a group had when it went (`Inverse::Lane` and `Inverse::Group` would carry them) would end it, and the model's special case for an ear that names a node that went (`ear_of`) with it.
TODO: the mock engine's modulators that hear a lane have the lane's keys and its transpose, and not its remap or its pattern (the mock has neither), and an envelope that hears a group or a lane opens on any note that they take.
TODO: the terminal's pane names the depth of a free cable by its parameter alone (`depth cutoff`), where the tree names the plugin as well (`L1 Filter: cutoff`), and the loader's label for the depth of a cable to a sibling is `Mod 0 depth 0` as a lane's is (a pre-existing wording).
TODO: the cost of a modulator is the benchmark's now (its stress session has 32, all the session's own: 833 µs a block of 256 frames with a chord of six, which is what it was within the noise between two runs): each block a modulator finds the plugin of each cable by its node, a search through the lanes' chains and the buses, which nothing yet makes cheaper than that.

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

### Node positions (`ui`)

A front-end that draws the session as a canvas (the browser, see "Web UI") keeps where each node is in the session
file, as a `ui` table on the node's own table. The engine and the TUI do not use them: the TUI does not show them,
and keeps them through a load, an edit and a save.

```toml
[ui]
master = { x = 1200, y = 300 }  # the master mix has no table of its own: its place is here

[[group]]
ui = { x = 800, y = 40 }

[[group.effect]]
plugin = "builtin:reverb"
ui = { x = 1100, y = 40 }

[[instrument]]
plugin = "builtin:osc"
ui = { x = 0, y = 0 }

[[instrument.effect]]
plugin = "builtin:filter"
ui = { x = 376, y = 0 }

[[instrument.modulator]]
ui = { x = 376, y = -200 }
```

- `x` and `y` are pixels on the page's canvas: finite numbers, whole or with decimals (saved as typed: a whole
  number has no point, `12.3` is not `12.300000190734863`). A node without `ui` is laid out by the page.
- A `ui` that is not a pair of numbers (`ui = 5`, `ui = { x = 1 }`, a `[ui]` that is not a table) is ignored, with
  a log line: a layout is not worth refusing a session for. A number that is `nan` or `inf` is refused like every
  number.
- The places are in the model as a side table (`SessionModel.ui`, by `NodeId`; the master's under
  `NodeId::MASTER`), read by the loader (`app::load`) and written by `app::save::to_save` and `session::to_toml`.
  The place of a node that is taken out stays in the table (ids are not reused but for what undo gives back, so nothing
  else reads it) and is not written.
- Older sessions nested `[[instrument.effect.modulator]]`: such a modulator keeps its `ui` when it migrates into the
  lane's rack.

### Arranging the nodes (`web/src/app/patch/arrange.ts`, steps 98 and 98b)

The user's "we need to rethink the arrange into something smart that minimizes the length of cables maybe (we want some distance of cables for spacing, but I think we should try to minimize and see how it turns out)" (2026-10-28, after step 96; the first half of that message is step 97). The button
**Arrange** of the canvas (`PatchView`) used to put every node where the layout of the rows has it (a strip for each lane with its modulators above it, the groups after the longest lane, the master last: `toFlow`'s `layout`, which is steady and long) and give back the places that a person had moved.
It now puts them where **the cables between them are short**, with room for each cable. A session is still opened in rows, which is also what keeps nodes where they are while it changes, and nothing is saved (see the TODOs): the arrangement is the page's own.

Step 98b is the user's "do you think its possible to arrange from this [Image #2], to this? [Image #3] just looking at the 2 osc?, all otherthings looks awesome, if possible we do it but not a big deal, then lets go on the microphone" (2026-10-28, after step 98; the first picture was what step 98 made of the user's `bell-2.toml`, Oscillator 2 below and to the right of Oscillator 1 with the cable of the first to the first input of the Math along the top of the second, and the second the two oscillators one over the other in a column, the Math to the right between them and no cable over a node). It was possible, and not by a nudge: **the cost of step 98 liked the staggered layout better** (about 1,819 for the column against 1,354 for the stagger, by its own measure: the cable of the modulator that drives a knob of both oscillators ended at the middle of the top of each, and had to come down through the first to reach the second), and the search, which looks at the nodes one at a time in the order of the graph, also ended in places that were not the best there are (a modulator that drives nothing wedged between the two, the oscillators swapped with the two cables to the Math crossed). Five changes (below), and **`bell-2` is now what the second picture shows** (Oscillator 1 at the top, Oscillator 2 under it with 31 pixels between them and 48 to the right, so that the cable of the modulator comes down beside the first, the Math to their right between them, the modulators above, and the chain Reverb, Delay, Master in a row: 5,191 pixels of cable in the rows, with six cables over nodes, 1,438 and none over a node arranged).

- **`arrange(blocks, wires, fixed)`** is a pure function of the nodes (a `Block` is a box: an id, a place, a width and a height) and the cables (a `Wire`: the two nodes, the handle that each ends at as `handleOf` names it, and a weight), and nothing else: it uses no random number (the starts of step 98b have a generator of their own that every browser runs alike), looks at the nodes in the order of the graph and, in the starts, in others, makes a fixed number of steps
  (`BUDGET`, 150,000 places priced, and not a time) and puts every node on a grid of `GRID` (8) pixels, so that **the same session is arranged the same way in every browser** and a test can say what it comes to. It starts from the places that came in (the layout of the rows) and moves each node, one after the other, to the place that is best for the cables at it, until no node can be moved to a better one
  (at most `PASSES`, 12, goes round; `descend` is that work, `arrange` judges it: what comes out is the places that came in if it is **worse** by `score`, which is what the work prices (the cables' length, `BACKWARDS` times what they are short of their room, `OVER_RATE` times what is over nodes and, since step 98b, `ALIGN` times what the nodes that feed one node are out of line and `INVERT` times what the cables at one node are crossed by; the places are put on the grid, which can cost a pixel or two on a layout that had its room to the pixel), and one that is as good is taken, since it has the nodes that no cable is at drawn in; a node in `fixed` stays where it is to the pixel).
- **What a place costs** (`costOf`): **a cable** is the curve that the page draws between the two places it joins (a cable of audio or of notes leaves the middle of the right edge of its node and arrives at the middle of the left edge of the next, the second input of an effect at two thirds of its height and the first, when it has two, at a third; a cable of modulation leaves
  the bottom of its modulator and arrives at the top of what it drives, in the column of its knob (since step 98b, `toX`): the same cubic curve as `getBezierPath`, in `SEGMENTS` (12) straight pieces), and costs its **length**, and as much again, twice (`BACKWARDS`), for each pixel by which its end is short of the **room** that it should have in front of its start: `GAP_X` (72) for audio and notes and `GAP_Y` (56) for modulation, which
  is where a cable's button and slider go and the spacing that the user asked for, and the one number to turn to have more or less. **A cable that passes over a node that it does not join** costs `OVER_RATE` (1.5) for each pixel of it that is over the node, made `SLACK` (4) larger (a cable is drawn over the nodes it passes, and hides them), so that a node that is crossed by a little costs a little, which gives the search a slope to go down; a node that is moved is also priced for the cables of
  others that pass over it. **No node comes nearer to another than `MARGIN`** (24), and none nearer than `SIDE_ROOM` (32) to the left edge of a node that a cable to the depth of a cable ends at, which are rules and not prices. **A node is drawn a little to where the layout of the rows had it** (`HOME`, 0.03 a pixel), which keeps what is indifferent where it was, and **a node that no cable is at is drawn to the middle of the nodes that have cables** (`PACK`, 0.05), so that it is not left where the others have moved away from (it was a lane
  in the user's `king-hippo-rave.toml` that was left a screen away, in the first try).
- **What is tried** for a node: where each of its cables would have it (its end the room in front of the other's, in line with it), where the middle of them all would, each cable's place along the one that the others agree on, against the edges of the boxes that are near (touching, with the margin: under, over and beside, in line with either edge), and a few steps either way (`GRID`, four and sixteen of them). **A node is moved with the nodes that are held to it** (`heldWith`: those that its cables hold at just
  their room, to a step of the grid, along a cable of audio or notes that leaves it and a cable of modulation that does, and the ones that those hold in their turn, at most `CHAIN`, 12): a row of nodes is slid as one, and a node with the modulators above it, and not left with a node that cannot move without making the cable to its neighbour short (a plateau that moving one node at a time cannot get off: the second try left a lane's modulators behind and the lane a screen below its effects).
- **What step 98b changed** (`arrange.ts`, `toFlow.ts`). **(1) A cable of modulation ends in the column of the knob it drives** (`Wire.toX`, `Anchor.ox`: pixels from the node's left edge along its top edge; `toFlow`'s `cellX` and `controlOf`): a node's controls are a grid of four cells of 72 pixels (`CELL_WIDTH`) in the middle of its 304, and a cable comes down to the middle of the cell of its knob (a parameter by its place among the node's knobs, a lane's volume and an effect's mix by the cell after the knobs, a group's volume and pan by its first two cells; a setting of a modulator and the depth of a cable have none that is told), so that a modulator is put over the knob it drives and not over the middle of the node, and two nodes that one modulator drives can be one under the other with the cable down beside the first (the height of the knob in the node is not told). **(2) The nodes that feed one node are drawn into a column** (`ALIGN`, 2 a pixel; `feeders`, `Measure.stagger`): the sources of cables of audio or notes that matter into one node (not the faint links, not modulation, a node that sends two cables to it counted once) are drawn to have one left edge, as the layers of a drawing of a graph are. **(3) The cables that end at one node on two places of its left edge are not crossed** (`INVERT`, 1 a pixel; `crossings`, `inversion`, `Measure.inverted`): what feeds the first input of an effect that has two, which is the higher, is not lower than what feeds the second, by as many pixels as it is. **(4) The work is made again in other orders** (`STARTS`, 12; `TOTAL_WORK`, 1,500,000 places priced times the nodes): `arrange` runs `descend` in the order of the graph and in up to eleven others that `shuffled` makes (a generator with a state of its own, the same in every browser) and takes the one with the lowest `score` (exported: `length + BACKWARDS · short + OVER_RATE · inside + ALIGN · stagger + INVERT · inverted`; one that is not better than the first is not taken, and what came in is kept if every one is worse); a start is made only while `roomForAnother` says that the work in all stays within `TOTAL_WORK`, so that a session of fifty nodes is one start and one of twenty has all twelve: where a node ends depends on which was moved before it (`descend(blocks, wires, fixed, order, work)` visits the nodes in `order` and adds what it priced to `work`). **(5) A node that a cable to the depth of a cable ends at keeps `SIDE_ROOM` (32) free at its left edge** (`Rect.corridor`, `corridors`; `tooNear` is the rule, in the search and in `crowded`): the page runs such a cable round the node and in at its port on the left edge, 24 pixels from it (`SIDE_OFFSET` of `cableRoute.ts`), so that a node at the margin (24) from the left of it had the cable along its edge, inside its box by the page's measure: **found by the browser suite**, the one spec of 199 that the first version of this step failed (`e2e/build.spec.ts`: "a cable to the depth of a cable goes round the node it ends at, and has its slider in the room under the modulator that drives it, over no node": four points of the cable in the Oscillator, which the new search had put at exactly 24 from the LFO's left edge).
- **The page** (`toFlow(graph, remembered, scopes, { arrange })`): `FlowOptions.arrange` asks for the arrangement in place of the layout of the rows for a layout from nothing, and is left out of the question for a session in which a person has placed a node (a `ui` is a pin that the arrangement would have to keep: the button gives the places back first, as it did). The rows' layout is still worked out (it is what the nodes start from, what the slots that
  `carryOver` knows a node by are made from and what `arranged` is), `wiresOf` makes the cables (a link by which a lane hears every source counts `HEARS_WEIGHT`, 0.25; **a modulator that hears a lane or a group and has no cable of its own is tied to it by a cable that counts `TIE_WEIGHT`, 0.3, so that it is above what it hears**; the first input of an effect that has two is `audioIn:pair`), `blocksOf(graph, flow)` the boxes
  and cables of a layout for `measure` (what the report and the tests say a layout is worth: `length`, `short` (the pixels by which the cables are short of their room), `over` and `inside` (how many cables, and how many pixels of them, are over a node they do not join), `backwards` (how many are short by more than half a step of the grid), `crowded`, `width` and `height`). `useLayout` asks for the arrangement when the button is pressed and when the last of the nodes that a person had placed has been given back, and says `optimized` while
  what is drawn is that arrangement and the session has not changed since, and while there is no session (a person's placed node is never that: `optimized` is false while one is): **the button is bright when a session is opened** (the nodes are in rows), dim once they are arranged, and bright again when the session changes, a node is moved or the nodes are given back. **A node that is added after an arrangement** goes where the rows would put it, moved as far as the node it follows is from its place in the rows (`carryOver`: in width as well as in height since this step,
  and an effect by the effect before it, which is level with it, and not by the head of its strip), and clear of the others; arranging again puts it where the cables want it.
- **What it comes to** (`arrange.report.test.ts`, which was a scratch file of the step: the fixture sessions as the controller makes them, before and after, in the cost's own measure, which since step 98b has the cables of modulation end at the column of the knob, so that the "before" numbers are not the ones that step 98 reported; each percentage is what is left of the length): the tracer 378 pixels of cable to 229 (61 %), the groups' fixture 10,874 to 3,879 (36 %), the free modulators' 8,202 to 3,176 (39 %), the audio cables' 8,404 to 5,302 (63 %), the second input's 3,982 to 1,557 (39 %), the sources' 5,020 to 2,942 (59 %), and a stress session of 49 nodes 57,913 to 38,328 (66 %) and of 97 nodes 146,977 to 108,211 (74 %);
  cables that pass over a node 10, 4, 1, 16 and 59 to none, 0 and 1 (the sources' none to two faint links, the stress session of 97 nodes to one); no two nodes nearer than the margin; the time 21 ms for the tracer, 386 ms for the groups' fixture, 346 for the free modulators', 223 for the audio cables', 50 for the second input's, 97 for the sources', 378 ms for 49 nodes (one start) and 870 ms for 97 (the budget: a session of hundreds of nodes does not take longer than that; those of twenty or fewer are the best of twelve starts in 0.4 s at the most). **The user's `bell-2`**: 5,191 pixels of cable in the rows, 1,438 arranged (28 %), none over a node, the oscillators one under the other, as the second picture of step 98b. Looked at on the user's own sessions (`bell-2`, `king-hippo-rave`, `tang`, `lost-bird`, `ducks`, `test5`, `pump`, `shared-reverb`):
  a chain of effects in a row with the room of a cable between each, the lanes' ends level with what they feed, the modulators above what they drive (over the knob, not over the middle of the node), nothing over a node's controls, the lane that was a screen away next to the others, and the oscillators that feed one node in a column (`king-hippo-rave` is a tall column of them now).
- **Held by tests.** `arrange.test.ts` (37: the anchors, the measure with the room that is missing and a graze of a box's slack, the arrangement of two nodes, a modulator over what it drives, a row of effects, the ends of lanes, a node that is held with its modulator (the master fixed), a stranded node and one that is drawn in when the cables are as short as they can be, a cable that is taken round a node, a node across a cable and a node held beyond it by another cable,
  the weights of the cables, a faint cable that does not move a node, the fixed nodes and the corner, a cable short of its room, a result that is worse than what came in, the curves of what has moved, the same result run again, the grid, circles, the budget and 40 random sessions), `toFlow.test.ts` (ten: the option, every node placed with the margin and the same each time, shorter and over no node, 60 made-up graphs, the ties and the second input, the weight of the links by which a lane hears, the pin of a placed node,
  nothing moves when a cable is drawn, a new effect by the one it follows), `useLayout.test.tsx` (four: nothing to arrange without a session, bright when a session is opened and again when it changes, the places that a person gave given back one at a time, a placed node) and the rewritten tests of the button (`PatchView.move.test.tsx`, `PatchView.stable.test.tsx`); four browser specs rewritten (`layout.spec.ts` twice, `cascade.spec.ts`, `freemodulators.spec.ts`): the rows' layout they asserted after Arrange is the arrangement now.
  Page 2803 to 2854 tests in 122 files, browsers 199 (6.9 m). **Held by mutants** (step 98): 72 mutants of the logic of step 98, in two runs, one at a time against the unit tests that should catch them (`arrange.ts` 52: the anchors, the curve, what is over nodes, the margin and the room, the grid, the judgement, the fixed nodes, the pulls, the costs, the places that are tried first, the nodes that are held, the work and the measure; `toFlow.ts` 9; `useLayout.ts` 8; `PatchView.tsx` 3). The first run, of 65, found 22 that the tests did not catch: 15 survived and 7 were caught only by the type check (a constant that the mutant left unused, which `noUnusedLocals` sees and Vitest does not), and they were **15 gaps, each a test now** (a graze of the slack round a box and the threshold at which a cable is over a node, what is over nodes counted by the cable's weight, the cost's weight, crossing a node in the judgement, a tie taken, the fixed nodes and the corner, the pull to where a node was, the cable of a node over another node, the nodes that are held to a node, the curves of what has moved, the weight of the links by which a lane hears, and two flags of the layout), **one dead term** (crowding in the judgement, which the search never makes: out of the code, and reading it showed that the judgement did not price the room that a cable is short of, which has a mutant and a test now) and **six no fault**: the check that a curve is clear of a box on its last side (a shortcut of the clipping), a place rounded down to the grid and not to the nearest (the search walks the places of the grid by steps either way and comes to the same), the room that a modulator wants and the side of it that a node that receives wants in the places that are tried first (seeds, which the steps correct: the result is the same), a guard for a cable that starts at an input (the page has none), and the button's `!anyMoved`, which `optimized` implies (taken out of the code). The second run, of 72 against the final code, caught 66 (none only by the type check) and the six that survive are the same kinds of no fault, the button's being `flow.arranged`, which `optimized` implies as well and stays, held twice.
- **Held by tests and mutants** (step 98b). `arrange.test.ts` 37 to 51 (the measure's `stagger` and `inverted` with a node that sends two cables to one node, faint cables and modulators that are no column or crossing, and several pairs added up; a cable that ends in a column of its top edge and one that arrives at a side; the one number `score` with each of its weights; the corridor at the left edge in `crowded` and in the search, with the tracer and an envelope on the depth of the LFO's cable; the nodes that feed one node in a column by their left edges though one is narrower; the node that is free being put the right way round with the one that is not, whichever of the two it is; the user's `bell-2` as boxes and cables; what a descent adds to the work done and `roomForAnother` at its edges; the orders of the starts, which are permutations, the same every time, another for each start and with every place taken by several nodes), `toFlow.test.ts` 190 to 193 (the column of each kind of cable: knobs of a lane, an effect with a ninth that a cable drives, a lane's volume after its six knobs, an effect's mix, a group's volume and pan, and none for what the node has not; a modulator over the column of its knob; the corridor on the real tracer), and `build.spec.ts` (unchanged: it found the corridor). Page 2854 to 2871 tests in 122 files, browsers 199 passed. **Mutants** of the logic of step 98b: 68, in two runs (67, and 18 of them again after the tests that the first run asked for: 85 runs), one at a time against the unit tests that should catch them (the column of the nodes that feed one node 9, the crossing of cables 12, the starts 13, the knob's column 21, the corridor 11, the places tried above another box 2). The first run caught 53 (50 by the tests that were there and three only by the type check: a weight or a variable the mutant left unused) and let 14 survive, which were: **six gaps, each a test now** (the weight of the column and of the crossing in the one number that decides between the starts, and in the search; which of the two sources of a crossing is free; the places of the order that a start looks at the nodes in, with the last node not moved; and, in `toFlow`, four cells that were indistinguishable in the first test because a lane with eight knobs has its volume in the column of the first: it has six now, and the test has a volume of an effect, a mix of a lane and a pan of a lane, which the node has not), **one dead guard, out of the code** (a pair of nodes of which both move together was priced once for each, which a move of the whole does not change: the same for the crossing: two `Set`s gone from `costOf`), and **seven no fault**: a start that is as good as the best being taken or not (a tie by the one number), the guard against a start that finds nothing to do after the first has found something (the same test of the same nodes and cables), and the places that are tried first (seeds, which the steps either way correct: the column that a knob's cable is taken off, the two against the edge of another box with the room that the corridor wants, and the two above another in line with its left edge and its right, each the other's when the widths are equal and the two together held by the test of the crossing). The second run of the 18: 11 caught, the seven that the first run called no fault.
- **Decisions** (D98b of `docs/web-ui.md`; the user may overrule them). (1) A cable of modulation ends at the column of its knob and not at the middle of its node: the cell of the grid is told, the height is not. (2) The nodes that feed one node are in a column and the cables at two inputs of a node are not crossed, as the layers of a drawing of a graph are: the user's second picture. (3) Twelve starts in other orders under a budget of work, the best by the one number: sessions of twenty or fewer nodes take up to 0.4 s, those of fifty and more are one start. (4) A node that a cable to the depth of a cable ends at keeps 32 pixels at its left, where the page runs that cable. (5) What was tried and not kept: **snapping the places that are tried against the edge of another box away from it** (to the nearest place of the grid such a place can be inside the margin and is turned away, so that a node cannot get against it): on 60 sessions made up it was better 34 times and worse 23 with 2 % off the mean, and on the fixtures and on `bell-2` it was better and worse by turns (the groups' 7 % worse, `bell-2` 2 % worse), so it is not in; and **a wider slack round the nodes** for what a cable passes over (12, 16, 24 and 32 pixels were tried on `bell-2`: the stack and the stagger took turns, and the result was a different one at each).
- **Decisions** (D98 of `docs/web-ui.md`; the user may overrule them). (1) The button makes the arrangement and a session is still opened in rows (so that every test and every spec of the rows holds, and the user can look at both): making the arrangement what a session opens as is one line in `useLayout`. (2) The cost is a length with a room and a price for crossing: the user's "some distance of cables for spacing" is `GAP_X` and `GAP_Y`.
  (3) The order of the lanes on the canvas is not kept (a lane is where its cables want it). (4) A node that is placed by a person is a pin and the button gives it back, as before. (5) Deterministic and with a budget and not a time. (6) A modulator that hears a lane and has no cable is tied to it, and a node with no cable is drawn to the middle.
- TODO: the arrangement is the page's own: it is not saved (a page that is opened again, and a second browser, have the rows, or the places that were put by hand), since saving it is a `SetNodeUi` for each node, one step of the history each. Two ways to end it, the user's to choose: open a session arranged (the arrangement is a function of the graph, so that every browser has the same, and nothing is saved), or an intent that
  sets the places of many nodes as one step (`SetNodeUi` for a list).
- TODO: a node that is added after an arrangement is put by the rules of the rows and moved clear of the others, which is not where the cables would have it (it is put level with the node it follows, and may be a column from where it should be, or clear of the master below it): arranging again fixes it; arranging the new node alone, with the others pinned (`fixed`), would.
- TODO: a cable of modulation ends at the column of its knob and at the top of its node (step 98b): how far down a node the knob is is not told, so that a modulator is put GAP_Y above the node and the cable, which goes on down to the knob, is longer than the arrangement thinks (`toFlow` knows the rows of the grid), and a cable to the depth of another's cable, which the page runs down round the node and in at its left edge (`cableRoute.ts`), is taken as one to the top, with the room that it needs at the left kept (`SIDE_ROOM`): what its real route crosses is not priced.
- TODO: the search is a descent from twelve starts and not a proof: it can still end where a better place is not found (the groups' fixture is left with a cable 14 pixels short of its room and a pair of cables behind it, the sources' with the faint links of two lanes across nodes), the places that it tries are seeds that the steps either way correct (no test holds each one: they were judged no fault), a node that cannot get past another one that is in its way finds no place past it, and a result is sensitive to the slack round the nodes (see D98b). More starts, or a move that takes a node to the other side of its neighbour, would find more at the price of the time.
- TODO: the heights are the layout's own (`nodeHeight`), so that a node that is taller than it says (a wrapping name, a scope that grew) may touch another; the margin is 24 pixels. A cable's slider and its button are at its middle, which can be over a node (older TODO). Not here: a layout by layers with the cables routed round the nodes, a time stamp on what was arranged, and a choice of how tight the room is (`GAP_X` and `GAP_Y` are constants).
- TODO: `Flow.arranged` (every node is where a layout from nothing has it) is no longer what decides the button: `optimized` is true only when the layout is the arrangement, which implies it, so that `disabled={flow.arranged && optimized}` holds the one guard twice (a mutant that drops `flow.arranged` survives, as it must), and no other code of the page reads it: the field and its twenty assertions in `toFlow.test.ts` could go, and the button would read `optimized` alone.
- TODO: the arrangement runs on the page's thread when the button is pressed (0.4 s for fifty nodes, 0.9 s for a hundred, and up to 0.4 s for a session of twenty, which is the best of twelve starts; nothing noticeable for four lanes): a worker would not block the page, and would let more starts be made.

### Right-click menus, choosing with Shift and moving what is chosen (`ContextMenu`, `contextMenus.ts`, step 107)

The user's "can we add a right click menu system? Also lets have shift+mouse drag to select many things?" (2026-10-30, after step 106). **The page only**: the menus ask the intents that the buttons of the nodes and the bar ask (`Editor`), the controller, the model, the file, the engine and the terminal are as they were.

- **The menu** (`ui/ContextMenu.tsx`, `ContextMenu.css`). A menu at the pointer, drawn in the page (a portal in `document.body`, `position: fixed`) in the looks and the classes of `MenuButton`'s, with no button: whoever opens it knows where the pointer was and what the menu is of, and hands it items that do their own work (`ContextItem`: `run`, or `items` for a menu that opens beside it, and `checked` for a switch (`menuitemcheckbox`, `aria-checked`), `current`, `disabled` with a `title` that says why, `danger`, `rule`, `group`). It is the pattern of the WAI-ARIA authoring practices: the menu has the focus when it opens, with no item chosen (a key that is typed is never the page's, whose keys play notes: every key that the menu takes is stopped); the arrow keys move between the items and wrap, Home and End go to the ends, Right or Enter opens the menu of an item that has one and Left closes it and gives the focus back to the item that opened it, Enter or Space chooses, Escape closes the menu that has the focus (the first, with the focus given back to where it was), Tab closes it and gives the focus back, and a letter, or the first few of a name typed within 700 ms, goes to the item that starts with it (one letter, or the same one again, goes to the next such item, from the first when nothing has the focus; more go to the first from the item that has the focus). A letter with Control, Command or Alt held and a key that is not a letter (F5, Shift) are the browser's: not taken. Pointing at an item that has a menu opens it after the pointer has rested 140 ms, and pointing at another takes it away (`HOVER_MS`: a pointer on its way to the menu crosses the items between). It is put where it fits: the first at the pointer, held `EDGE` (4) pixels inside the window; the menu of an item beside the item, at its right if the room that is left (less the edge) is as wide as the menu and at its left if not; no taller than the window (it scrolls). It closes when a press or the wheel is anywhere else (a wheel in it scrolls its list), when the page is sized and when the window is left; **Escape closes it wherever the focus is** and the key is taken (`preventDefault` and `stopPropagation` at the window, in the capture phase), so that what is chosen on the canvas is not given up by the Escape that was for the menu.
- **The host** (`app/patch/contextMenu.tsx`: `ContextMenuHost`, `useContextMenu()`). One menu at a time. `open(event, { label, items })` takes the event of a right click (its place, and `preventDefault` and `stopPropagation`, so that the browser's own menu and the canvas's other handlers stay away), shows the menu at the pointer under a key of its own (a menu that is opened while another is open is a new one: nothing of the first, its open menus and its focused item, is kept), and a menu with no item opens nothing, takes away the one that was open and still keeps the browser's menu away.
- **What the menus offer** (`app/patch/contextMenus.ts`: pure functions of `MenuDeps`, which are the client's state, the editor (the methods that a menu asks, `MenuEditor`), the canvas's own actions (`CanvasActions`: the panel of parameters, a node's scope, what is chosen, the field for a module's name, the arrangement and the view) and where the pointer was on the canvas). A menu is made from the session when it is opened, and not kept up to date. The lists are the ones that the buttons have (`instrumentItems`, `effectItems`, `freeEffectItems`, `sendItems`, `noteFromItems`, `noteToItems`, `earItems`, `presetsOf`, `MODULATOR_KINDS`), so that a menu and a button say the same, and a menu that has nothing to choose from is shut and says why (`No instrument is installed`, `There is no place that takes a cable from it`).
  - *The canvas* (`paneMenu`; a right click on it, and on a link by which a lane hears every source, which has nothing to ask): Add an instrument, Add an effect (the modules of the library first), Add a modulator, Add a source of notes (the keys and each open input that no device stands for); Undo and Redo, in the words of the history (`Undo add Reverb`); Save the N chosen as a module… and Choose none, above the rest, once something is chosen; Choose every node; Choose nodes with a box (a switch: the Select button's); Arrange the nodes (shut with `They are arranged` while they are) and Show every node.
  - *A lane* (its card): Change the instrument (Choose an instrument when it has none), Load a preset, Parameters… and Scope (switches that say which is on), Add an effect, Add a modulator (that hears the lane), Send audio to, Take notes from, Mute and Solo (switches), Save the chain as a module…, Remove X only (its effects stay in the lane) and Remove the lane of X for good, in the words that the button's menu has (`removalWords`, `keptWords`: `client/removal.ts`). *An effect*: Move earlier and Move later in the chain (shut at the ends), Load a preset, Parameters…, Scope, Send audio to and Remove X, and, for the first effect of a lane that has no card, the lane's own Add a modulator, Mute, Solo and Save the chain as a module… (and Remove takes the lane with it when the effect is on its own). *A modulator*: Whose notes it hears, and Remove. *A source of notes*: Send notes to, and Remove. *A group* (an old file's, or the terminal's `g`): what its header has. *The master*: Limiter (a switch).
  - *A cable*: Remove cable A → B, which is what the × at its middle does (a cable of modulation or of audio is taken out, a link of the audio path is cut: `SetRoute none` or `SetFeed none`).
  - *What is chosen* (a right click on a node that is one of several that are chosen, which is then the menu of what is chosen, named for how many: `the 3 chosen nodes`; a node that is not chosen, and the only one that is, have their own): Save the N chosen as a module… (N is what a module can hold of them; shut with `A module needs an effect or an instrument` while no plugin is held) and Choose none. The field for the name opens **where the pointer was** (`ModuleNameDialog` in `SaveModule.tsx`, the dialog that the buttons' field is, which `dialogPlace` keeps in the window).
  - A right click on a field that takes text is not taken: it has the browser's own menu, which cuts and pastes.
- **Where what a menu adds goes.** An instrument, an effect, a modulator or a source of notes that is added from the canvas's menu is put where the pointer was, in the canvas's pixels (`screenToFlowPosition`): `Editor.addInstrument`, `addFreeEffect`, `addFreeModulator` and `addSource` take a `place`, ask for the add and then for the node to be put there (`#placed`: `SetNodeUi`, which is a step of the history of its own: **Undo takes the place back and a second Undo the node**), and the canvas is not moved to show what the person is looking at; if the place is refused the node is shown as one that has none, and the notices say why it could not be moved. A module is several nodes and goes where the layout puts it. The menus of a node add to that node (an effect to its chain, a modulator that hears it) and have no place.
- **Choosing with Shift** (the page's `PatchCanvas`). A box drawn with **Shift held** chooses the nodes that are all in it, with Select off, and a click with Shift (or Control or Command) held adds a node to what is chosen or takes it out; **a plain click on a node chooses nothing**, since a click on a knob is a click on the node it is in. React Flow marks a node chosen as soon as it is clicked, before it says so (so that what it says cannot be filtered), so the canvas is selectable only while it is to be: `elementsSelectable` is `choosing || adding || boxing`, where `adding` is a key of Shift, Control or Command held (`useKeyPress`) and `boxing` is set by `onSelectionStart` and **cleared a turn after the button is let go of** (`setTimeout`, from the capture of `pointerup` and `pointercancel` at the window: cleared at once, React Flow's own end of the box found the canvas not selectable and never ended it, and the box and the pan to it stayed: the browser spec found it), so that a box goes on being a box when Shift is let go of before the button. A click on the canvas gives up what is chosen (`onPaneClick`: React Flow's own gives up nothing while the canvas is not selectable). **Escape** gives up what is chosen and leaves choosing whenever either is so, unless something that is open took the key. With choosing on (the button) it is as it was (a click chooses, a box drawn with the main button chooses), except that the middle button alone pans (`panOnDrag`: it was the middle and the right one) and **the right button is the menu's**; the bar says what is chosen and has Save as module… whenever something is chosen, or choosing is on. A node's root has the class `nokey` (React Flow ignores the keys that come from inside it, and starts no box on a press inside it: a Shift-click on a node's header adds it, a Shift-drag on it moves it, a Shift-drag from a port draws a cable, and the arrow keys on a knob turn the knob and do not move the node).
- **Moving what is chosen** (`useMovableNodes.ts`). A node that is taken is moved with every node that is chosen (React Flow's third argument of `onNodeDragStart` and `Stop`), and each is told to the server where it was put down (`SetNodeUi` for each that is not where the session has it: **N steps of the history**), held where it is put until the server has answered (`held`, counted for each drag of a node that is in flight, and `keepHeld` in `moving.ts`, which also keeps a node chosen when the session is laid out again). **The arrow keys move what is chosen** (React Flow's, 5 pixels, 20 with Shift: a change of position with no drag): each node that they move is held and told once, when they have rested `KEYS_REST_MS` (400 ms), with the last place. A node is draggable by its own word (`draggable: true` in `toFlow`), which `nodesDraggable` gives way to: **nodes are moved while choosing is on**, as with it off (this section's earlier words, and step 105's, said they were not: `nodesDraggable={!choosing}` was a prop with no effect, and is gone).
- **Decisions** (D107 of `docs/web-ui.md`; the user may overrule them). (1) A right click opens the menu of what is under the pointer, made from the lists that the buttons have, so that the menus and the buttons agree, and the buttons stay. (2) A menu is made when it opens; it goes at once for anything but a choice of its own items. (3) A node that is added from the canvas's menu is put where the pointer was by two steps, an add and a move, and not by one: a `place` on the intents that add (and `SetNodesUi` for a group's move) would make them one, and is the controller's, where this step is the page's. (4) Shift held is the mode of choosing for as long as it is held, and a plain click chooses nothing. (5) Nodes that are chosen move together, each told to the server by its own `SetNodeUi`. (6) The right button is the menu's while choosing, too: the middle button pans. (7) The keys of the menu are not heard by the page, and Escape goes to the menu before it goes to what is chosen. (8) A module that a menu adds is not placed, since it is more than one node.
- **Held by tests and mutants (step 107).** Page 3,130 tests in 135 files (3,008 in 130): `ContextMenu.test.tsx` 30 (the items and their roles, sections, the keys, type-ahead, the pointer's rest, menus in menus to any depth, where it opens to the pixel, what closes it, what it listens to), `contextMenu.test.tsx` 5 (the host), `contextMenus.test.ts` 44 (every menu, item by item, the words, the rules that set the sections apart, where a node goes, the specs of a node and of a cable), `PatchView.menus.test.tsx` 22 (the menus on the canvas, the keys that choose, Escape, the field for the name where the pointer was), `useMovableNodes.test.tsx` 12, `moving.test.ts` 3 more (a node stays chosen), `editor.test.ts` 5 more (what is added where a menu was opened); the browser suite 227 (`e2e/menus.spec.ts` 11: a box with Shift held, Shift let go of before the button, Shift-click, a drag of what is chosen, with choosing on the middle button pans and the right button is the menu's, a menu on the canvas that adds at the pointer, the keys of a menu, a node's menu and a cable's, a module saved from the canvas's menu, and on the real program the place of what a menu added and of what a drag moved, in the saved file and for a page that is opened again). **Held by mutants** (step 107): 350 mutants of the logic of the step, in four runs, one at a time against the unit tests that should catch them (the menu 99: its sections, keys, timings, places, what closes it and what it says of its items; the host 7; what each menu offers 100; the words of a removal 12; moving what is chosen 26; what is kept when the session is laid out again 7; choosing and the menus' wiring on the canvas 48; the field for the name 26; the editor's place 9; and 16 of the code that the first run's tests made: the specs of a node and a cable 11, the item of a lane's chain 2, the type-ahead 3): the first run of 334 caught 277 and let 57 survive: **47 gaps, each a test now** (the pointer's rest, a third menu inside the second, the room to the pixel, a letter at 699 and at 700 ms, the keys that are not the menu's, the host's every word, which flag a switch says, the rules that set the sections apart, what the selection's name counts, a node held once for two steps of the keys, the drag that is over, the switches of the panel and the scope, the arrangement that is done, where the pointer was across and down, the focus after a press elsewhere; five of them in code that moved into `nodeSpec` and `edgeSpec` to be tested), **5 that the browser holds** (four by the specs that were there, a box and a Shift that is let go of; one by a spec that was written: the middle button pans), **2 dead code, out of the code** (the fallback of a lane's name in the field of its chain, and `nodesDraggable`, above) and **3 no fault** (two lines of a menu in the other order, which a mutant that I wrote did not change; a guard that cannot be false, since a modulator has the item of every note; and the net for `pointercancel`, which nothing can see, since the next pointer up ends a box all the same). The second run, of 58, caught 52 and let 6 survive (five that the host's own tests, which it did not name, catch, and the guard); **three faults were found by it**: a letter typed while no item had the focus skipped the first item, a node's menu for several that are chosen counted what a module can hold and not what is chosen (`the 1 chosen nodes` for a node and the keys), and the prop that did nothing.
- **Found by the whole run of the browser suite** (before this step's last commit): `e2e/modules.spec.ts` "a chain of effects is saved as a file of the library…" failed one run in three on a busy machine, and on step 106's page alike: the helper `heardAt` pressed a key when the page had no meter reading yet (a meter that has none has no `aria-valuenow`, which `Number` reads as 0, full scale), so that it was "heard" at once and let go of before the sound had begun, and the tap's sound came after the check that the master was quiet again, into the next silence. The helpers wait for a master that is quiet before a key goes down (`quiet` in `e2e/sound.ts`), and the spec waits for the page to have the cut before the key; sixteen runs in a row pass.
TODO: a group of nodes moved, and a node added and put by a menu, are N steps and two steps of the history (one Undo takes back one node's place): `SetNodesUi { nodes }` and a `place` on the intents that add, which are the controller's and the history's, would make each one step.
TODO: the menus have no copy, paste, duplicate, align or remove of what is chosen (a menu of what is chosen saves it as a module and gives it up); the knobs have no menu (the `~` button is the modulation menu); a module that a menu adds is put where the layout puts it, and not at the pointer; and a long press on a touch screen does not open a menu (a right click does).
TODO: the terminal has no menus (its popups are the keys' own); the keys that a menu has are the page's, and the Help tab does not say that a right click does anything.
TODO: Escape gives up what is chosen and leaves choosing, and the panel of parameters closes at the same Escape (its own key): one key for three things; a person who wants the panel to stay has no way to keep it.
TODO: a node that is chosen and not seen (off the view) is moved with the others by a drag, and the bar says how many are chosen and not where; `Show every node` is the menu's and the canvas's own button's.

### Pinned parameters (`pinned`)

A front-end that shows a plugin's parameters as knobs on its node (the browser) keeps which ones in the session file, as a
`pinned` list of parameter names on the plugin's table: the lane's instrument (on the `[[instrument]]`), an effect (`[[instrument.effect]]`)
and a bus effect (`[[group.effect]]`). The engine and the TUI do not use it: the TUI does not show it, and keeps it through a
load, an edit and a save.

```toml
[[instrument]]
plugin = "builtin:osc"
pinned = ["sustain", "detune", "waveform"]

[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["resonance", "cutoff"]
ui = { x = 376, y = 0 }
```

- The order is the order the knobs are shown in. With none pinned (no `pinned`, or an empty list) a node shows the first eight
  parameters of its plugin; a parameter a cable ends at is shown too, for as long as the cable is there, and is not a pin.
- It is a key of the plugin's table beside `ui`, not part of it: a node can be pinned without being placed.
- It is read leniently, as a place is (a layout is not worth refusing a session for): what is not a list is left out and logged,
  and so is any entry of a list that is not a name. A name that the plugin has no parameter of, a separator, and a name that comes
  twice (the first place counts) are no pins; each is logged, and the next save leaves them out. A name that two parameters share
  is the first of them.
- In the model it is `PluginSlot::pinned`, the plugin's own parameter indices (`ParamSlot::index`, which cables and ports name too),
  so it goes where its plugin goes: an effect that is moved takes its pins along, an instrument that is replaced has none. The
  loader resolves the names (`PluginSlot::pin_names`) and the saver writes them (`pinned_names`). At most 64 can be pinned to a node
  (`MAX_PINNED`).

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

### Tempo

```toml
bpm = 96.0              # optional: the tempo patterns are recorded at, in beats a minute (20 to 300)

[[instrument]]
plugin = "builtin:osc"
```

A top-level key, before every table. It is the tempo that the next pattern is recorded at (the metronome's count-in and
the length of the recording are counted in it) and the one that the TUI shows (`120 BPM`, set with `b`). A session whose file
has none is at the tempo of its first recorded pattern (`[instrument.pattern] bpm`, below), or at 120 if it has none. A tempo
that is out of 20 to 300 is held to it when the session is loaded, and a value that is not a number (a string, `nan`, `inf`)
refuses the session, as every number does. The audio thread is told the tempo once when the session is loaded, if the file says
one or has a pattern (`SessionModel::bpm`, set by the loader). A save writes `bpm` only when a load would not come to that tempo
anyway (`SessionModel::bpm_to_save`), so that the files of sessions that never set one are as they were.
A lane that is added after the tempo was set starts at 120 in the audio thread (`PatternPlayer::new`) and is told the tempo when a recording starts, which the
TUI's `r` does first; a pattern that has been recorded keeps its own `bpm` whatever the tempo is set to afterwards.
The tempo is also what an LFO and a step sequencer go by when they follow it (`sync`, see "Tempo sync"): the graph has one tempo (`AudioGraph.bpm`, 120 until `SetGlobalBpm` says another, which the
loader, `SetBpm` (so its undo and redo too) and the start of a recording send), and a modulator that follows it goes as fast as it does.
TODO: a pattern does not follow the tempo (its events are frames, and its length is its beats counted at its own `bpm`); `docs/web-ui.md` Open question 10.

### Pattern recorder/player

Each instrument can have a recorded MIDI pattern. Recording captures note events
for a fixed number of beats at the current BPM. Playback transposes the pattern
to match any held key and loops while the key is held.

**Session config:**

```toml
[[instrument]]
plugin = "lv2:http://tytel.org/helm"

[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true
transpose = "in_key"

[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100

[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
```

- `bpm` — the tempo the pattern was recorded at: its length in frames is `length_beats` counted in it (default: 120). It is not the
  session's tempo (see "Tempo"): setting that, with `b`, does not change a pattern that has been recorded
- `length_beats` — pattern length in beats (default: 4 = 1 bar in 4/4). The Length row of the pane sets it: the pattern that is there loops at the new
  length from then on (its notes stay where they are, and those that the loop no longer reaches wait for it to be long again), and the next recording is that long
- `base_note` — reference note for transposition (set to the lowest recorded
  note)
- `events` — recorded MIDI events with `frame` (sample offset), `status`
  ("on"/"off"), `note` (e.g. "C4"), and `velocity`
- `enabled` — whether pattern playback is active
- `looping` — whether playback loops while the key is held or plays once
  (default: true)
- `transpose` — `"chromatic"` (default) or `"in_key"`. Chromatic shifts every
  note by the semitone distance from `base_note` to the trigger key,
  preserving exact intervals. In-key shifts by **scale degrees** of the piano
  scale (`[piano] scale`, live-updated from the scale picker), so a C-E-G
  pattern triggered on D in C major plays D-F-A (the diatonic D minor triad)
  instead of D-F#-A. Off-scale recorded notes keep their chromatic offset from
  the scale tone below (passing tones survive); off-scale trigger keys snap to
  the scale tone below. With the Chromatic scale selected, in-key degenerates
  to plain semitone shifting.

**Behavior:**
- Press `r` on the Session tab to start recording. A 4-beat metronome count-in
  ticks first (notes played during count-in snap to frame 0), then recording
  begins. Play notes on the virtual piano or MIDI keyboard. Recording
  auto-stops after `length_beats` at current BPM.
- Hold any key to play back the pattern transposed (relative to `base_note`).
  The pattern loops while the key is held and stops on release.
- Press `r` again to overwrite with a new recording. Re-recording keeps the
  pattern's transpose mode.
- The player's lists of held keys and of sounding voices have room for every key, so that the first
  note that starts a pattern does not ask the audio thread's allocator for memory
  (`the_first_notes_of_a_pattern_do_not_allocate`).
- Press `Ctrl+R` to clear the pattern.
- Press `b` to set the global BPM.
- BPM is displayed in the status bar. Pattern indicators show in the chain tree:
  `▶` = pattern exists, `⏺` = recording. In-key patterns show `in-key` in the
  tree label.
- Selecting the pattern node shows its settings in the parameter pane:
  Length (beats), Enabled, Loop, and Transpose (Chromatic/In Key), toggled
  with `Left`/`Right`.
- What is done to a pattern is done by intents, which the browser's requests are as well: `r` is `SetRecording { node, recording }`, `Ctrl+R` and `d`
  on a pattern are `ClearPattern { node }`, and the rows of the pane (typed or stepped) are `SetPatternField { node, field, value }` (see "Session state"). The
  pattern is on its lane's node in the graph a browser is given (`PatternInfo`: whether it is recording, enabled, looping and in key, its length in beats, the tempo
  it was recorded at and how many notes it has), and what is done to it is told as `PatternSet`.
- The browser page has the same controls in a block under every lane (`app/patch/PatternCell.tsx`, step 51; see "Web UI"): Record / Stop, Clear (it asks first: a pattern is
  not put back by undo), the length in beats, Play, Loop and In key, and a line that says what the pattern is. The count-in is `COUNT_IN_BEATS` (4) beats of the metronome,
  which the page reads as `PATTERN_COUNT_IN_BEATS`.

### Example sessions

`examples/` has sessions to start from, each a recipe for one thing that the built-in plugins do (the first three are the older ones):

| Session | What it shows |
|---------|---------------|
| `basic.toml` | the built-in sine, and nothing else |
| `split.toml` | two instruments on a split keyboard |
| `echo.toml` | a filter into a ping-pong delay, with an LFO on the filter and one on the delay's time |
| `supersaw.toml` | a saw with a unison of seven (`unison`, `spread`, `width`), a low-pass filter that a slow LFO moves, a reverb |
| `wind.toml` | the oscillator's `noise` through a band-pass filter that two slow random LFOs move, into a big reverb (hold a key: it swells in over 2.5 s) |
| `bell.toml` | FM (`fm`, `fm_ratio` of ×3.5) with an envelope that takes the swing down while the note rings, so that the bell starts bright and ends as a plain tone |
| `arp.toml` | a step sequencer on `detune` with the values of the notes over twelve (a depth of 0.25 makes a step of 1 twelve semitones): a minor seventh arpeggio from a held key, whatever key it is, an eighth-note triplet a step (`sync = "1/8 triplet"`: six a second at the 120 beats a minute it says, and as fast as the tempo does) |
| `pwm.toml` | a square whose `pulse` an LFO moves, three oscillators for each note, a short echo |
| `epiano.toml` | an electric piano: FM at a ratio of one with an envelope on the swing (bright at the start of a note, mellow after it), the delay's Chorus preset, a little reverb |
| `sync-lead.toml` | a hard-synced saw (`sync` of 6 at the top) whose ratio an envelope opens over the first 0.4 s of a note and lets settle (2.75 while it is held), a little `drift`, a low-pass filter |
| `sub-bass.toml` | a saw with a `sub` of 1 (the sine an octave under the note), through a low-pass filter that a short envelope opens at the start of each note: a pluck, an octave under the key |
| `fold.toml` | a triangle through the Drive's `fold` shape, with a slow LFO on `drive`: a sweep from a soft sound to a bright, metallic one and back, at the pitch of the key all the way |
| `wobble.toml` | a saw with a sub through a low-pass filter that a triangle LFO opens and shuts, a cycle an eighth note (`sync = "1/8"`, at 140 beats a minute): a wobble bass that goes as fast as the tempo does, from the start of its wave at every key |
| `gate.toml` | three saws for each note whose oscillator volume a step sequencer opens and shuts, a step a sixteenth note (`sync = "1/16"`, at 120 beats a minute): the stutter of a trance gate, from its first step at every key, as fast as the tempo does |
| `melody.toml` | a random LFO (sample and hold) on `detune`, a new number every eighth note (`sync = "1/8"`), held to the minor pentatonic by the oscillator's `quantize`: hold C and it plays the notes of C minor pentatonic, hold F and it plays F's, at the pace of the tempo |
| `shared-reverb.toml` | two oscillators into one reverb, by audio cables (`[[cable]]`): a saw and a square an octave above it each go to the mix as they are, and each sends part of itself to one reverb, all wet, that stands by itself in a lane that has no instrument (it was on the bus of a group that had no lane in it until step 103, and the file as it is renders the same bits, every sample): the two oscillators and, under them, one room that rings on when the keys are let go (with `route = "none"` on each oscillator, which the header says, the room alone; with one oscillator soloed, which it says too, that oscillator and the room that it sends to) |
| `ring-bell.toml` | ring modulation, by the two inputs of a Math effect: two sines (the second a tritone above) with no way to the mix of their own, one cable into A and one into B of a Math in a lane of its own (the Ring Bell preset, a little reverb after it): the sum and the difference of the two pitches and neither of them, a bell (take the second cable out and B is the Math's own carrier) |
| `growl.toml` | logic on a signal: a saw through a Math in its own chain with nothing patched to B, so that B is the Math's own carrier, a saw of E2: the Xor Growl preset at four bits, a crushed, buzzing growl that has the pitch of neither; a slow LFO moves the carrier's `pitch` |
| `pump.toml` | a sidechain, by the Math's second input: a pad (a saw, three oscillators a note) through a Math in its `follow` mode (the Pump preset), a ghost kick (a sine two octaves under the key, no way out of its own) opened on each beat by a step sequencer and a cable from it into B: the pad is ducked at each kick and comes back before the next, the pumping of a dance track (without the cable B is the Math's own carrier, a sine of a little over 1 Hz, which pumps the pad at its own pace) |
| `shared-lfo.toml` | one LFO on two lanes, by a free modulator (a top-level `[[modulator]]`, in no lane): a saw and a square an octave above it each go through a low-pass filter, and one sine LFO of 0.5 Hz opens the saw's filter as it shuts the square's (two cables, a depth of +0.2 and one of -0.2, each naming its plugin by its place: `to = "instrument[1].effect[0]"`), so that the two take turns, two seconds a turn |
| `microphone.toml` | the computer's microphone through an echo and a reverb (step 99): an audio input in a lane that has no way out of its own (`route = "none"`), heard through two cables, one into the Slapback delay and the reverb of a lane of its own and one to the mix at half its level; **use headphones** (through speakers the microphone hears what it plays and howls); under `--audio-device none` and in the tests a sine of 220 Hz at 0.25 stands in for the microphone |

The eighteen that make a sound with nothing installed (`SOUND_EXAMPLES` in `src/audio.rs`) are held by tests that render them through the real engine: each plays a chord that is heard (the loudest sample above 0.05) and is
not loud (below 0.6); the arpeggio goes through its notes within 5 cents (it is within half a cent: the pitch is measured by autocorrelation, step by step) when its key is pressed after a pause that is not a whole number
of steps, so that the key's restart of the sequence is what is heard; the bell is more than four times brighter in its first moments than at the end (it is seven times) and louder; the wind is quiet in its first moments and
swells over its attack, and is still sounding a moment after the key goes up; the electric piano is brighter in its first moments than later; the sync lead is brightest at the top of its sweep (1.85 times as bright as in its first moments and 1.45 times as at the ratio it settles at; the test asks for 1.4 and 1.25) and has the pitch of its key once it has settled (within 0.1 cent; the test asks for 5, and does not measure the pitch while the ratio moves: a wave that changes from one cycle to the next moves the peak of the autocorrelation by 26 cents at the start of a note, and at the top of the sweep another peak, 6/5 of the key's, is the one that is found); the sub bass is heard an octave under the key (C2 for C3, within 1.2 cents at the three places measured; the test asks for 5) and is 1.66 times as bright at the start of a note as later (the test asks for 1.3); the fold is about 7 times as bright at the top of its LFO's sweep as at the bottom (the test asks for 4) and has the pitch of its key while it is folded (within 0.3 cent at the three places measured; the test asks for 5); the arpeggio, the wobble and the gate go by their tempo (the tests give each a tempo of its own, `at_tempo`: the arpeggio's eight steps are in tune within 1.4 cents, measured, at 60 and at 180 beats a minute, three and nine steps a second; the wobble's filter goes round in 0.2142 s at 140, where an eighth note is 0.2143 s, and in 1.999 times as long at 70, the period read from the autocorrelation of the sound's brightness; the gate's shut steps are under 0.02% of the loudest and its open ones at least 0.44 of it, since the three oscillators of a note beat against each other, on the sixteenths of 120 beats a minute and on twice as long at 60); the melody plays, for each of sixteen eighth notes, a note of the minor pentatonic counted from the key (within 0.05 semitone, 5 cents: the pitch is read from the places where the wave goes up through zero, which are 0.01 cent from it, where the autocorrelation of a triangle was 5.6 cents out at F3), one note all through each step and another than the last at 13 of 15, with eight different notes in all; the shared reverb's room rings on after the keys are let go, which only its cables bring in (the chord is let go at 1.0 s and the oscillators are done by 1.25 s: the rms from 1.6 s to 2.6 s is 0.0024, exactly 0 with the cables taken out, 0.0027 with the saw's cable
alone and 0.0013 with the square's; the room of both is the two rooms added, the reverb being linear at this level, to 7e-9 or 0.0003% of the rms, where the test asks for 0.01%; the chord's loudest sample is 0.224; the room is still 0.0055 five seconds in and 0.0002 seven; and with `route = "none"` in the table of each oscillator, as the header says, the session is the room alone, from the first moment of a chord that is held: it is the whole session less the oscillators as they are heard without it, to 7e-9, against an rms of 0.0135, where the test asks for 1e-5 of that; and with the saw soloed, as the header says, the session is the one in which the square is muted, to the bit, and its room, which is heard for the saw's cable though the room's lane is not soloed, is the room of the saw's cable alone from 1.6 s to 2.6 s, the same bits, with an rms of 0.00265, where the test asks for a millionth of that); the shared LFO's saw is 5.8 times as bright when the LFO is at its top (0.5 s) as when it is at its bottom (1.5 s) and the square 4.4 times as bright at the bottom as at the top (each lane alone, the other muted; the test asks for 2), the session has one modulator that is in no lane with two cables to the first effect of each lane, and the LFO is heard (the session with its table taken out differs from it by 0.43 at the most, against a loudest sample of 0.47, where the test asks for 0.05); the ring bell has a cable into each input of its Math, and (single note, C3 on the first oscillator and F♯3 on the second) the sum and the difference of the two pitches are 770 times as strong as the two pitches themselves (the test asks for 100), and with the second cable taken out the sound is the first oscillator times the effect's own carrier, E4, at the sum and the difference of those two, again 790 times as strong as the rest; the growl has its Math in its chain with no cable, the LFO on the carrier's `pitch` and the Xor Growl at four bits, and it is another sound than the saw it is made of (the largest difference is 0.52, against a loudest sample of 0.43 for the growl; the test asks for 0.05) and another with its LFO taken out (0.69); the microphone example is heard through its two cables alone (the tone of the null backend, a sine of 220 Hz at 0.25, stands in for the microphone: with the cables taken out the session is silent to the bit, and with only the dry cable the tone is at half of the lane's 0.8, a peak of 0.1 within 0.005); the pump has the kick's cable at the second input of the pad's Math, and (a chord, with the pad alone as the yardstick, since its voices beat against each other) 0.09 of the pad is left just after a kick and 0.56 before the next one, 6.2 times as much (the test asks for 3), and with the cable out the sound is another, the pad pumping at the carrier's pace (the difference is 0.66 of the pad's own level; the test asks for 0.2); and the delay presets that `pwm.toml` and `epiano.toml` name are ones that the delay has (the loader only logs a name that a plugin lacks, so a misspelling would load and
sound like no preset). A saved sound of somebody's own (`tracer.toml`, `cool-sound.toml`) is not held to any of it. An example that has modulators in a lane or a group is moved when it is loaded and saved the new way (see "Free modulators", "Every modulator is global"): it sounds as it did (the corpus' copies are the same bits moved or not). The page cannot open a session (it can save one under a name): an example is started with `tang serve examples/bell.toml`.

## Note remapping

Some instrument plugins have bad samples on certain notes. Note remapping lets
you substitute a specific key with a nearby note on a separate MIDI channel,
using pitch bend to shift it to the correct pitch.

### Config syntax

```toml
[[instrument]]
plugin = "lv2:http://tytel.org/helm"
pitch_bend_range = 2  # optional, default ±2 semitones

[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
"C#2" = { note = "D2", detune = -0.5 }
```

- `pitch_bend_range` — the plugin's pitch bend range in semitones (default: 2.0).
  Must match the plugin's own pitch bend range setting (the built-in oscillator's is its `bend`, ±2 to begin with).
- `[instrument.remap]` — a table of note substitutions. Keys are source note
  names (`[A-G][#b]?[0-9]`, C4 = middle C = MIDI 60). Each value has:
  - `note` — the target note name the plugin will actually play
  - `detune` — pitch bend offset in semitones (e.g. 1.0 = one semitone up)

### How it works

- Normal (non-remapped) notes play on MIDI channel 1 as usual.
- Each unique detune value is assigned its own MIDI channel (2–16). Notes that
  share the same detune value share a channel. The channels are handed out in the order
  of the notes that are remapped, lowest first (the detune of the lowest note has channel 2),
  so that a session loads the same every time (it was the order of a hash map's, which
  is another for each load: `remapper_hands_out_its_channels_in_the_order_of_the_notes_it_remaps`).
- When a remapped note-on is received, Tang sends the rewritten note-on followed
  by a pitch bend message on that channel (bend after note-on — some plugins
  only apply pitch bend to already-sounding notes).
- Most plugins respond to all MIDI channels by default (omni mode), so no
  plugin-side configuration is needed beyond setting the pitch bend range.

### Limits

- Maximum 15 distinct detune values (MIDI channels 2–16).
- Detune must not exceed `pitch_bend_range` (error at load time).
- Sustain pedal (CC64) only affects channel 1 (non-remapped notes) of a plugin. (The built-in oscillator's pedal holds a note whatever the channel it is on.)

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
on a group header asks it); `Save` (the session's own file); `SaveAs { name }` (the session written as the file that
`session::plain_file_name` makes of `name`, beside its own file, which is its file from then on: the spaces round the name go, `.toml` is added
unless it ends so, and it is a plain file name, no `/` or `\`, none of `:*?"<>|` or a control character, something before the extension, not a name
Windows keeps and at most 255 bytes; refused, with nothing written and nothing changed, for a name that is not one and for a file that is there
already, a link that leads nowhere included, unless it is the session's own (that is a save), as `IntentError::Invalid`, and a write that fails is
`Io`; told as `Saved { file }` with the new name; no step of the history; a file that is there is refused by the write itself (an exclusive create,
`session::save_new`), not by a look at the path first; the TUI's `Ctrl+Shift+S` asks it too, and so does the page's "Save as…"), `SaveModule { node, name }` (the chain of effects of a lane, or of the lane that an effect is in, written to the library of modules under `name`: no change of the session, no step of the history; the model's `modules_dir`, `ModuleSaved { name }`, the catalog told again: see "Modules: effects and instruments, with the cables between them, saved to a file"), `SaveSelectionAsModule { nodes, name }` (the same for the plugins that are chosen, from any lanes: the instruments and effects in `nodes`, with the audio cables that have both ends among them, and the modulators in `nodes` with the cables that they have to what is in the module; refused for a group, a bus effect, a source of notes, a modulator with no plugin to go with it, a lane that has no instrument and for none at all: see "Modules: effects and instruments, with the cables between them, saved to a file"), `Undo` and `Redo` (the history, below); and fifteen that change
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
`SetPinned`, `RenameGroup`, `SetScale`, `SetLocked`, `SetBpm`, `MoveEffect`, `SetCableGain` (the level of an audio cable, merged as a depth is: `Key::Level`, "set the level of an audio cable"), and what is added: `AddEffect` (a lane's or a bus's), `AddFreeEffect` (in the words `add Reverb`: undo takes the lane out with the effect in it), `AddInstrument`,
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

## Architecture

N+2 threads, no async (the web server, behind the `web` feature, is the one
island of async, on a thread of its own):

- **Audio thread** (cpal, JACK or ALSA on Linux) — processes the instrument
  chains, fills output buffers. Promoted to SCHED_FIFO real-time scheduling on
  first callback. The instrument list is owned (moved into) the audio callback
  closure — no mutex needed. A plugin swap mechanism exists via bounded crossbeam
  channels (send new plugin in, receive old plugin back for main-thread drop).
  The loader uses it to put the session's plugins into the audio thread, and
  `app::controller::Controller::poll` reaps the plugins handed back, in the TUI,
  in `play` and in the headless loop of `serve`. What the callback's `audio::Renderer`
  renders with is an `audio::Engine` (a trait: `num_channels` and `process(midi, out)`;
  `AudioGraph` is the one there is), so that a second engine can be run beside it on the
  same input and compared bit for bit (see "The oracle").
- **MIDI thread(s)** (midir) — one per input device, all push into the MIDI channel.
- **Audio input thread(s)** (`tang-audio-input`, cpal; one for each Audio Input plugin that is made for a device, none under `--audio-device none`) — owns the input stream and fills the ring that the audio thread reads
  (see "The audio input"); it opens the microphone after the first block has been rendered and closes it when none has been for a second.
- **Main thread** — runs the TUI loop (`tui::event_loop`), which waits on a channel
  of the terminal's events, the web interface's requests when there is one
  (`--web`) and a 100 ms tick, and draws; or the `play` loop, or the headless loop of
  `serve`. The virtual piano lives here and pushes into the same MIDI channel as
  hardware devices.
- **Terminal reader thread** (TUI only) — `tang-terminal`: crossterm can only be
  polled, so this thread polls the terminal (50 ms at a time, so that stopping it is
  quick), and sends what it reads, a failure included, to the loop
  (`src/tui/terminal_input.rs`). It is started when the terminal is set up and
  stopped before it is restored.
- **Catalog scan thread** (TUI only, transient) — enumerates installed plugins
  for the selector popup at TUI startup and exits when done. Sends per-format
  batches over an unbounded channel that `Controller::poll` drains into
  `app::catalog::Catalog`.
- **Splash render thread** (TUI only, transient) — owns the terminal during
  session plugin loading, animating the startup splash while the main thread
  loads plugins. Exits before `tui::run` takes over the terminal.
- **Web server thread** (`web` feature, `tang serve` and `tang --web`) — `tang-web`, a
  current-thread tokio runtime that runs the HTTP server, one task per WebSocket
  and the telemetry pusher (`src/web/server.rs`, `pusher.rs`). It never touches
  the controller: requests go to the main thread over a channel (`web::owner`),
  what the controller applies and the telemetry come back through the hub to
  each client's bounded queue, and notes go straight to `NoteInput`. In `tang serve`
  the main thread runs the headless loop (`web::runtime`); in `tang --web` the TUI
  loop answers the requests among its own business (`src/tui/web.rs`).

**Exit.** `main` calls `run()`, which stops everything that is Tang's, in order, and then
ends the process itself: it flushes stdout and stderr, prints an error as `Error: …`
(status 1) and calls `libc::_exit` (Unix; `process::exit` elsewhere). The ordinary exit runs what the plugins'
native libraries have to run at exit while the catalog scan, which may still be in one of
them (a quit a moment after the start finds it there), is in them, and the program crashed
on the way out (SIGSEGV or SIGBUS) about one time in twenty. `tests/cli.rs` holds what is
printed and the statuses (0, 1, and 2 for a command line that is not one), and
`tests/serve.rs` interrupts a `tang serve` early forty times in a row.

The shared LV2 world (`plugin::Runtime`) is created lazily on the first LV2
plugin load, not at startup — building the world scans every installed LV2
bundle, which sessions without LV2 plugins shouldn't pay for.

MIDI-to-audio communication via bounded MPSC channel (crossbeam-channel, capacity 1024); each message carries the source it came from (see "MIDI routing").

Audio thread logging uses `log::debug!()` for per-event messages (filtered out at default
Info level) and `log::info!()` only once on first callback. No real-time safety issue at
default log levels.

**The scope ring.** The audio thread keeps the last 8192 frames of the output mix (`telemetry::ScopeRing`, `Telemetry.scope`: 170 ms at 48 kHz, both sides, every sample a 32-bit cell of its own), stored at the end of
`AudioGraph::process`, after the master meter, with no lock and no allocation, and any thread copies the latest frames out (`latest`: at most `SCOPE_READ_MAX`, 2048, a quarter of the ring, so that what is read is not written over
while it is read; a copy that the writer overtook is made again, up to three times, and then taken as it is). The TUI's Scope tab reads it on every frame that it draws, and the web runtime's pusher on every tick for a client that has asked
for the `scope` channel (see "Web UI"). Before the first buffer it is silence.

**The probes.** Besides the master's ring the audio thread has a pool of eight (`telemetry::PROBES`, `Telemetry.probes`: rings of the same kind) for the signal at the output of a node: of a lane's instrument (after
its volume), of each effect (after its mix) and of a group (after its pan; a bus effect, after its mix), stored by taps in `InstrumentLane` and `Group::finish` into the ring that the controller has told the audio thread the node has
(`GraphCommand::SetProbe { target, ring }`, `ClearProbes`; a lane or a group that nothing listens to has an empty list of taps, a check at each stage). The controller says which ring each watched node has (`Controller::set_probes(Watcher, &[NodeId])`,
`src/app/probes.rs`: a watcher is a client of the web interface or the terminal, two watchers of a node share its ring, a node that no ring is left for has one when one is free, and what has no signal has none: a modulator, a lane that has no
instrument, the master) and keeps the audio thread's taps where the nodes are as the structure of the session changes (`reconcile_probes`, from `conclude` and `poll`). Which node a ring is for is in the telemetry
(`Telemetry::probe_node`) for the pusher, which sends each client the frames of the nodes it watches (channel 4: see "Web UI"). The terminal is a watcher too, `Watcher::TERMINAL`, for the scope of a node on its Session tab (see "Scope tab"), so the
probes are in every build; `keep_probes_of`, which the owner of the web interface calls for the connections that have closed, is with the `web` feature or in tests only.

MIDI devices are hot-pluggable — `Controller::poll` (called from the TUI loop, the
`play` loop and the headless loop of `serve`) looks at the MIDI input ports about once a
second (not in `serve` when there was no MIDI subsystem at startup): a port that is new is
opened (all of them, or the ones that match `--midi-device`), and a device that is open and whose
port is not listed any more has gone: its connection is closed, `MidiManager::poll_new_devices` says
so (`DeviceChanges { came, gone }`, each device by its source) and the controller sends
`GraphCommand::ReleaseSource` for it, so that what it was holding is let go of (a key that was down
when the cable was pulled does not hang). A device is the port it was opened on, by the port's id
(`MidiInputPort::id`) and its name, and not by its name alone: a stale endpoint (one that is gone and
is still listed, with no name) is not opened and does not hide a device that has gone (`midi::plan`,
a pure function with tests of its own). A device that is plugged in again has the source it had
(`SourceTable`, by the port's name); one that is unplugged and plugged in again between two looks is
not seen to go. **On macOS** CoreMIDI gives a program the list of the ports as the server last told
it, and the server tells a thread on its run loop: a program that never runs one saw the ports that
were there when it started and no others (a device plugged in after the start was never opened, and
a port that went stayed listed with no name), so `MidiManager::open_ports` turns the run loop of the
thread once, with no waiting, before it lists the ports (`midi::let_the_system_see_the_devices`,
`core-foundation`, macOS only). Two tests of `tests/serve.rs` make a virtual controller and hold this
to the real MIDI system (`#[ignore]`d: they need CoreMIDI or ALSA's sequencer to make ports):
a device plugged in after the start is opened and heard; one that is unplugged is noticed, and
one that is plugged in again is opened again and heard.

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

## The oracle

`src/oracle/` (test builds only) writes down what the engine renders, so that a new engine can be held to it (P0 of `docs/patch-graph.md` §11.6, step 70 of `docs/web-ui.md`).
A *scenario* is a session as text, a script of notes and controllers (`(frame, bytes)`, so that the same script is the same notes at every block size), edits, a block size and a
rate. `oracle::Rig` loads it the way the program does (`load_session`, with a `Controller` round it over stub channels, the program's own bounded ones for what the engine reports,
and the built-in plugin catalog) and drives it a block at a time through the audio callback's own `audio::Renderer` (the notes that came in, the denormals flushed, the engine behind
the `Engine` trait, the channels interleaved): `block()` makes the edits that have their frame in the block, sends the notes at their place in it, and renders it (timed, and the
allocator's calls counted); `poll()` lets `Controller::poll` see what the engine reported (a control that was learned, a pattern that was recorded), as the loops of the program do.
`oracle::render` is all of a scenario: every sample, a hash of all 4,096 telemetry cells after each block, the allocations of each block and what became of the edits;
`oracle::fingerprint` boils it down to two hashes and a few numbers (the peak, the rms, the rms of each eighth).

The corpus is `src/oracle/sessions/*.toml` (the fifteen examples and the tracer scene as they were when the oracle was made, a copy, so that editing an example does not break
it; and `cross`, `mix`, `solo` and `remap`, which the generator does not draw, and `sends`, `patched` and `routed`, which have audio cables and, in the last, ways out that are cut, and `unfed`, which has effects whose feed is cut, a lane's and a bus's, heard through cables, and `free`, which has an oscillator with no way out and a lane that has no instrument and a reverb that a cable feeds, and `freemod`, which has three modulators that are in no lane (a free LFO, envelope and step sequencer, with cables to plugins of a lane and of a bus and one between them: step 91), and `freeear`, which has four that hear a lane, a group and every note (step 93), and `math`, which has math effects with and without a cable at their second input (step 95), and `gains`, which has cables from three modulators to a lane's volume, the mixes of effects and a group's volume and pan (step 97), and `input`, which has an audio input (the tone that the null backend gives it) in a lane whose way out is cut, heard through two cables, one to a reverb in a lane of its own and one to the mix (step 99), and `master`, which has three oscillators far over full scale and a `[master]` table that sets the volume and a ceiling of −3 dB (step 102) (the generator draws none of these: it would change the sessions of every seed); each is played with a chord, the wheel, the pedal and all sound off), `edited:cables` (a session whose cables are drawn, turned and taken out while it plays, a lane taken out and put back by Undo with its cables), `edited:routes` (the ways out of a lane and of a group cut and given back while it plays, a lane that is cut taken out and put back by Undo, a cable drawn that only a cut way out keeps from closing a loop), `edited:feeds` (the links into a lane's effect, a bus's and another lane's cut, and one of them given back, while a chord is held; cables drawn from the lanes into effects; an effect that has its link cut taken out and put back by Undo, the lane that has one taken out and put back by Undo, an effect added to another lane and moved there: step 86), `edited:free` (a reverb added in a lane of its own that has no instrument, fed by a cable from another lane, muted, soloed and its way out cut with a cable from it to the mix, a filter added after it, the lane taken out and put back by Undo: step 87), `edited:solos` (two cables drawn between a lane in a group and one outside it, and lanes and a group soloed and muted one after another, a lane taken out while it is soloed and put back by Undo: the law of what a cable carries, step 78), `edited:latency` (a drive put in a lane and on a bus, blended, a cable drawn from it and taken out, the drive taken out and put back, with Undo and Redo: the latency of step 80), `edited:sources` (the keys and a device that is never plugged in are put in as sources, and cables of notes are drawn and taken out while a chord, the pedal and held keys sound, with a lane that loses the keys while the pedal holds its chord and one that is left with the device while two keys are held, a source taken out and put back by Undo, a lane taken out with its cable of notes and put back by Undo: step 82; `performance_with_a_late_key` is its script), `edited:instruments` (a cable from lane 0's instrument into lane 1's delay, the instruments of lane 1 and then of lane 0 taken out while a chord is held, Undo putting them back as the very plugins, the cable taken out and drawn again, lane 1 taken out while it has none and put back whole: step 85, the effects of a lane that has no instrument run), `edited:freemods` (free modulators of three kinds made while a chord is held and cables drawn from them to every kind of plugin, the cutoff set by the user, a filter that a free cable drives taken out and put back by Undo, a lane and a group taken out with the free cables to them and put back, a free modulator taken out and put back: step 91), `edited:freeears` (free modulators made to hear lanes and groups and every note while a chord is held, the lane that one hears taken out and put back by Undo: step 93), `edited:gains` (cables drawn to a lane's volume, the mixes of effects and a group's volume and pan while a chord is held, the values set by the user, an effect, a modulator, a lane and a group taken out and put back by Undo, the instrument cleared and put back: step 97), `edited:input` (an audio input added in a lane of its own, which starts bare, cabled to a reverb and to the mix while the session sounds, its gain, its balance, its mute and solo and its volume turned, the lane taken out with its cables and put back by Undo and Redo: step 99), `edited:master` (the limiter's ceiling brought down and the master volume up while a chord is held, the limiter taken out of the sound and put in again, Undo and Redo: step 102) and 300 sessions that
`oracle::generate` makes from a seed (xorshift64*, the same on every platform: one to four lanes, up to two groups, effects of the four built-in kinds, every kind of modulator with
cables between them and to the group's members and bus, mutes, solos, ranges, transposes, remaps, patterns; a script of notes with MPE expression, controllers, a pedal, a key hit
again, all notes off and all sound off; and four to ten edits), each at block sizes of 64, 256 and 1024 frames: 1035 renders. 796 of the 900 generated renders are heard and 47
clip; of the 2,043 edits, 1,769 are applied, 238 skipped (nothing to aim at) and 36 refused (the tests say how many must be).

**Moved** (steps 94 and 96): a scenario is loaded as the program loads a session, with the modulators of its lanes and groups moved to the session's own (`session::parse`: see "Free modulators", "Every modulator is global"). At step 94 `Scenario::moved` said so for the corpus and `unmoved()` was the session as it is
written, which the loop that the plan replaced and the benchmark ran, and a moved session was held to the unmoved one; step 96 took the lanes' and the groups' racks out of the engine, so that there is no unmoved session to run and the flag, the method and the comparison are gone (see "The racks go (step 96)").

**Edits** (`oracle::edits`): what a front-end asks of the controller while the session sounds, which a file cannot show (a fade is made when a mute *changes*; an effect that is
moved takes its mix along, and the cables that drive it, which name it; an Undo hands a node back). An `Edit` names a place that wraps round what there is when it is made (the lane
is a place among the lanes), `resolve` makes the `Intent` that a front-end would send from the model as it is, and `make` carries it out through `Controller::apply`, so it is in
the history: a parameter, a mix, a volume, a pan; mute and solo; an effect added, moved or taken out, a lane's or a bus's; a modulator, with its cable or without; a range; a lane
in a group; a lane or a group added or taken out; a preset; the tempo; a source of notes put in or taken out and a cable of notes drawn or taken out (places among the sources, the lanes and the cables of notes); the instrument of a lane taken out (`Edit::ClearInstrument`: skipped for a lane that has none, drawn by no generated session, which would change those of every seed); the link into an effect cut or given back (`Edit::Feed`: a place among the effects of the lanes and then of the buses, wrapped, skipped when the session has none: `FEED_EDITS`, drawn by no generated session); a free modulator made, a cable drawn from one to a parameter of any plugin (`Edit::FreeCable`: places among the free modulators, the plugins of the session and their parameters) and a free modulator taken out (`Edit::{AddFreeModulator, RemoveFreeModulator}`: `FREE_MODULATOR_EDITS`, drawn by no generated session); a free modulator made to hear a lane, a group or every event (`Edit::Ear`: places among the free modulators and the lanes and the groups, wrapped: `FREE_EAR_EDITS`, drawn by no generated session); the limiter put in the sound or taken out and the master's volume or the limiter's ceiling set (`Edit::{Limiter, Master}`: `MASTER_EDITS`, drawn by no generated session); Undo and Redo.

- **The engine is deterministic** (`a_scenario_renders_the_same_bits_twice`): a render is the same bits every time, in every process. It was not: the oscillator's voices were
  in a hash map that was walked in another order each run (fixed in `9c0eef4`), and the note remapper handed out its channels in a hash map's order (`51a9674`). A hash of the
  samples is only worth something once this holds.
- **The engine has the model's nodes** (`Rig::assert_the_engine_has_the_nodes_of_the_model`, called by `render` after every block; P2 of `docs/patch-graph.md` §11.6): the lanes and
  groups that the engine has, and the id of each and of every effect of each, are what the model has after every edit (`Engine::facts`, `AudioGraph::lanes` / `groups`, compared with
  `testutil::model_lanes` / `model_groups`; the audio cables, `Facts::cables` against `testutil::model_audio_cables`; and, since P5a, every modulator's cables by the place that the engine names what they drive by and by depth, and what each is centred on, `Facts::racks`
  and `bases` against `testutil::model_racks`, which the model derives from the nodes the cables name: see "Cables by node and port"; and, since step 93, whom each free modulator hears, `Facts::ears` against `testutil::model_ears`). The edits add, move and take out effects, put lanes in groups, add and take out lanes and groups, and Undo and Redo (and, in `edited:cables`, draw, turn and take out audio cables, in `edited:routes` cut and give back ways out and in `edited:solos` solo and mute lanes and a group that cables go between), so that the ids that
  the controller sends and the journal sends for what undo puts back are held for every session of the corpus, not only for the scenes of `src/app/`'s tests. An engine that has no
  lanes to say (the tests' own) is not held to it.
- **It is what it was** (`the_renders_are_what_they_were`; the whole corpus in `the_whole_corpus_is_what_it_was_and_the_same_twice`, which is `#[ignore]`d: `cargo test --release
  oracle -- --ignored`, about 40 s): `src/oracle/exact/<os>-<arch>.txt` has the hash of every render of the platform it was made on (macOS on Apple silicon is the one in the
  repository), held to the bit. The always-on renders are the same bits in a debug build and in a release build (measured), but the whole corpus is not: twelve of its 978 older renders, in six generated sessions (59, 60, 92, 129,
  179 and 183), differ in the last bits between the two (within the tolerance; found in step 77, when the file was first written by a debug build and twelve lines of older renders changed, which a copy of the commit before
  the step rendered in a debug build gave as well, and the release binary from before it did not), so the file is written by a release build, as the command says; `src/oracle/fingerprints.txt` has the numbers for every platform,
  held to `oracle::TOLERANCE` (1%: another platform's libm differs in the last bit, and `a_last_bit_of_difference_in_the_rate_stays_within_the_tolerance` moves the sample rate by one
  bit, which moves every phase step and coefficient by one, and finds the largest change of a number to be 0.035%). A platform that has no file of its own is held to the numbers
  only; to give it one run `TANG_UPDATE_ORACLE=1 cargo test --release oracle -- --ignored` there. **A change that is meant to alter what the engine renders** runs the same command
  (`TANG_UPDATE_ORACLE=1 cargo test oracle` writes the always-on part only), and says so, with the reason, in its commit. An update of the operating system that changes its libm
  changes the hashes: the failure says whether the numbers are within the tolerance, which is then the libm's.
- **The audio thread does not allocate** (`no_block_after_the_first_asks_the_allocator_for_memory`; all 1,035 in the ignored test): the first block takes the commands of the load, a
  block that an edit was made before takes its commands (they build the graph, and may allocate), and no other block asks the allocator for memory. It found the pattern player's
  lists (`57edbde`), and a cable that is taken out, which left the plan at the start of the block after the one that took the command (it leaves it at the end of the block it goes down in now).
- **Two engines agree** (`oracle::agree(scenario, make_a, make_b)`): the same scenario rendered by two engines (`oracle::engine` is the `AudioGraph` as the program has it, whose lanes and
  groups are run by the kernel's plan; `oracle::legacy` is the same `AudioGraph` run by the loop that the plan replaced, `AudioGraph::process_legacy`, kept in test builds for this; a `make`
  is a function of the `Wiring`, the channels and cells an engine is built from) is compared bit for bit, the first sample that differs and its block named. This is how the plan of P1
  was held to the loop, on every platform and with no file to be out of date: the always-on corpus in every run (`the_plan_gives_what_the_loop_it_replaced_gave_bit_for_bit`) and the whole corpus
  in the ignored test (`the_whole_corpus_is_the_same_under_the_plan_and_under_the_loop_it_replaced`). The loop has no audio cables, no ways out that are cut, no cut links of a chain and no modulators: the scenarios that have any (`Scenario::has_cables`, `has_cut_routes`, `has_cut_feeds`, `has_free_modulators`; `is_in_the_old_loop` is none of them) are not held to it. **Since step 96 that leaves few**: a modulator in a lane, in a group or in a table of its own is the session's own when the file is loaded, and the loop has none of those (its racks went with the engine's: it could run a lane's and a group's, which were the engine's then), so that only 4 of the always-on scenarios are held to it (`the_scenarios_that_cut_a_way_out_are_those_that_the_loop_the_plan_replaced_is_not_held_to_either` says which count) and the modulated ones are held to the stored hashes, which are what they were. It has no latency compensation either (P3f), so the plan is held to it with the compensation off (`oracle::engine_uncompensated`: `AudioGraph::without_latency_compensation`, test builds), which keeps the plan's own machinery held for every session, the ones that have a drive too (210 of the 300 generated sessions have one); and the compensation is held apart: where nothing is late (`Scenario::has_latency`: a built-in drive in the file, or put in by an edit) the engine with it is bit for bit the engine without it (`where_nothing_has_latency_the_engine_is_what_it_is_without_the_compensation_bit_for_bit`, and the whole corpus in the ignored test), and where something is late and is added to what is not (`edited:latency`) it is heard.

The tests that run every time render the sessions, the fourteen that edit cables, ways out, solos, latency, sources, instruments, feeds, an effect that stands by itself, the second input of an effect, free modulators and their ears, the volumes, mixes and pans that cables drive, the audio input and the master's limiter and volume, and the first 24 generated ones at 256 frames (69 renders, about 10 s in a debug build): the determinism, the fixtures, that no block
allocates, that every one of the 300 generated sessions loads, that the generator draws every feature that the table in `oracle::tests` lists (50 of them, each in at least about two
fifths of the number it makes of it), that the sessions are heard, that the edits mostly apply and that an edit is made before the block that has its frame in it, and the tolerance;
and the tests of the edits (what each makes of itself) and of the WAV header.

**The benchmark** (`oracle::bench`: `cargo test --release oracle::bench -- --ignored --nocapture --test-threads=1`) times the callback's `Renderer` alone against the time a block has
(frames over sample rate) for one oscillator and a chord, and for a session that is a lot for the engine (8 lanes of a unison of seven through a ladder filter, a delay, a reverb and a
drive with four modulators each, two groups with a reverb and a filter on their buses: 78 plugins and modulators), idle, with a chord of six and with every lane muted, at 64, 256 and
1024 frames, and counts the allocator's calls. On an Apple M4 Pro (release build, an ordinary thread): 0.6% of the time a block has for the oscillator; 6.8% idle, 15.0% with the
chord and 15.0% with every lane muted for the large session (801 µs of the 5.33 ms at 256 frames): the cost goes with the block, a muted lane costs what a heard one does, and
effects and modulators on silence cost 45% of a sounding session. The numbers and what they say are in `docs/web-ui.md`, step 70.

**WAV files** (`oracle::wav`: `cargo test --release oracle::wav -- --ignored`): the sessions and the first eight generated ones, as the engine renders them, in
`target/oracle-wav/` (32-bit float, stereo), to listen to.
TODO: a recording of a pattern among the edits (`SetRecording` counts in four beats of the metronome first, so that a scenario would be two seconds longer for it); sessions that name
a plugin that has to be installed (the adapters of CLAP, VST3 and LV2 are not in the corpus); exact files for Linux and Windows (their CI is held to the numbers); the cost of a node of
a graph over the DSP in it, which only a kernel with nodes that do nothing can show (P1).

## The kernel

`kernel/` is a workspace crate (P1 of `docs/patch-graph.md` §11.6, step 71 of `docs/web-ui.md`): the machinery of the patch graph, with no interface in it and no knowledge of lanes,
plugins or MIDI. The engine's lanes, groups and output mix are run by it (see "The engine on the kernel", below).

- **`Arena<T>`**: where the state of nodes lives. A `Key` is a slot and a generation: the key of a value that is gone finds nothing and never finds the value that came to its slot
  later, and a slot at the last generation is retired. `remove` hands the value back with its state, so that undo can put back the very node.
- **`Patch<I>`**: nodes (`NodeSpec`: audio ports, each of some channels, and a `rank`) and cables (an output port to an input port). An id is whatever the caller has (the engine's is `Stage`: a part of a lane, a group or an effect, named by the model's id). An input is a sum (`PortSpec::audio`) or `single` (`PortSpec::single`, below).
- **`compile(&Patch) -> Plan`**: the order is the topological order that takes the least `(rank, insertion)` of the ready nodes at every step, so it is a function of the patch and of
  nothing else; a cycle, two nodes of one id, a cable at a node or a port that is not there and a cable between ports of different widths are errors that say where. Every output and every
  sum at an input is a value that is alive from where it begins to the op that reads it, and values that are not alive together share a buffer (the fewest that fit, the lowest free one first).
  A buffer is zeroed where a value begins in it, and the `feeds` of an op add each of its outputs into the sums that its cables end at, in the order of the cables.
- **The sum rule.** The input of a port is a sum that starts at +0.0 and adds its producers in plan order, also for one cable: `0.0 + -0.0` is `+0.0`, today's mix does that, and the oracle
  hashes bits. The sums are in a fixed order whatever runs where, which is what lets a render be the same bits on any number of threads later.
- **A single input** (`PortSpec::single`, P3a): an input that has exactly one cable, which is the only cable that leaves its output, *is that output's buffer*: the output lives to the op that
  reads it, the cable has no feed, nothing is zeroed, added or copied, the bits are the output's own (a negative zero stays one) and the node may work in place on what it was given (it is the
  only reader). With no cable it is silence, and with two or more cables, or a cable from an output that has others, it is a sum as any input is. It is what the stages of a chain are (the
  old chain does not make `0.0 + -0.0` of anything between its stages); the group's bus and the mix stay sums, since the old engine added into zeroed buffers there. The reference
  interpreter has the rule, and the random patches draw half of their inputs `single` (from a generator of their own, so that their shapes did not change); the test nodes follow a
  negative zero end to end (a source makes one as it is, and a node of one input and one output can be a plain copy), so that a sum that should not be, or a buffer that is shared
  wrongly, is seen.
- **`Plan::run(pool, frames, nodes, cx)`**: over a `Pool` that is fitted to the plan (`fit`) and given room (`reserve`), both on a thread that may allocate. The buffers of an op are moved
  into an `Io` and back (a node that works in place hands its input on with `Io::forward`), nothing is allocated (`kernel/tests/no_allocation.rs` counts the allocator's calls, the first block
  after a fit included), and a node that fails stops the block with its error and leaves the buffers where they belong. `Nodes<I, C>` is how the executor reaches nodes (an `Arena<N>` is one,
  for `N: Node<C>`); `C` is whatever the program tells every node of the block. `Kernel<N>` is an arena with its plan, for the crate's tests and benchmark.
- **Latency** (P3f, `kernel/src/latency.rs`, `delay.rs`): `align(patch, latency)` finds, from the latency of each node, how late what arrives at each node and what it makes is and by how many frames each cable is to be delayed so that
  what is added at an input is in step (`Alignment`); `with_delays(patch, alignment, id_of)` makes the patch that has a node on each cable that is to be delayed, and `DelayLine` is what such a node is made of (a ring for each channel that a block of
  any length goes through in place, longer or shorter with the newest of what it holds kept, allocating nothing when it runs: 1.5 ns a frame of stereo, 0.4 µs a block of 256 frames, 0.007% of the time it has, whatever the length of the line, 23 frames or 131,072). `compile` and the executor know nothing of latency: the delays are nodes of the patch that they are
  given, and `compile::layout` (the check of the patch and the order of its nodes) is shared by `compile` and `align`. See "Latency (`Plugin::latency`)".
- **Tests.** `testutil::random_patch` draws patches from a seed (the same on every platform); the order, the colouring (the fewest buffers that fit, over 5,000 patches), the zeroing and the
  feeds are checked against what the patch says, independently of the compile. `reference::run_reference` (a buffer of its own for every output, the sum made at the input) is what the
  executor is held to over 400 random patches, bit for bit, with nodes that fail and plans that are swapped in the middle. `kernel/tests/benchmark.rs` (ignored) is what the graph costs over nodes
  that do nothing: 10 to 15 ns a node of bookkeeping, 73 ns at 256 frames of stereo, and a chain of a thousand nodes is 78 µs a block of 256 frames (Apple M4 Pro).
**The engine on the kernel** (`src/plugin/chain/nodes.rs`). `AudioGraph`'s lanes and groups are plain vectors, and the commands that address them still name places. Each lane and
each group holds the id that the model gave it (`NodeId`, from `crate::ids`: `AddInstrument { node }` and `AddGroup { node }` carry it, and a lane's instrument plugin is named by the
lane's id, as the model has it), and each lane and each group an id for every effect of its chain, one for each mix (`effect_nodes`: `InsertEffect { node }` and `InsertGroupEffect
{ node }` carry them; a lane that gives up its plugins, `KeptLane::take_plugins`, keeps the mixes and the ids, as it keeps the places they were at). The ids go where the node goes: a
swap of two lanes, a lane or a group that undo hands back and puts in again, an effect that is put in, moved or taken out (an effect that is turned away for its channels keeps none).
The loader, the controller and the journal send the model's ids (the loader makes an effect's node before it sends the effect), and what holds the engine's to the model's: `LaneFacts`
and `GroupFacts` (`AudioGraph::lanes` / `groups`, test builds) have them, so every test of `src/app/` that compares the engine with the model compares them, and the oracle compares
them after every block of every render (`Engine::facts`, see "The oracle"). The plan names those ids, and a lane is a **row of nodes** (P3a, `Stage`): a *head* (`Stage::Head(lane id)`: the events through the range, the remap, the pattern and the
transpose, the modulators, the instrument, the volume and the tap after it), a node for each *effect* of its chain (`Stage::Fx(effect id)`: the plugin, the blend by the mix, the tap),
and a *tail* (`Stage::Tail(lane id)`: the metronome's click on top of what the chain made, the fade of a mute or a solo, the meter, the learned controls); a group is the same (its
head takes the sum of its members, a sum input, and applies the volume; its tail has the pan, the tap after it, the fade and the meter); the output mix is `Stage::Master`. The stages
of a chain are `single` inputs, each the buffer that the stage before made; a lane's tail is a cable to its group's head (to the mix if it has none, or if the group it names is not
there) and a group's tail is a cable to the mix (unless the way out of the lane or the group is cut: `route`, see "The way out"; and a stage of a chain has no link from the one before it where its feed is cut: `feed`, see "The input of an effect"), and ranks are the places (the lanes in order, then the groups, then the mix; every node of a lane has the lane's rank), so that the order
of every sum is the order of the loop that the plan replaced: a group's bus is its members added in the order of their places, and the mix is the lanes that have no group and then the
groups. A lane with no instrument runs its effects all the same (step 85: what comes to the first of them is the silence that its head made and what the cables that end at it or after it bring, so that the effects that a lane keeps when its instrument is taken out go on working: a reverb's tail rings out, and what another lane sends into one of them is heard through it; before, `run_fx` returned at once: "nothing to work on", which was true before there were cables), and its tail still adds the click. An instrument is given a buffer for each of its outputs and
no more (the old chain gave the chain's whole width to an instrument that had no effects and a volume of 1), writes into the first of the output's buffers, and, if it has more
outputs than the chain has channels (Pianoteq has sixteen), into the lane's own `inst_buf` from which the first two go on. Two nodes with one id are a plan that does not compile (the
kernel's `DuplicateNode`: logged, and an assertion in a debug build). The vectors are still addressed by place, so the plan is made with a list of where each stage is (`Place`, sorted
by stage: a block finds a node by a binary search, and takes a node that is not at its place any more, because the one that was has gone since the plan was made, for no node: silence,
never the one that is there now). `Schedule` (the plan, its pool, what it was made for) reads the wiring into a `Shape` before every block (a list of numbers: for each lane its id, its group's and its effects' ids,
for each group its id and its bus effects' ids, in the order of the places) and compares it with the one the plan was made for, so that a plan is made again only when the wiring
changed (a lane, a group or an effect that comes, goes, is another at its place, changes place, or a lane that changes group), whoever changed it, and a knob or a mix that is turned
compiles nothing and allocates nothing.
**Audio cables** (P3b in the engine, sent by the controller since P3c-1: see "Audio cables (`[[cable]]`)"): `GraphCommand::{AddCable { id, from, to, gain }, RemoveCable { id }, SetCableGain { id, gain }}` by `EdgeId`, from the instrument of a lane
(`CableFrom::Instrument`: what it puts out after the lane's volume and before its chain), an effect (`Effect`: after its mix, a lane's or a bus's) or a group (`Group`: after its pan and fade) to an effect
(`CableTo::Effect`: added to what its chain gives it), the bus of a group (`GroupBus`) or the mix (`Master`), all named by the model's ids. A cable is a node of the plan (`Stage::Cable`, with the rank of the
node that owns its start) whose body is `fade` from the level it was at (`at`, 0 for a cable that has just been drawn) to the level it is going to (`goal`), the code that lets a muted lane go: it comes in,
goes down and is turned over one buffer, with no click. `goal` is the level that was set times what its owner is heard at (`Schedule::set_gates`, between `refresh` and the plan: a mute or a
solo takes a cable with it, a lane in a muted group keeps its sends, the output of a group has been let in or out already and is not gated again, and what a cable carries into a lane that is not soloed is heard: see "Mute and solo"), and 0 for a cable that was told to go. The cables are
kept by id and put in the patch in that order (`build_patch`, which `compile` and the check for a loop both use), so that the plan and the sound are a function of the wiring and not of the order of the
commands; the shape has them in it. `AddCable` refuses with a log line an end that is not there (the id of a group for an instrument, and the other way round), a level that is not a number and a cable that
makes the wiring not compile, a loop (compiled on the side first); a cable that is drawn again under its id goes on from where it was; `RemoveCable` brings one that is heard down over a buffer and takes
it out of the plan at the end of that buffer (so that the block that took the command is the one that makes the plan again, and asks for memory, and the blocks after it have nothing to make); a cable whose lane, effect or group goes, goes with it. At an output that has a cable and the chain link, the link is a sum of one (the kernel's rule for a `single` input).
A cable costs 0.45 µs a block of 256 frames (`docs/web-ui.md`, step 74).
`AudioGraph::process` is: the commands; the
plan (`refresh`); what is heard (`Schedule::set_gates`: the `heard_goal` of every lane and group, the head gates and the cables' goals, from the mutes and solos of all of them and from what the cables carry, worked out before any runs); the modulators (they reach across
lanes and groups, so they are not nodes until P5: `process_events`, which ticks them and writes what their cables drive before `Plan::run`); `Plan::run`; and what the modulators drove, published. The nodes run the code that was in the loop, cut where it was cut by what it did
(`run_head`, `run_fx` and `run_tail` of the lane and of the group, in `nodes.rs`: a tail hands its buffer on with `Io::forward`, and the plan adds a lane's into the bus of its group or into
the mix); the mix is copied to the output, measured, and given to the scope. The old loop is `AudioGraph::process_legacy` (`#[cfg(test)]`, with `InstrumentLane::process` and
`Group::finish`, which are the lane and the group as one piece, and the `accum`, `mix_buf`, `lane_buf`, `buf_a` and `buf_b` that only they use: the specification, kept as it was), and the
oracle holds the plan to it bit for bit. The plan costs what the loop did: 817 µs a block of 256
frames for the stress session with a chord of six, 814 µs the loop (release, Apple M4 Pro, `oracle::bench` times both; measured again after P3b: see `docs/web-ui.md`, step 74).
**Latency** (P3f, see "Latency (`Plugin::latency`)"): `Schedule::compile` asks `align` for the delays of the patch that `build_patch` made (the latency of every instrument and effect that has one, `latencies_of`, held to `MAX_LATENCY`), makes the patch with them in it (`Stage::Delay`) and compiles that; each delay is a `Link`, which the schedule keeps by the two nodes that its cable joins, so that a plan that is made again keeps the line and what it holds, and the dry sound of an effect that has latency has a line of its own, by the effect's id, that `run_effect` puts the input through before the blend. The latencies are in the `Shape`, so that a plugin that says another makes the plan again. A session where nothing has latency has no delay and no line: the plan is what it was. The stress session of the benchmark (8 lanes with a drive each, at a mix of 1) costs 815 µs a block of 256 frames with a chord of six, where the loop that the plan replaced, which makes nothing up, costs 812 µs.
TODO: the plan is compiled and the pool fitted on the audio thread, in `Schedule::refresh`, when the wiring has changed (a block that an edit was made before may allocate, as the first
may); the main thread can make the plan now that the nodes have the model's ids, and the audio thread would find each id's place when the plan is installed (the list that `Place` makes
is that), but nothing needs it before P3's cables, which are by id. The commands that address a lane, an effect or a group still name places; the functions that kept a
modulator's references in step with them are gone (the model's with step 89, the audio thread's with step 90). Not in the crate yet: feedback cables (P3), events (P4),
control ports (P5), a skip of silent work and more cores (P7).

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
  `SetNodeUi`, `SetPinned`, `SetBpm`, `SetRecording`, `ClearPattern`, `SetPatternField`, `MoveEffect`, `AddGroup`, `SetLaneGroup`, `RenameGroup`, `RemoveNode` (a modulator, an effect, a lane
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
  design tokens as CSS variables (`web/src/ui/tokens.css`), the controls
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
TODO: the places that the layout remembers are the page's: a page that is opened again, and a second browser, lay the nodes that no person has placed out from nothing, so that the canvas is not what it was when it was left (docs/web-ui.md D45, Open question 6).
TODO: until the nodes are arranged a cable between two modulators that are in one row runs across what is between them, and its slider can be over a node (the cables' routes are made for the layout from nothing).
TODO: the view moves to show a node that is added where it cannot be seen, and may leave the others out of it.

## Platform

Cross-platform: Linux, macOS, Windows.

LV2 support is behind the `lv2` Cargo feature (enabled by default). The LV2
dependency chain (livi → lilv → lilv-sys) requires the `lilv` system C
library:

- **Linux**: `apt install liblilv-dev` (or distro equivalent)
- **macOS**: `brew install lilv`
- **Windows**: not supported

The upstream `lv2-sys` crate (RustAudio/rust-lv2) doesn't compile on macOS
because its `unsupported.rs` is a `compile_error!`. PR #117 fixes this with
a one-line `cfg_attr` aliasing macOS to the Linux bindings. Until that PR
merges, `Cargo.toml` patches `lv2-sys` to akx's fork (1 commit ahead, 0
behind upstream develop). Drop the `[patch.crates-io]` entry once #117 lands.

VST3 support is behind the `vst3` Cargo feature (enabled by default). It uses
pre-generated bindings (no build-time SDK dependency) and works on all platforms.

The `web` Cargo feature (off by default) is the browser front-end: `ts-rs`
(which generates the TypeScript of the protocol types), the server (`axum`,
`tokio`) and `ctrlc` for stopping `tang serve`. `tokio-tungstenite` and
`futures-util` are in it for the tests' WebSocket client (axum's `ws` is built on
the same crate). See "Web UI".

CI runs clippy + tests on all three platforms:

- **Linux** — full default features (LV2 + VST3 + CLAP), `apt install liblilv-dev libasound2-dev libjack-dev`
- **macOS** — full default features (LV2 + VST3 + CLAP), `brew install lilv`
- **Windows** — `--no-default-features --features vst3` (CLAP is not feature-gated, so it's always built; LV2 is unsupported)

Each of them then also runs clippy (`--all-targets`) and the tests with `--features
web` added, which includes `tests/serve.rs`, the real binary on `--audio-device
none`. What needs a Unix there is `#[cfg(unix)]`: the two Ctrl+C tests (they send the
signal with `kill`) and the three ignored tests that make virtual MIDI ports. The job
`typescript` checks that the generated TypeScript is current. The job `kernel` builds
and tests the workspace crate `kernel/` alone (clippy, the tests in debug and in
release) on the three platforms: it has no plugin format, sound card or interface in
it (see "The kernel"). The job `web` (Linux) runs the page's lint, types, Vitest,
production build and Storybook build, builds the program with `--features web` and
runs the Playwright suite against it (`npm run e2e`); a failed run keeps
`playwright-report` for seven days, whose traces have the values that a test read.

A `.gitattributes` (`* text=auto eol=lf`) keeps every text file LF in a Windows
checkout: the golden snapshots, the generated fixtures and the session text that tests
edit are compared as written, and CRLF failed 38 tests there. The runners are several
times slower than the machine that the tests were written on, so what waits on a clock
has a bound for them (Vitest's `testTimeout` is 30 s under `CI` and 5 s otherwise), and
a browser test of the real program reads the most of a live value over a window and
not one reading at a moment: the tracer's LFO makes the sound depend on the phase, and
a runner's timeline is steady, so that a reading that is bad at one phase is bad every
time (`e2e/master.spec.ts`, `held`).

## Future ideas

- A patchable voice (track B, `builtin:patch`): a proposal, with what the engine has and lacks, the order of work and the ten decisions that
  are the user's, is `docs/track-b.md`; nothing of it is started
- Audio and MIDI as cables (the user's idea of 2026-10-20: inputs and outputs on every node and connections that can be changed, "connect two
  different osc to the same reverb", MIDI treated the same way): the session as a patch of lanes, buses, keyboards and cables of three kinds (audio,
  MIDI, the modulation that exists). A proposal, with what the engine has and lacks, a staged path (audio cables as a routing table on the lanes and
  groups first, then MIDI sources, then nodes between, a general engine only if wanted) and the twelve decisions that are the user's, is
  `docs/patch-graph.md`; it is being built (see below). Its §11 is the design without a time limit (the user asked for the best way, whatever it costs to
  build, for a modular system that is still fast), and it replaces the order of the staged path: a typed graph with ids, compiled on the main thread into a
  plan that a small real-time kernel runs over node state that outlives every plan; audio, events (with note ids and sample times) and control (modulation)
  as kinds of port; the lane, the group and the rack as templates of nodes; modules and the voices of track B on the same kernel; reached by growing the
  kernel under today's structure with the old engine as a bit-for-bit oracle, not by a routing table that would be thrown away. It contains the two ideas
  at the end of this list (a keyboard as a node, audio input as a source). The user said go on 2026-10-20 ("lets go e2! I like this. I like it all!"), which takes the
  recommendations of its §11.7 as the plan. **P0 (the oracle and the boundary) is built** (step 70 of `docs/web-ui.md`; see "The oracle"); **P1 (the kernel,
  with today's lane and group as two coarse nodes, bit for bit equal to the old engine) is built** (step 71; see "The kernel"); **P2 (ids for places, narrowed to the nodes that cables will join: the engine has the model's ids for its lanes, groups and effects, and the plan names them) is built** (step 72); **P3a (the lane and the group as rows of nodes in the plan, and the kernel's input that is its cable's own buffer) is built** (step 73); **P3b (audio cables in the engine: a cable is a node of the plan that comes, goes and is turned over a buffer, goes with the mute of its lane and is not drawn if it would close a loop) is built** (step 74); **P3c-1 (cables as sends: the model, the graph a client gets, the intents `AddCable` and `SetCableGain`, `RemoveEdge` for an audio cable, the controller's refusals of a loop, the cables that go with a node, undo and the file's `[[cable]]`; see "Audio cables (`[[cable]]`)") is built** (step 75); **P3d (the page draws audio cables: dragged from an output to an input that takes them, a level on the cable, a menu for the keyboard, undo, and the first example with two oscillators into one reverb: `examples/shared-reverb.toml`) is built** (step 76); **P3c-2a (the way out of a lane and of a group, `route = "none"`: in the file, the model, the engine, the intents and the history, the terminal's `w` and a button on the page, with the oracle's `routed` session and `edited:routes` scenario; see "The way out") is built** (step 77); **P3c-2b (the law of mute and solo for what a cable carries, with its head gate: the engine, the oracle's `edited:solos`, the page's copy of the law and the mock engine's meters; see "Mute and solo") is built** (step 78); **P3e (the terminal's patchbay: the Patch tab, a matrix of outputs against inputs; see "Patch tab") is built** (step 79); **P3f (latency: `Plugin::latency`, the paths that are added put in step, the dry sound of an effect that is late; see "Latency (`Plugin::latency`)") is built** (step 80); **P4a (sources and masks in the engine: an event has a source, a lane has the set of sources it hears and lets go of what a source it stops hearing has sounding; see "MIDI routing") is built** (step 81); **P4b (MIDI cables in the model, the file, the intents and the history: a source of notes is a node that a person adds, a cable of notes goes from it to a lane, a lane that has none hears every source, and the controller tells the audio thread what each lane hears; devices that come and go are noticed, and on macOS late-plugged devices are opened; see "Sources of notes and cables of notes") is built** (step 82); **P4c (the page draws sources of notes and cables of notes: source nodes before the lanes, a cable dragged from an output of events to a lane's input, menus for the keyboard, the links by which a lane hears every source, the bar's "+ Source", the message that says which MIDI inputs are open, and the mock engine pinned by fixtures; see "The page" under "Sources of notes and cables of notes") is built** (step 83); **P5.0 (every audio link is a cable to a client: the controller gives a cut link back for a cable drawn between its ends and refuses one where the link is, a module that stands by itself is bare, the Patch tab and the page have the links as cables; see "Links are cables") is built** (steps 88a and 88b: the controller's rule, then the page's picture of it); **P5a (the model keeps a modulation cable by the node and the port it drives, and not by a place in a rack: the twenty functions that kept places in step are gone from `model.rs`, `kind_of` says the place that the audio thread still names, the loader makes a cable of a file against the nodes there are, and the oracle holds the engine's places to the model's after every block; see "Cables by node and port") is built** (step 89); **P5b-1 (the audio thread names what a modulation cable drives by the node, as the model does: the engine's cables have the model's edges and its modulators the model's nodes, it finds the plugin, the modulator or the cable when it applies a rack, and its 32 place-keeping functions are gone; the oracle's whole corpus is the same bits; see "Cables by node and port") is built** (step 90); **P5b-2 (a modulator that is in no lane and no group: `ModulatorLocation::Free`, the intent `AddFreeModulator`, a top-level `[[modulator]]` in the file, the terminal's `F`, an oracle session and scenario, and `examples/shared-lfo.toml`, one LFO on two lanes; see "Free modulators") is built** (step 91); **P5b-3 (the page makes and aims a modulator that is in no lane and no group: the bar's "+ Modulator", a row of them above the first lane, the reach of a modulator that has no parent in `client/reach.ts`, the `~` menu's "Attach", the mock engine pinned by the controller's fixtures; see "Free modulators") is built** (step 92); **P5c-1 (the ear: a modulator that is in no lane and no group hears one lane's or one group's notes instead of every event, the one thing that a lane's or a group's modulator had and a free one had not: the intent `SetModulatorEar`, `hears = "instrument[0]"` in the file, `h` in the terminal and a ♫ button on the page, with an oracle session and scenario; the first step of making every modulator global, whose plan is in `docs/patch-graph.md` §11.6; see "Free modulators", "The ear") is built** (step 93); **P5c-2 (every modulator is global: what the page and the terminal make is one of the session's own with the ear of the knob's lane or group, a loaded session's lane and group modulators are moved into the session's rack with the ears they had and saved the new way, and the oracle holds a moved session to the one that is not, bit for bit but for three decisions: two modulators on one parameter add up, a group's cable to a member's modulator acts a block later, and a lane or a group that is taken out leaves the modulators that heard it; see "Free modulators", "Every modulator is global") is built** (step 94); **the small half of P3g (an effect has a second input of audio that only cables end at, and the Math is the effect that has one: `Plugin::side_input` and `process_side`, `CableTo::EffectSide`, the port `audioInSide`, `.side` in the file, a column in the Patch tab and a handle on the page, an oracle session and scenario and three examples: see "An effect with a second input") is built** (step 95; **a parameter that says when it has an effect, and a knob that has none dimmed: the Math's carrier while a cable is patched at B, its bits outside the logic modes** is built too, step 95b; **P5c-3 (the lanes' and the groups' racks are taken out of the model, the engine, the controller, the terminal and the page: every modulator is the session's own, and a loaded file's `[[instrument.modulator]]` and `[[group.modulator]]` are read and moved and not written; see "Free modulators", "The racks go (step 96)") is built** (step 96); **a modulator drives the volume of a lane, the mix of an effect and the volume and the pan of a group, which the host keeps for a node and a plugin has not among its parameters (the user's "I cannot modulate everything, for example mix and some volume, is that fixable?", after step 96: `ModGain`, `Driven::Gain`, `gain = "mix"` in the file, one ramp a buffer in the engine that leaves the user's value as it is; see "The volume, the mix and the pan that a modulator drives") is built** (step 97); **the Arrange button of the canvas makes the cables short, with the room of a cable kept (the user's "we need to rethink the arrange into something smart that minimizes the length of cables", the second half of the message of step 97: `web/src/app/patch/arrange.ts`, a search over the places of the nodes with the page's own curves for the cables, what is over nodes priced, a margin and a pull for nodes that have no cable; see "Arranging the nodes") is built** (step 98; and step 98b: the user's two oscillators of `bell-2` one over the other, by a cable that ends at the column of its knob, the nodes that feed one node in a column, the cables at two inputs not crossed and the best of twelve starts); **the microphone** (`builtin:input`: an instrument in a lane that starts bare, a thread that listens to the sound card and a ring that the audio thread reads, a tone on the null backend; see "The audio input") is built too, step 99; **the master's limiter and volume** (the user's "I think we need a limiter or a limit-option on the master, as we often reach the clip-lvl and its hard to control": a look-ahead peak limiter in the sound by default at −1 dBFS, a master volume, `[master]` in the file, `SetLimiter`, `L`, `C` and `V` in the terminal and a row of controls on the master's card; see "The master limiter and volume") is built too, step 102; **a chain of effects as a module** (the user's "In the future, I want to be able to design a set of modules, save them as a stand-alone module that you can then load inside another instrument": the chain of a lane saved to a file of a library with `SaveModule`, listed in the catalog as `module:<name>` and added whole, in a lane of its own, by `AddFreeEffect`; `K` and `N` in the terminal, a button and the bar's "+ Effect" on the page; see "Modules: effects and instruments, with the cables between them, saved to a file") is built too, step 104, and **any plugins that are chosen on the page, with the audio cables that are between them, as a module** (the user's picture of three effects joined by cables, "the goal is to save the graph of fx or instruments ... into its own module": `SaveSelectionAsModule`, a module of a lane for each of its own with its instrument if it has one, added bare by the same `AddFreeEffect`; the page's Select button and its box; see the same section) is built too, step 105, and **the modulators that are chosen with them, with their cables to what is in the module** (the user's "yes agree LFOs should be next!": a `[[modulator]]` with `hears` and its cables in the module's file; see the same section) is built too, step 106, and **a module that is added as one box** (the user's "the goal when we re-add a module is that it gets rendered like just a single box": a record of the lanes and the modulators that one add made, `ModuleInstance`, `[[module]]` in the file and `NodeKind::Module` on the wire, taken out whole as one step, which the engine does not know, and which the page draws as one box that opens; see "A module that is added is one box") is built too, step 108, and **a selection collapsed into one box where it is** (the user's "compact/collapse them into a module in place ... to clean up the ui a bit": `CollapseIntoModule` makes the step-108 record from lanes and modulators that are already in the session, in one step that Undo takes away, with the box where the nodes were and a name that a person types; `UngroupModule` takes a box apart and keeps what is in it; the engine is told nothing; see "Collapsing what is chosen into one module where it is") is built too, step 109; the rest of P5c (the lane dissolves) and P5d of `docs/patch-graph.md` §11.6 are next); the rest of P3 (`docs/patch-graph.md` §11.6: the rest of P3g, the ports that plugins name for themselves) and of P4 (P4d the terminal's patchbay; then time stamps, events in the kernel and note ids) is next
- Recording audio output to WAV file
- Auto-reconnect on audio device disconnect
- Background/daemon mode with system tray icon
- In-app volume control
- MIDI device selection tab
- Audio device selection tab
- A keyboard as a node: the notes of the computer's keys, an on-screen piano or a MIDI device as a node with a "MIDI cable" to a
  lane, in place of the lane's own key range, so that there can be several keyboards with ranges of their own (a split, a
  layer) and other sources of notes (a pattern, an arpeggiator, Web MIDI) are nodes as well (a port kind next to audio and
  modulation; today every note goes to every lane through `NoteInput` and each lane filters it by its range)
- Audio input as a source: a lane whose chain starts at an input of the sound card (a microphone, a line) in place of an
  instrument, through the same effects, groups and modulators (an envelope follower is a modulator to come with it); and
  from there Tang as a plugin (VST3 / CLAP) whose buffer comes from the DAW: `audio::Renderer` is already the one code the
  sound card's callback and the null backend run, so a plugin's `process` would be a third thing to drive it
  The user's direction of 2026-10-28, mid-turn while step 96 was being finished, and the next thing they want after it: "for future (after this) I want to be able to
  add my computer microphone as an input that I can connect to other devices via audio cable." In the model of the cables that is a **source node with an output of
  audio** (as the keys and a MIDI device are sources of notes: see "Sources of notes and cables of notes"), added from the bar like "+ Source" and drawn as a node whose output
  is an audio cable that can end at any effect, any group's bus or the mix (`AddCable` with a start that is the input, and the same law of mute and solo, latency and loops); the
  engine would need an input stream (cpal) that fills a lock-free ring for the output callback to read, and the null backend a silence or a test signal for CI. Open questions for
  when it is started (a first sketch in `docs/web-ui.md` §13): which device (the system's default input, or a name in the file as `[[device]]` has, and a flag beside `--audio-device`),
  the permission that macOS asks of a program that opens a microphone, the feedback of a microphone that is heard through the speakers it listens to (start muted or at a low gain?),
  mono into both channels, devices of two sample rates, and the latency of the input, which the delay compensation would add to what the cables carry. **Built at step 99 as an instrument in a lane that starts bare** (`builtin:input`: see "The audio input"), and not as the source node sketched here: the node of its
  own is P5c's, and Tang as a plugin is still to do.

## Notes for Claude

- This document serves as a design document for us.
- We use TODO:s (on new lines) to clarify what is yet to be implemented.
- Notify me when you find refactorings we should do before implementing new things.
- Notify me when you find discrepancies in this document vs how it actually works.
- Commit hashes cited in this file and in `docs/` are from the step-by-step history of this work, which went into `main` as one squashed commit: it is kept as pull request #10 (`git fetch origin refs/pull/10/head:web-ui-history`).
