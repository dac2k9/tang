// The nodes of the canvas (docs/web-ui.md §9.3): what each kind of node of the session is
// drawn as. A plugin node is a function of its data and the controls in it read their own
// values, so that a value that changes renders its control and nothing else.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { effectItems, instrumentItems } from "../../client/catalog";
import { keptWords, removalWords } from "../../client/removal";
import { holdersOf } from "../../client/holders";
import { inputsStoodFor } from "../../client/inputs";
import { slotReader } from "../../client/readers";
import { audibleAt, chainPlaceOf, envelopeStagesOf, hasWindowOf, nodeById, paramCountOf, presetsOf, removalOf, sameChainPlace, sameRemoval, sameStages } from "../../client/selectors";
import { NAME_LIMIT } from "../../protocol";
import type { ModulatorKind, Node, NodeId, Preset } from "../../protocol";
import { formatValue } from "../../ui/Knob";
import { LiveEnvelope } from "../../ui/LiveEnvelope";
import { LiveTrace } from "../../ui/LiveTrace";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { Meter } from "../../ui/Meter";
import { useClient, useClientSelection, useClientState, useEditor, useFrames } from "../ClientProvider";
import { focusWhenPossible, keepFocus } from "../focus";
import { AudioPort, DepthPort, DrivePicker, ExposeControl, GainCell, GroupCell, LimiterCell, ModulatorSettingCell, MuteSoloCell, NotesPort, ParamCell, RangeCell, StepsControl, SyncControl } from "./controls";
import { controlDetail, kindOf, outputScale, seconds, settingsOf } from "./describe";
import { HearsMenu } from "./HearsMenu";
import { InspectorContext, paramsButtonId } from "./inspector";
import { LevelStrip } from "./LevelStrip";
import { ModulateMenu } from "./ModulateMenu";
import { ModuleTagButton } from "./ModuleTag";
import { NodeScope, ScopeButton, ScopePanel } from "./NodeScope";
import { NotesFromMenu, NotesToMenu } from "./NotesMenus";
import { PatternCell } from "./PatternCell";
import { ReductionMeter } from "./ReductionMeter";
import { SendMenu } from "./SendMenu";
import type { CableRow, PatchNode } from "./toFlow";

/**
 * The header of a node: its name (or, for a node whose name is edited in place, `field`), its badge and its buttons. The badge, what kind of plugin it is, is under the name, in small type: beside it,
 * it left the name 40 pixels in a lane that has a number in its name and a "×" ("Os…"), and 30 in a lane of Pianoteq 9 ("Pian…"). `sub` is what goes after the badge on that line (the button that
 * sends the audio of the node: a lane's header has no room in its row of buttons for another).
 */
export function Header({ name, badge, sub, children, field }: { name: string; badge?: string | null; sub?: ReactNode; children?: ReactNode; field?: ReactNode }) {
  const badged = badge ? <span className="tang-node__badge">{badge}</span> : null;
  return (
    <header className="tang-node__header">
      {field ? (
        <>
          {field}
          {badged}
        </>
      ) : (
        <span className="tang-node__title">
          <span className="tang-node__name" title={name}>
            {name}
          </span>
          {badged}
          {sub}
        </span>
      )}
      {children}
    </header>
  );
}

/** The kinds of modulator a lane or a group can be given, as the menu offers them (and the bar's, for one that is in no lane and no group). */
export const MODULATOR_KINDS: readonly MenuItem<ModulatorKind>[] = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "macro", label: "Macro" },
  { id: "midiLearn", label: "MIDI control" },
  { id: "velocity", label: "Velocity" },
  { id: "key", label: "Key" },
  { id: "steps", label: "Step sequencer" },
];

/** The `id` of the button that adds a modulator to the lane or group `node`, which what takes a modulator out gives the focus to. */
const addModulatorId = (node: NodeId): string => `add-modulator-${String(node)}`;

/** The `id` of the button in the bar that adds a modulator that is in no lane and no group, which what takes one out gives the focus to. */
export const ADD_FREE_MODULATOR_ID = "add-free-modulator";

/** The button of a lane or a group that adds a modulator for it: a menu of the kinds. The modulator is the session's own (a cable from it can end at any plugin) and hears the notes of the lane or the group. */
function AddModulatorMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  return (
    <MenuButton
      id={addModulatorId(node)}
      label={`Add a modulator to ${name}`}
      items={MODULATOR_KINDS}
      onChoose={(kind) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addFreeModulator(kind, node);
      }}
    >
      +
    </MenuButton>
  );
}

/** The `id` of the button that adds an effect to the lane or group `node`, which what takes an effect out gives the focus to. */
const addEffectId = (node: NodeId): string => `add-effect-${String(node)}`;

/** The button of a lane or a group that puts an effect at the end of its chain: a menu of what is installed, under the format of each. */
function AddEffectMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => effectItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id={addEffectId(node)}
      label={`Add an effect to ${name}`}
      className="tang-node__fx"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addEffect(node, plugin);
      }}
    >
      fx
    </MenuButton>
  );
}

/** The `id` of the button that changes the instrument of the lane `node`, which the focus stays on (the lane's node stays). */
const changeInstrumentId = (node: NodeId): string => `change-instrument-${String(node)}`;

/**
 * The button of a lane that puts another instrument in it: a menu of what is installed and can be one, under the format of each.
 * An empty lane (the TUI can empty one) is asked to be given one. The lane keeps its effects, modulators, range and volume; the
 * cables that drove the old instrument's parameters go with it.
 */
function ChangeInstrumentMenu({ node, name, empty }: { node: NodeId; name: string; empty: boolean }) {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => instrumentItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id={changeInstrumentId(node)}
      label={empty ? `Choose an instrument for ${name}` : `Change the instrument of ${name}`}
      className="tang-node__instrument"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.replaceInstrument(node, plugin);
      }}
    >
      ♪
    </MenuButton>
  );
}

/** (The same empty list every time, for what depends on one.) */
const NO_PRESETS: readonly Preset[] = [];

/** Whether two answers of `presetsOf` are the same list and the same preset: what the menu depends on, and not the values of the plugin. */
const samePresets = (a: ReturnType<typeof presetsOf>, b: ReturnType<typeof presetsOf>): boolean =>
  a === b || (a !== null && b !== null && a.presets === b.presets && a.current === b.current);

/**
 * The button of an instrument, or of an effect of a lane, that loads one of its plugin's presets: a menu of their names, the one that was
 * loaded last marked, and the menu opening on it. Nothing for a plugin that has no presets, and for a bus effect (the audio thread
 * loads a preset by a lane's slot). The parameters take the preset's values when the server says it was loaded.
 */
function PresetMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const found = useClientSelection((state) => presetsOf(state, node), samePresets);
  const presets = found?.presets ?? NO_PRESETS;
  const current = found?.current ?? null;
  const items = useMemo<MenuItem[]>(() => presets.map((preset) => ({ id: preset.id, label: preset.name, current: preset.name === current })), [presets, current]);
  if (items.length === 0) return null;
  return (
    <MenuButton
      id={`presets-${String(node)}`}
      label={`Load a preset into ${name}`}
      className="tang-node__presets"
      items={items}
      onChoose={(preset) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.loadPreset(node, preset);
      }}
    >
      ≡
    </MenuButton>
  );
}

/**
 * The button of an instrument or an effect that puts all the parameters of its plugin in the panel (the node has room for a few
 * knobs): pressed again, it takes the panel away. Nothing for a plugin that has no parameters.
 */
function ParamsButton({ node, name }: { node: NodeId; name: string }) {
  const inspector = useContext(InspectorContext);
  const count = useClientState((state) => paramCountOf(state, node));
  if (count === 0) return null;
  return (
    <button
      type="button"
      id={paramsButtonId(node)}
      className="tang-menu-button tang-node__params nodrag nopan"
      aria-label={`Parameters of ${name}`}
      title={`All the parameters of ${name}`}
      aria-expanded={inspector.inspected === node}
      onClick={() => {
        inspector.toggle(node);
      }}
    >
      ⚙
    </button>
  );
}

/** The `id` of the button that opens the plugin's own window of the node (step 112). */
export const windowButtonId = (node: NodeId): string => `window-${String(node)}`;

/**
 * The button of an instrument or an effect that opens its plugin's own window (step 112, "a button to open the official UI"): the face that the plugin draws for itself, in a window on the machine that runs Tang,
 * not in the browser, which takes the keyboard from that machine's terminal; pressed again, it brings the window to the front. Nothing for a plugin that has none (every plugin but a VST3 one), and nothing while the
 * server's machine cannot show a window (it has no screen: a login over ssh, a container). What is done in the window comes back as changes of the plugin's parameters, which the knobs of the page show.
 */
function WindowButton({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const has = useClientState((state) => hasWindowOf(state, node));
  if (!has) return null;
  return (
    <button
      type="button"
      id={windowButtonId(node)}
      className="tang-menu-button tang-node__window nodrag nopan"
      aria-label={`Open the window of ${name}`}
      title={`Open the plugin's own window, on the machine that runs Tang`}
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.openEditor(node);
      }}
    >
      UI
    </button>
  );
}

/** The `id` of the button that moves the effect `node` one place earlier or later in its chain, which the focus is kept on. */
const moveEffectId = (node: NodeId, direction: "earlier" | "later"): string => `move-${direction}-${String(node)}`;

/**
 * The two buttons of an effect that move it one place along its chain: earlier (towards the instrument or the group) and later (towards the master).
 * The audio goes through the effects in the order of the chain, a lane's or a bus's alike; the cables that drive the plugins stay on them. The
 * button of a direction the effect cannot go (the first effect has no place earlier, the last none later) says so and does nothing, and keeps the
 * focus where it was, which a button that was taken away or disabled would not. The canvas puts the effect's node in its new place among the
 * others, and a browser lets go of the focus of an element that is moved in the page (of the node that is put after the one it was before, not of
 * the one that stays): a button that was pressed with the focus on it is given it again.
 */
function MoveEffectButtons({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const place = useClientSelection((state) => chainPlaceOf(state, node), sameChainPlace);
  if (!place) return null;
  const buttons = [
    { direction: "earlier", glyph: "‹", step: -1, available: place.index > 0 },
    { direction: "later", glyph: "›", step: 1, available: place.index < place.count - 1 },
  ] as const;
  return (
    <>
      {buttons.map(({ direction, glyph, step, available }) => (
        <button
          key={direction}
          id={moveEffectId(node, direction)}
          type="button"
          className="tang-menu-button tang-node__move nodrag nopan"
          aria-label={`Move ${name} ${direction} in its chain`}
          title={`Move ${name} ${direction} in its chain`}
          aria-disabled={!available}
          onClick={(event) => {
            if (!available) return;
            const focused = document.activeElement === event.currentTarget;
            // (If it is refused the page says so: `Editor` puts the reason among the notices.)
            void editor.moveEffect(node, place.index + step).then((moved) => {
              if (moved && focused) keepFocus(moveEffectId(node, direction));
            });
          }}
        >
          {glyph}
        </button>
      ))}
    </>
  );
}

/**
 * The button of a modulator, an effect or a source that takes it out, with the cables that drive it (or, for a source, the cables of notes from it); the focus goes to `focusTo`, the button that adds one
 * like it (to its lane or group, or to the session), if there is one to go to.
 */
export function RemoveNodeButton({ node, name, focusTo }: { node: NodeId; name: string; focusTo: string | null }) {
  const editor = useEditor();
  return (
    <button
      type="button"
      className="tang-node__remove nodrag nopan"
      aria-label={`Remove ${name}`}
      title={`Remove ${name}`}
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.removeNode(node).then((removed) => {
          if (removed && focusTo !== null) focusWhenPossible(focusTo);
        });
      }}
    >
      ×
    </button>
  );
}

/**
 * The button of a lane or a group that takes it out of the session: a menu that asks, first for keeping (where the focus starts), and then for removing it with the words of what goes with it
 * and what stays. A lane that has an instrument is asked for twice over: for taking the instrument out alone, which leaves the lane with its effects and the rest (an empty lane,
 * which the focus goes to the button that puts an instrument in), and for taking the lane out, for good, with all of it, which is what goes to the button of the bar that adds a lane. The only lane
 * of a session cannot be taken out (a session keeps a lane), but its instrument can; and nothing for any other node.
 */
function RemoveLaneOrGroupMenu({ node }: { node: NodeId }) {
  const editor = useEditor();
  const client = useClient();
  const removal = useClientSelection((state) => removalOf(state, node), sameRemoval);
  if (!removal || (removal.last && !removal.instrument)) return null;
  const what = removal.kind === "lane" ? `the lane of ${removal.name}` : removal.name;
  const items: MenuItem<"keep" | "instrument" | "remove">[] = [
    { id: "keep", label: `Keep ${removal.name}` },
    ...(removal.instrument ? [{ id: "instrument" as const, label: `Remove ${removal.name} only${keptWords(removal)}` }] : []),
    ...(removal.last ? [] : [{ id: "remove" as const, label: `Remove ${what} for good${removalWords(removal)}` }]),
  ];
  return (
    <MenuButton
      id={`remove-${String(node)}`}
      label={`Remove ${what}`}
      className="tang-node__remove-menu"
      items={items}
      onChoose={(choice) => {
        if (choice === "keep") return;
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (choice === "instrument") {
          void editor.clearInstrument(node).then((cleared) => {
            if (!cleared) return;
            // (A lane that has nothing left but effects has no card, and its effects stand by themselves: the focus goes to the bar, where an instrument is added.)
            const graph = client.state.snapshot?.graph;
            focusWhenPossible(graph !== undefined && !holdersOf(graph).has(node) ? changeInstrumentId(node) : "add-instrument");
          });
          return;
        }
        void editor.removeNode(node).then((removed) => {
          // (The bar has no button that adds a group: the page offers no way to make one, so that the focus goes where it goes when a lane is taken out.)
          if (removed) focusWhenPossible("add-instrument");
        });
      }}
    >
      ×
    </MenuButton>
  );
}

/** The `id` of the button that turns the name of the group `node` into a field. */
const renameGroupId = (node: NodeId): string => `rename-group-${String(node)}`;

/** The `id` of the button that turns the name of the macro `node` into a field (step 110). */
const renameMacroId = (node: NodeId): string => `rename-macro-${String(node)}`;

/**
 * The name of a node that a person gives it, in a field, in place of the name in its header: Enter (or leaving the field) asks the server for it (`rename`), Escape leaves it as it
 * was, and a name that is the one it was asks nothing. The server takes the spaces off, and gives a name of nothing the name the node has by
 * default. `done` is called once, with whether the focus goes back to the button that opened the field (it does, for Enter and Escape: the
 * person is still there).
 */
function NameField({ name, rename, done }: { name: string; rename: (text: string) => Promise<boolean>; done: (focusBack: boolean) => void }) {
  const finished = useRef(false);
  const [text, setText] = useState(name);
  const finish = (ask: boolean, focusBack: boolean): void => {
    if (finished.current) return;
    finished.current = true;
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    if (ask && text !== name) void rename(text);
    done(focusBack);
  };
  return (
    <input
      className="tang-node__name-field nodrag nopan"
      type="text"
      aria-label={`Name of ${name}`}
      maxLength={NAME_LIMIT}
      value={text}
      autoFocus
      onFocus={(event) => {
        event.currentTarget.select();
      }}
      onChange={(event) => {
        setText(event.target.value);
      }}
      onKeyDown={(event) => {
        if (event.key !== "Enter" && event.key !== "Escape") return;
        // The key is taken here: the focus goes to the button at once, and a browser gives an Enter that was left alone to whatever has the
        // focus when it comes to act on it, which would press the button and open the field again.
        event.preventDefault();
        finish(event.key === "Enter", true);
      }}
      onBlur={() => {
        finish(true, false);
      }}
    />
  );
}

/** The name of a group, in a field in its header. */
function GroupNameField({ node, name, done }: { node: NodeId; name: string; done: (focusBack: boolean) => void }) {
  const editor = useEditor();
  const rename = useCallback((text: string) => editor.renameGroup(node, text), [editor, node]);
  return <NameField name={name} rename={rename} done={done} />;
}

/** The name of a macro, in a field in its header (step 110). */
function MacroNameField({ node, name, done }: { node: NodeId; name: string; done: (focusBack: boolean) => void }) {
  const editor = useEditor();
  const rename = useCallback((text: string) => editor.renameMacro(node, text), [editor, node]);
  return <NameField name={name} rename={rename} done={done} />;
}

/** The button of a node that turns its name into a field (`id` is its own, which the focus goes back to). */
function RenameButton({ id, name, onEdit }: { id: string; name: string; onEdit: () => void }) {
  return (
    <button type="button" id={id} className="tang-menu-button tang-node__rename nodrag nopan" aria-label={`Rename ${name}`} title={`Rename ${name}`} onClick={onEdit}>
      ✎
    </button>
  );
}

/**
 * The class of a node that cannot be heard at the master (`audibleAt`): a lane or a group that is muted, or not soloed while something is, and the effects in the chain of
 * one. It is dimmed, and still works.
 */
function useUnheard(node: NodeId): string {
  return useClientState((state) => audibleAt(state, node) === false) ? " tang-node--unheard" : "";
}

/** The `id` of the button in the bar that adds an effect that stands by itself, which the focus goes to when one is taken out (`app/AddFreeEffect.tsx`). */
export const ADD_FREE_EFFECT_ID = "add-free-effect";

/**
 * An instrument or an effect: its plugin's name and format, the parameters it shows, and its own gain; a lane's keys, group, mute and solo as well. An effect in a lane that only holds effects
 * (`data.heldIn`: the lane has no card, and an effect that stands by itself is one) stands for the lane too: the first of them has the cell of the lane's mute, solo and way out and the "+" that adds
 * a modulator to it, and its "×" takes the lane out with it when it is on its own there (the only effect, and no modulator in the lane).
 */
export function PluginNode({ data, type }: NodeProps<PatchNode>) {
  const effect = type === "effect";
  // (A lane that has no instrument has no sound to look at.)
  const silent = !effect && data.format === null;
  // (Where an effect is: the lane or group whose chain it is in, which the focus goes back to when it is taken out.)
  const parent = useClientState((state) => (effect ? (nodeById(state, data.node)?.parent ?? null) : null));
  const unheard = useUnheard(data.node);
  const held = data.heldIn;
  return (
    <div className={`tang-node tang-node--${type}${unheard} nokey`}>
      {/* (A lane of a module that is open has the tag that closes the module again: the effect that stands for a lane that has no card has it.) */}
      {data.module ? <ModuleTagButton tag={data.module} /> : null}
      {/* (An effect takes audio in, at two places if it has a second input, and a lane takes notes in, from the sources: but not a lane whose instrument takes no notes, an audio input.) */}
      {effect ? (
        data.side ? (
          <>
            <AudioPort node={data.node} direction="in" pair={{ side: false, label: data.side.main }} />
            <AudioPort node={data.node} direction="in" pair={{ side: true, label: data.side.side }} />
          </>
        ) : (
          <AudioPort node={data.node} direction="in" />
        )
      ) : data.notes ? (
        <NotesPort node={data.node} direction="in" />
      ) : null}
      <AudioPort node={data.node} direction="out" />
      <Header
        name={data.name}
        badge={data.format}
        sub={
          <>
            {silent ? null : <SendMenu node={data.node} name={data.name} />}
            {data.notes ? <NotesFromMenu node={data.node} name={data.name} /> : null}
          </>
        }
      >
        {effect ? (
          <span className="tang-node__actions">
            <MoveEffectButtons node={data.node} name={data.name} />
            <PresetMenu node={data.node} name={data.name} />
            <WindowButton node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            <ScopeButton node={data.node} name={data.name} />
            {held?.first ? <AddModulatorMenu node={held.lane} name={data.name} /> : null}
            <RemoveNodeButton node={held?.alone ? held.lane : data.node} name={data.name} focusTo={held ? ADD_FREE_EFFECT_ID : parent !== null ? addEffectId(parent) : null} />
          </span>
        ) : (
          <span className="tang-node__actions">
            <ChangeInstrumentMenu node={data.node} name={data.name} empty={data.format === null} />
            <PresetMenu node={data.node} name={data.name} />
            <WindowButton node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            {silent ? null : <ScopeButton node={data.node} name={data.name} />}
            <AddEffectMenu node={data.node} name={data.name} />
            <AddModulatorMenu node={data.node} name={data.name} />
            <RemoveLaneOrGroupMenu node={data.node} />
          </span>
        )}
      </Header>
      <div className="tang-node__controls nodrag nopan nowheel">
        {data.knobs.map((index) => (
          <ParamCell key={index} node={data.node} index={index} driven={data.modulated.includes(index)} />
        ))}
        <GainCell node={data.node} gain={effect ? "mix" : "volume"} />
        {data.notes ? <RangeCell node={data.node} name={data.name} /> : null}
        {/* (The choice of group is there only while the session has a group: the page offers no way to make one, and a session that was loaded with one has it drawn.) */}
        {data.groupCell ? <GroupCell node={data.node} name={data.name} /> : null}
        {/* (An effect that stands by itself has the cell of the lane that holds it, which has no card to have it on: the first of them does.) */}
        <MuteSoloCell node={held?.first ? held.lane : data.node} name={data.name} />
      </div>
      {/* (A node that is no lane has no pattern: the cell draws nothing for it. Nor has a lane that takes no notes, which has no keys to record.) */}
      {data.notes ? <PatternCell node={data.node} name={data.name} /> : null}
      {data.scope ? <NodeScope node={data.node} name={data.name} /> : null}
      {/* (A lane has a level, which an effect in it does not: the lane's is the end of its chain. A lane that has no way out has it too.) */}
      {effect ? null : <LevelStrip node={data.node} name={data.name} />}
    </div>
  );
}

/**
 * Whether two nodes are the same modulator as far as its node is drawn: of the same kind, and a
 * control bound to the same thing. Its settings are not part of it: each control reads its own,
 * so that turning one renders that control and not the node.
 */
export function sameModulatorShape(a: Node | undefined, b: Node | undefined): boolean {
  if (a === b) return true;
  if (a?.kind !== "modulator" || b?.kind !== "modulator" || a.source.type !== b.source.type) return false;
  return a.source.type !== "midiLearn" || JSON.stringify(a.source) === JSON.stringify(b.source);
}

/**
 * The shape an envelope's four settings make, with a dot where the envelope is while notes are played. It reads the settings itself, so that
 * turning one draws the shape again and nothing else of the node.
 */
function EnvelopeShape({ node, name, read }: { node: NodeId; name: string; read: () => number | undefined }) {
  const frames = useFrames();
  const stages = useClientSelection((state) => envelopeStagesOf(state, node), sameStages);
  if (!stages) return null;
  const words = `attack ${seconds(stages.attack)}, decay ${seconds(stages.decay)}, sustain ${formatValue(stages.sustain)}, release ${seconds(stages.release)}`;
  return <LiveEnvelope label={`${name} shape: ${words}`} stages={stages} read={read} frames={frames} />;
}

/**
 * The cables that start at a modulator, as a list at the foot of its node: a row for each, saying where it goes, with a port at the left edge of the
 * row where a cable from another modulator can end, to drive how deep this one goes, and the button that modulates the depth (`ModulateMenu`: a new modulator
 * for it, or one that is there). Nothing for a modulator that drives nothing.
 */
function CableRows({ node, name, cables }: { node: NodeId; name: string; cables: readonly CableRow[] }) {
  if (cables.length === 0) return null;
  return (
    <ul className="tang-node__cables" aria-label={`Cables of ${name}`}>
      {cables.map((row) => (
        <li key={row.edge} className={`tang-cable-row${row.driven ? " tang-cable-row--driven" : ""}`}>
          <DepthPort node={node} edge={row.edge} driven={row.driven} />
          <span className="tang-cable-row__label" title={row.label}>
            {row.label}
          </span>
          <ModulateMenu node={node} port={{ type: "depth", edge: row.edge }} name={`depth of ${name} → ${row.label}`} className="tang-modulate--cable" />
        </li>
      ))}
    </ul>
  );
}

/** A modulator: what it is, its settings as controls, and what it puts out, as a line that scrolls (an envelope has its shape too). */
export function ModulatorNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  // (A macro is called what a person calls it, in place: the field takes the place of its name, as a group's does.)
  const [editing, setEditing] = useState(false);
  const node = useClientSelection((state) => nodeById(state, data.node), sameModulatorShape);
  const read = useMemo(() => slotReader(client.telemetry, "modulation", data.node, { type: "modOut" }, "modOutput"), [client, data.node]);
  const source = node?.kind === "modulator" ? node.source : null;
  const scale = source ? outputScale(source) : { min: 0, max: 1, centred: false };
  // (An LFO is called what it is: its kind is not said twice.)
  const kind = source ? kindOf(source) : null;
  const settings = source ? settingsOf(source) : [];
  // (What takes it out gives the focus to the "+" that made it: its lane's or its group's, or the bar's for one that hears no lane and no group.)
  const owner = node?.parent ?? null;
  const heard = node?.kind === "modulator" ? node.hears : undefined;
  const addedBy = owner !== null ? addModulatorId(owner) : heard !== undefined ? addModulatorId(heard) : ADD_FREE_MODULATOR_ID;
  return (
    <div className="tang-node tang-node--modulator nokey">
      <Header
        name={data.name}
        badge={kind === data.name || data.name.startsWith(`${kind ?? ""} `) ? null : kind}
        // (A modulator that is in no lane and no group chooses whose notes it hears: one that is in a lane or a group hears its own.)
        sub={node?.kind === "modulator" ? <HearsMenu node={data.node} name={data.name} /> : null}
        field={
          editing ? (
            <MacroNameField
              node={data.node}
              name={data.name}
              done={(focusBack) => {
                setEditing(false);
                if (focusBack) focusWhenPossible(renameMacroId(data.node));
              }}
            />
          ) : undefined
        }
      >
        {/* (The note length that an LFO or a step sequencer follows the tempo by: no other kind has one, and the control is nothing for it.) */}
        <SyncControl node={data.node} />
        {/* (A macro is the one modulator that a person names: the others are called what they are.) */}
        {source?.type === "macro" ? (
          <RenameButton
            id={renameMacroId(data.node)}
            name={data.name}
            onEdit={() => {
              setEditing(true);
            }}
          />
        ) : null}
        <RemoveNodeButton node={data.node} name={data.name} focusTo={node ? addedBy : null} />
      </Header>
      {settings.length > 0 ? (
        <div className="tang-node__controls nodrag nopan nowheel">
          {settings.map((field) => (
            <ModulatorSettingCell key={field} node={data.node} field={field} driven={data.drivenSettings.some((setting) => setting === field)} />
          ))}
          {/* (Whether the box of a module that the macro is in shows its knob, beside the value that the knob turns.) */}
          {source?.type === "macro" ? (
            <div className="tang-cell">
              <ExposeControl node={data.node} />
            </div>
          ) : null}
        </div>
      ) : null}
      <div className="tang-node__body">
        {source?.type === "midiLearn" ? <p className="tang-node__detail">{controlDetail(source)}</p> : null}
        {source?.type === "envelope" ? <EnvelopeShape node={data.node} name={data.name} read={read} /> : null}
        {source?.type === "steps" ? <StepsControl node={data.node} name={data.name} /> : null}
        <LiveTrace label={`${data.name} output`} read={read} min={scale.min} max={scale.max} centred={scale.centred} frames={frames} />
        <div className="tang-node__drives nodrag nopan nowheel">
          <DrivePicker modulator={data.node} name={data.name} targets={data.targets} />
        </div>
        <CableRows node={data.node} name={data.name} cables={data.cables} />
      </div>
      <Handle type="source" position={Position.Bottom} id="modOut" className="tang-port tang-port--mod" />
    </div>
  );
}

/** A submix group: its volume and its pan, between its members and the master, and its mute and solo; its name is changed in place, and it can be taken out. */
export function GroupNode({ data }: NodeProps<PatchNode>) {
  const [editing, setEditing] = useState(false);
  const unheard = useUnheard(data.node);
  return (
    <div className={`tang-node tang-node--group${unheard} nokey`}>
      <AudioPort node={data.node} direction="in" />
      <AudioPort node={data.node} direction="out" />
      <Header
        name={data.name}
        badge="Group"
        sub={<SendMenu node={data.node} name={data.name} />}
        field={
          editing ? (
            <GroupNameField
              node={data.node}
              name={data.name}
              done={(focusBack) => {
                setEditing(false);
                if (focusBack) focusWhenPossible(renameGroupId(data.node));
              }}
            />
          ) : undefined
        }
      >
        <span className="tang-node__actions">
          <RenameButton
            id={renameGroupId(data.node)}
            name={data.name}
            onEdit={() => {
              setEditing(true);
            }}
          />
          <ScopeButton node={data.node} name={data.name} />
          <AddEffectMenu node={data.node} name={data.name} />
          <AddModulatorMenu node={data.node} name={data.name} />
          <RemoveLaneOrGroupMenu node={data.node} />
        </span>
      </Header>
      <div className="tang-node__controls nodrag nopan nowheel">
        <GainCell node={data.node} gain="groupVolume" />
        <GainCell node={data.node} gain="groupPan" />
        <MuteSoloCell node={data.node} name={data.name} />
      </div>
      {data.scope ? <NodeScope node={data.node} name={data.name} /> : null}
      <LevelStrip node={data.node} name={data.name} />
    </div>
  );
}

/**
 * The master: where the audio of the session ends, with its level and a scope of what comes out (a wave, or a spectrum), which the server is asked for while the master is here; its volume, the limiter
 * that holds the output under a ceiling (a button that puts it in the sound or takes it out, the ceiling, and how far it has brought the sound down: step 102).
 */
export function MasterNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  useEffect(() => client.watchTelemetry("scope"), [client]);
  const readPeak = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "peak"), [client, data.node]);
  const readClips = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "clips"), [client, data.node]);
  const readScope = useCallback(() => client.telemetry.values("scope"), [client]);
  const readScopeFrames = useCallback(() => client.telemetry.framesOf("scope"), [client]);
  return (
    <div className="tang-node tang-node--master nokey">
      <AudioPort node={data.node} direction="in" />
      <Header name={data.name} />
      <div className="tang-node__body">
        <Meter label="Master level" readPeak={readPeak} readClips={readClips} frames={frames} />
        <ScopePanel label="Output scope" read={readScope} readFrames={readScopeFrames} />
      </div>
      <div className="tang-node__controls nodrag nopan nowheel">
        <GainCell node={data.node} gain="masterVolume" />
        <GainCell node={data.node} gain="limiterCeiling" />
        <LimiterCell />
      </div>
      <ReductionMeter node={data.node} />
    </div>
  );
}

/**
 * A source of notes: the keys (the keys of the computer and the page's piano) or a device, which the session calls by the name of a MIDI input, or by as much of it as tells it. It says what it is and, for a
 * device, which of the inputs that are open it stands for, or that none is. Notes leave it at its output of events: to the lanes that no cable of notes ends at, which hear every source, and along the cables
 * that are drawn from it; its menu draws one to a lane, and its button takes it out, with the cables of notes it has.
 */
export function SourceNode({ data }: NodeProps<PatchNode>) {
  const inputs = useClientState((state) => state.inputs);
  const device = data.source?.type === "device" ? data.source : null;
  const stands = device ? inputsStoodFor(device.name, inputs) : [];
  const detail = device ? (stands.length > 0 ? `Plugged in: ${stands.join(", ")}` : "Not plugged in") : "Computer keys and the piano";
  return (
    <div className="tang-node tang-node--source nokey">
      <NotesPort node={data.node} direction="out" />
      <Header name={data.name} badge={device ? "MIDI input" : "Keyboard"} sub={<NotesToMenu node={data.node} name={data.name} />}>
        <span className="tang-node__actions">
          <RemoveNodeButton node={data.node} name={data.name} focusTo="add-source" />
        </span>
      </Header>
      <div className="tang-node__body">
        <p className="tang-node__detail tang-node__detail--source" title={stands.join(", ")}>
          {detail}
        </p>
      </div>
    </div>
  );
}
