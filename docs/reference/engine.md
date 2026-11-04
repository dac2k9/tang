# The engine

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): the threads and the exit, the master limiter and volume, latency, the kernel of the patch graph (the signal chain is in `CLAUDE.md`). `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

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
