# The session file, and how a lane is wired

Part of what `CLAUDE.md` held in one piece until it was split (2026-11-04): the file format beyond the basics (groups, mute and solo, the way out of a lane, the input of an effect, an effect that stands by itself, places and pins), tempo, patterns, note remapping and the example sessions. `CLAUDE.md` has the index of the reference files. The sections keep the titles and the text they had, which the other documents and the comments in the code refer to ("see ..."), and the `TODO:` lines are where they were.

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
TODO: the bar is full again: with "+ Effect" it needed 1,444 pixels in a row with the field of Save as open, with "+ Modulator" (step 92) 1,555 pixels, and 1,467 since the page lost "+ Group" (step 103), and 1,561 with the `+ Module` that step 111 gave modules (they were listed with the effects until then; `e2e/saveasbar.spec.ts`): an `Add ▾` menu (instrument, effect, modulator, source, module) would take the five buttons back to one.
TODO: `N` is not in the action bar of the Session tab (its hints are in about a hundred snapshots; the Help tab lists it).

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
