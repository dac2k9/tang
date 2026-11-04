// The controls of a node, bound to the session: each reads its own value from the client's
// store (so a value that changes renders that control and no other), shows what the user
// has set it to until the server has answered (`Drive`), and draws the modulation on it:
// the ring from the cables that end at it and the dot from the telemetry.
import { Handle, Position } from "@xyflow/react";
import { memo, useContext, useId, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { modulationExtent, modulationsAt, modulationsOn, settingExtent } from "../../client/modulation";
import type { Modulation } from "../../client/modulation";
import { gainName } from "../../client/names";
import { NOTE_NAMES } from "../../client/notes";
import { gainPorts } from "../../client/reach";
import type { Drivable, GainPort } from "../../client/reach";
import { slotReader } from "../../client/readers";
import { rateAt } from "../../client/tempo";
import { cableDepthOf, cableGainOf, cableSourceOf, chainLengthOf, flagsOf, gainOf, groupChoicesOf, isKnob, limiterOf, macroExposedOf, modulatorFieldOf, paramOf, paramUnusedBecause, rangeOf, sameFlags, sameGroupChoices, stepsOf } from "../../client/selectors";
import { CABLE_GAIN_RANGE, DEPTH_RANGE, GAIN_RANGES, MODULATOR_FIELD_DEFAULTS, MODULATOR_FIELD_RANGES, MODULATOR_FIELD_SCALES, SYNC_DIVISIONS, SYNC_FREE, WAVEFORMS } from "../../protocol";
import type { EdgeId, Gain, ModulatorField, NodeId, NoteRange, Param, Port } from "../../protocol";
import { Choice } from "../../ui/Choice";
import { formatValue, Knob } from "../../ui/Knob";
import { afterKey } from "../../ui/knob-geometry";
import { LiveBar } from "../../ui/LiveBar";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { StepBars } from "../../ui/StepBars";
import { useClient, useClientSelection, useClientState, useDrive, useEditor, useFrames } from "../ClientProvider";
import { ConnectingContext, keyOf } from "./connection";
import { seconds } from "./describe";
import type { SettingField } from "./describe";
import { ModulateMenu } from "./ModulateMenu";
import { SaveModuleButton } from "./SaveModule";
import { depthHandle, handleOf, paramHandle } from "./toFlow";

/** What of a parameter does not change as its value does: the same by this, it is drawn the same. */
export function sameParamFacts(a: Param | undefined, b: Param | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.index === b.index &&
    a.name === b.name &&
    a.min === b.min &&
    a.max === b.max &&
    a.default === b.default &&
    a.unit === b.unit &&
    a.labels === b.labels
  );
}

/**
 * Whether two modulators are the same as far as the ring of a knob goes: the same kind, and for a control the
 * one it is bound to. (An LFO's rate and waveform and an envelope's stages move the dot, not the ring, which is
 * how far the modulator can swing the value: turning them is not for the knobs they drive to draw again.)
 */
function sameKindOfSource(a: Modulation["source"], b: Modulation["source"]): boolean {
  if (a === b) return true;
  if (a.type === "midiLearn" && b.type === "midiLearn") return JSON.stringify(a.control) === JSON.stringify(b.control);
  return a.type === b.type;
}

/** Whether two lists of modulators are the same ones, of the same kinds, at the same depths. */
export function sameModulations(a: readonly Modulation[], b: readonly Modulation[]): boolean {
  return a.length === b.length && a.every((modulation, i) => modulation.amount === b[i]?.amount && sameKindOfSource(modulation.source, b[i].source));
}

/** A value with its unit, if the parameter has one. */
function formatterFor(unit: string | null): (value: number) => string {
  return unit ? (value) => `${formatValue(value)} ${unit}` : formatValue;
}

/**
 * What any control of a parameter of the plugin in `node` reads and does: what the parameter is (`facts`, which do not change
 * as its value does), what it shows (what the user set it to, until the engine has answered, and then the session's value),
 * the control that sets it, and the reader of what the engine makes of it (modulation included), for a view to draw on a frame.
 */
function useParam(node: NodeId, index: number) {
  const client = useClient();
  const control = useDrive().param(node, index);
  const facts = useClientSelection((state) => paramOf(state, node, index), sameParamFacts);
  const stored = useClientState((state) => paramOf(state, node, index)?.value);
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const readEffective = useMemo(() => slotReader(client.telemetry, "modulation", node, { type: "param", index }, "effective"), [client, node, index]);
  return { facts, shown: pending ?? stored, control, readEffective };
}

/**
 * What a control of a parameter that has no effect now says of itself (`why`: null if it has one): the stylesheet dims it, and the pointer says why. It can be turned all the same: the value is
 * kept for when the parameter has an effect.
 */
function unusedProps(why: string | null): { "data-unused"?: ""; title?: string } {
  return why === null ? {} : { "data-unused": "", title: `No effect now: ${why}` };
}

/** A parameter of the plugin in a node: a knob, or a choice if its values have names. */
export const ParamControl = memo(function ParamControl({ node, index }: { node: NodeId; index: number }) {
  const frames = useFrames();
  const { facts, shown, control, readEffective } = useParam(node, index);
  const modulations = useClientSelection((state) => (state.snapshot ? modulationsOn(state.snapshot.graph, node, index) : []), sameModulations);
  const format = useMemo(() => formatterFor(facts?.unit ?? null), [facts?.unit]);

  const ring = useMemo(
    () => (facts && shown !== undefined ? modulationExtent(shown, facts.min, facts.max, modulations) : undefined),
    [facts, shown, modulations],
  );
  if (!facts || shown === undefined) return null;

  if (facts.labels) {
    return (
      <Choice
        label={facts.name}
        labels={facts.labels}
        value={shown}
        onChange={(chosen) => {
          control.set(chosen);
          control.commit();
        }}
      />
    );
  }
  return (
    <Knob
      label={facts.name}
      value={shown}
      min={facts.min}
      max={facts.max}
      defaultValue={facts.default}
      format={format}
      modulation={ring}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      readEffective={readEffective}
      frames={frames}
    />
  );
});

/**
 * The pin of a row of the panel: pressed when the parameter is one of the knobs its node shows, and the way to make it one or not
 * (`Editor.pinParam`: a node that has nothing pinned shows the first eight, and pinning one more keeps them).
 */
function PinButton({ node, index, name }: { node: NodeId; index: number; name: string }) {
  const editor = useEditor();
  const pinned = useClientState((state) => isKnob(state, node, index));
  return (
    <button
      type="button"
      className="tang-row__pin"
      aria-pressed={pinned}
      aria-label={`Show ${name} on the node`}
      title="Show as a knob on the node"
      onClick={() => {
        void editor.pinParam(node, index, !pinned);
      }}
    >
      {pinned ? "●" : "○"}
    </button>
  );
}

/**
 * A parameter of the plugin in `node` as a row of a list: its whole name, a slider, its value; a choice if its values have
 * names; and, while a cable drives it, a bar of what the engine makes of it. For a list to look through (a plugin can have
 * thousands of parameters, and a knob has room for a few letters). It is turned like a knob: shown as set until the engine
 * has answered, one intent a frame, a gesture's end at once; the keys are a knob's (a hundredth of the range, a thousandth
 * with shift, a tenth on the page keys, the ends on Home and End), kept from the browser, whose own step on a slider
 * that has none is the whole range; a double click goes back to the default.
 */
export const ParamRow = memo(function ParamRow({ node, index }: { node: NodeId; index: number }) {
  const frames = useFrames();
  const id = useId();
  const { facts, shown, control, readEffective } = useParam(node, index);
  const driven = useClientState((state) => (state.snapshot ? modulationsOn(state.snapshot.graph, node, index).length > 0 : false));
  const unused = useClientState((state) => paramUnusedBecause(state, node, index));
  const format = useMemo(() => formatterFor(facts?.unit ?? null), [facts?.unit]);
  if (!facts || shown === undefined) return null;

  const name = (
    <>
      <label className="tang-row__name" htmlFor={id} title={facts.name}>
        {facts.name}
      </label>
      <span className="tang-row__tools">
        {facts.labels ? null : <ModulateMenu node={node} port={{ type: "param", index }} name={facts.name} className="tang-modulate--row" />}
        <PinButton node={node} index={index} name={facts.name} />
      </span>
    </>
  );
  if (facts.labels) {
    return (
      <div className="tang-row" {...unusedProps(unused)}>
        {name}
        <select
          id={id}
          className="tang-row__select"
          value={String(Math.round(shown))}
          onChange={(event) => {
            control.set(Number(event.target.value));
            control.commit();
          }}
        >
          {facts.labels.map((label, position) => (
            <option key={`${String(position)}:${label}`} value={String(position)}>
              {label}
            </option>
          ))}
        </select>
      </div>
    );
  }
  const { min, max } = facts;
  return (
    <div className="tang-row" {...unusedProps(unused)}>
      {name}
      <input
        id={id}
        type="range"
        className="tang-row__slider"
        min={min}
        max={max}
        step="any"
        value={shown}
        aria-valuetext={format(shown)}
        onChange={(event) => {
          control.set(event.target.valueAsNumber);
        }}
        onPointerUp={() => {
          control.commit();
        }}
        onBlur={() => {
          control.commit();
        }}
        onKeyDown={(event) => {
          const next = afterKey(event.key, event.shiftKey, shown, min, max);
          if (next === undefined) return;
          event.preventDefault();
          if (next === shown) return;
          control.set(next);
          control.commit();
        }}
        onDoubleClick={() => {
          if (facts.default === shown) return;
          control.set(facts.default);
          control.commit();
        }}
      />
      <span className="tang-row__value">{format(shown)}</span>
      {driven ? <LiveBar label={`${facts.name}, as modulated`} read={readEffective} min={min} max={max} format={format} frames={frames} /> : null}
    </div>
  );
});

/** A number of decibels below full scale as a person writes it: `-1.0 dB`. */
const decibelsBelowFull = (db: number): string => `${db.toFixed(1)} dB`;

/**
 * What a host-side gain is called, and where a double click puts it; how far it goes is the controller's (`GAIN_RANGES`). The master's have names of their own, so that the page has one
 * "Volume" for each lane and group and the master's is not another of them.
 */
const GAINS: Record<Gain, { label: string; caption?: string; reset: number; format?: (value: number) => string }> = {
  volume: { label: "Volume", reset: 1 },
  mix: { label: "Mix", reset: 1 },
  groupVolume: { label: "Volume", reset: 1 },
  groupPan: { label: "Pan", reset: 0 },
  masterVolume: { label: "Master volume", caption: "Volume", reset: 1 },
  limiterCeiling: { label: "Ceiling", reset: -1, format: decibelsBelowFull },
};

/**
 * The port that a cable from a modulator ends at to drive a host-side gain: the volume of a lane or a group, the mix of an effect, the pan of a group. The output mix's volume and the limiter's
 * ceiling have none: a person sets them, and no modulator drives them.
 */
const GAIN_PORTS: Partial<Record<Gain, GainPort>> = {
  volume: { type: "volume" },
  mix: { type: "mix" },
  groupVolume: { type: "volume" },
  groupPan: { type: "pan" },
};

/**
 * A host-side gain of a node: an instrument's volume, an effect's mix, a group's volume or pan. A modulator drives it as it drives a plugin's parameter (the controller's `gain_targets`): the knob
 * has the ring of how far the cables on it can swing it and the dot of what the engine makes of it, and its value is what the user set.
 */
export const GainControl = memo(function GainControl({ node, gain }: { node: NodeId; gain: Gain }) {
  const client = useClient();
  const frames = useFrames();
  const control = useDrive().gain(node, gain);
  const stored = useClientState((state) => gainOf(state, node, gain));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const port = GAIN_PORTS[gain];
  const modulations = useClientSelection((state) => (port && state.snapshot ? modulationsAt(state.snapshot.graph, node, port) : []), sameModulations);
  const readEffective = useMemo(() => (port ? slotReader(client.telemetry, "modulation", node, port, "effective") : undefined), [client, node, port]);
  const shown = pending ?? stored;
  const { label, caption, reset, format } = GAINS[gain];
  const [min, max] = GAIN_RANGES[gain];
  const ring = useMemo(() => (shown === undefined ? undefined : modulationExtent(shown, min, max, modulations)), [shown, min, max, modulations]);
  if (shown === undefined) return null;
  return (
    <Knob
      label={label}
      {...(caption ? { caption } : {})}
      value={shown}
      min={min}
      max={max}
      defaultValue={reset}
      {...(format ? { format } : {})}
      modulation={ring}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      {...(readEffective ? { readEffective } : {})}
      frames={frames}
    />
  );
});

/**
 * The keys a lane takes: where its notes begin and where they end, each a choice of `any` (no end) or a note, in a cell of the
 * node's grid. A change asks for the range it makes (`SetRange`): the other end stays as it is, an end that is chosen past the
 * other takes it along (one note is a range), and `any` at both is every note, which is no range. What was chosen shows at once
 * and until the answer is in; after it, what the session has, which is what was asked for unless it was refused (and the page
 * says why). An end of the keyboard is `any`: C-1 as the low end, and G9 as the high end, are not in the lists. A node that is no
 * lane has no keys, and nothing is drawn for it.
 */
export const RangeCell = memo(function RangeCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const stored = useClientState((state) => rangeOf(state, node));
  const [pending, setPending] = useState<NoteRange | null | undefined>(undefined);
  const latest = useRef(0);
  if (stored === undefined) return null;
  const range = pending === undefined ? stored : pending;
  const low = range?.low ?? 0;
  const high = range?.high ?? 127;
  const choose = (next: NoteRange): void => {
    // (The whole keyboard is no range.)
    const asked = next.low === 0 && next.high === 127 ? null : next;
    const mine = (latest.current += 1);
    setPending(asked);
    void editor.setRange(node, asked).finally(() => {
      if (latest.current === mine) setPending(undefined);
    });
  };
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Keys</span>
        <select
          className="tang-keys__select"
          aria-label={`Lowest key of ${name}`}
          title="Lowest key"
          value={low === 0 ? "" : String(low)}
          onChange={(event) => {
            const note = event.target.value === "" ? 0 : Number(event.target.value);
            choose({ low: note, high: Math.max(high, note) });
          }}
        >
          <option value="">any</option>
          {NOTE_NAMES.slice(1).map((label, place) => (
            <option key={label} value={String(place + 1)}>
              {label}
            </option>
          ))}
        </select>
        <select
          className="tang-keys__select"
          aria-label={`Highest key of ${name}`}
          title="Highest key"
          value={high === 127 ? "" : String(high)}
          onChange={(event) => {
            const note = event.target.value === "" ? 127 : Number(event.target.value);
            choose({ low: Math.min(low, note), high: note });
          }}
        >
          <option value="">any</option>
          {NOTE_NAMES.slice(0, 127).map((label, place) => (
            <option key={label} value={String(place)}>
              {label}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
});

/**
 * The group a lane is in, as a choice in a cell of its node: none, or a group of the session by the name it has. The page offers no way to make a group (step 103: a lane is put in one that the session
 * has, a session that was loaded with one or that the terminal made one in, and the node draws the cell only then: `PatchNodeData.groupCell`). A group it leaves lets go of it (the cables of the modulators that
 * hear the group stay: a cable names its plugin). What is chosen is shown until the server has answered; if it was refused (the page says why) the group the session has comes back. A node that is no lane has no
 * group, and nothing is drawn for it.
 */
export const GroupCell = memo(function GroupCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const stored = useClientSelection((state) => groupChoicesOf(state, node), sameGroupChoices);
  const [pending, setPending] = useState<string | undefined>(undefined);
  const latest = useRef(0);
  if (stored === undefined) return null;
  const value = pending ?? (stored.current === null ? "" : String(stored.current));
  // (The choice is narrow and cuts a long name: the tooltip has all of it.)
  const inGroup = stored.groups.find((group) => String(group.id) === value)?.name ?? "no group";
  const choose = (next: string): void => {
    const mine = (latest.current += 1);
    setPending(next);
    void editor.setLaneGroup(node, next === "" ? null : Number(next)).finally(() => {
      if (latest.current === mine) setPending(undefined);
    });
  };
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Group</span>
        <select
          className="tang-keys__select"
          aria-label={`Group of ${name}`}
          title={`${name} is in ${inGroup}`}
          value={value}
          onChange={(event) => {
            choose(event.target.value);
          }}
        >
          <option value="">none</option>
          {stored.groups.map((group) => (
            <option key={group.id} value={String(group.id)}>
              {group.name}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
});

/**
 * The mute and the solo of a lane or a group, two buttons in a cell of its node (the controller's `SetMute` and `SetSolo`): a muted node is never heard, and while anything is soloed only what is
 * soloed is (`audibleOf` says what that makes of every node). A press asks for the other way and the buttons show what the session has: they are not changed before the answer is in, and a
 * refusal is said among the notices. A node that is neither a lane nor a group has none, and nothing is drawn for it. (Where a node's audio goes is not a setting of the node: it is its cables, which
 * include the links that a lane and a group make by themselves, and a node that has none sends nowhere.)
 */
export const MuteSoloCell = memo(function MuteSoloCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const flags = useClientSelection((state) => flagsOf(state, node), sameFlags);
  // (A lane that has a chain of effects can save it as a module; a group's bus and a lane that has no effect cannot.)
  const chain = useClientSelection((state) => chainLengthOf(state, node), Object.is);
  if (flags === undefined) return null;
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Mute / Solo</span>
        <span className="tang-flags">
          <button
            type="button"
            className="tang-flag tang-flag--mute"
            aria-label={`Mute ${name}`}
            aria-pressed={flags.muted}
            title={`${flags.muted ? "Unmute" : "Mute"} ${name}`}
            onClick={() => {
              void editor.setMute(node, !flags.muted);
            }}
          >
            M
          </button>
          <button
            type="button"
            className="tang-flag tang-flag--solo"
            aria-label={`Solo ${name}`}
            aria-pressed={flags.solo}
            title={`${flags.solo ? "Unsolo" : "Solo"} ${name}`}
            onClick={() => {
              void editor.setSolo(node, !flags.solo);
            }}
          >
            S
          </button>
          {chain > 0 ? <SaveModuleButton node={node} name={name} /> : null}
        </span>
      </div>
    </div>
  );
});

/** The settings of a modulator that a cable can end at (`SessionModel::field_targets`): an LFO's and a step sequencer's rate, the stages of an envelope. */
type PortedField = Extract<ModulatorField, "rate" | "attack" | "decay" | "sustain" | "release">;

/**
 * What a number of a modulator is called and written like. How far each goes, what a double
 * click puts it back to and how its knob is spread (a rate and the times of an envelope run over
 * decades, a level does not) are the controller's (`MODULATOR_FIELD_RANGES`, `_DEFAULTS`, `_SCALES`).
 */
const NUMBERS: Record<PortedField, { label: string; format: (value: number) => string }> = {
  rate: { label: "Rate", format: (value) => `${formatValue(value)} Hz` },
  attack: { label: "Attack", format: seconds },
  decay: { label: "Decay", format: seconds },
  sustain: { label: "Sustain", format: formatValue },
  release: { label: "Release", format: seconds },
};

/**
 * A number of a modulator: an LFO's rate, a stage of an envelope. A modulator can drive it as it drives a parameter (a cable to the
 * rate, to a stage), and it shows that as a parameter's knob does: the ring from where the cable can take it (the last cable's alone:
 * the engine writes a setting once for each cable and the last stands) and the dot where the engine says it is.
 *
 * The rate of a modulator that follows the tempo (its `sync` is a note length) is not what it goes by: the knob shows the rate that the tempo makes of the note length
 * (`rateAt`, as the engine's), moves as the tempo does, and cannot be turned, until the modulator is free again and it is the rate that it kept. No cable
 * drives it then (the engine goes by the tempo), so it has no ring and no dot.
 */
export const ModulatorFieldControl = memo(function ModulatorFieldControl({ node, field }: { node: NodeId; field: PortedField }) {
  const client = useClient();
  const frames = useFrames();
  const control = useDrive().modulatorField(node, field);
  const stored = useClientState((state) => modulatorFieldOf(state, node, field));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const sync = useClientState((state) => (field === "rate" ? modulatorFieldOf(state, node, "sync") : undefined));
  const bpm = useClientState((state) => state.snapshot?.meta.bpm);
  const modulations = useClientSelection((state) => (state.snapshot ? modulationsAt(state.snapshot.graph, node, { type: field }) : []), sameModulations);
  const readEffective = useMemo(() => slotReader(client.telemetry, "modulation", node, { type: field }, "effective"), [client, node, field]);
  const shown = pending ?? stored;
  const [min, max] = MODULATOR_FIELD_RANGES[field];
  const scale = MODULATOR_FIELD_SCALES[field];
  const ring = useMemo(() => (shown !== undefined ? settingExtent(shown, min, max, modulations, scale) : undefined), [shown, min, max, modulations, scale]);
  if (shown === undefined) return null;
  const { label, format } = NUMBERS[field];
  const following = sync !== undefined && sync > 0 && bpm !== undefined;
  return (
    <Knob
      label={label}
      value={following ? rateAt(shown, sync, bpm) : shown}
      min={min}
      max={max}
      scale={scale}
      defaultValue={MODULATOR_FIELD_DEFAULTS[field]}
      format={format}
      {...(following ? {} : { modulation: ring, readEffective })}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      disabled={following}
      frames={frames}
    />
  );
});

/** What the numbers of a modulator that no cable drives are called and written like: a macro's value, a step sequencer's glide and length (a whole number of steps), an LFO's fade. */
const VALUES: Record<"value" | "glide" | "length" | "fade", { label: string; format: (value: number) => string }> = {
  value: { label: "Value", format: formatValue },
  glide: { label: "Glide", format: formatValue },
  length: { label: "Length", format: (value) => String(Math.round(value)) },
  fade: { label: "Fade", format: (value) => (value > 0 ? seconds(value) : "none") },
};

/**
 * A number of a modulator that a person turns and nothing else moves, so that it has no place for a cable on its edge and no ring and no dot (what it drives shows
 * them): a macro's value, a step sequencer's glide and length, an LFO's fade. The numbers that a modulator can drive (`ModulatorFieldControl`) can be driven by a cable.
 */
export const ModulatorValueControl = memo(function ModulatorValueControl({ node, field, label: named }: { node: NodeId; field: "value" | "glide" | "length" | "fade"; label?: string }) {
  const frames = useFrames();
  const control = useDrive().modulatorField(node, field);
  const stored = useClientState((state) => modulatorFieldOf(state, node, field));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const [min, max] = MODULATOR_FIELD_RANGES[field];
  const { label, format } = VALUES[field];
  return (
    <Knob
      label={named ?? label}
      value={shown}
      min={min}
      max={max}
      scale={MODULATOR_FIELD_SCALES[field]}
      defaultValue={MODULATOR_FIELD_DEFAULTS[field]}
      format={format}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      frames={frames}
    />
  );
});

/**
 * The value of a macro, as the knob of its own node (`Value`) or, with a `label`, as the knob that the box of the module it is in shows for it (step 110): the name that the macro is called by, which is what the knob
 * does. It is the same knob either way (it reads and sets the macro's value by its node, wherever the macro is drawn or not).
 */
export const MacroValueControl = memo(function MacroValueControl({ node, label }: { node: NodeId; label?: string }) {
  return label === undefined ? <ModulatorValueControl node={node} field="value" /> : <ModulatorValueControl node={node} field="value" label={label} />;
});

/**
 * Whether a macro shows its knob on the box of a module that it is in (step 110): a choice of off and on, in a cell of the macro's own node beside its value, on to begin with. A macro that is in no module has no box to show it
 * on, and the choice is there all the same (it is what the macro will do when it is put in one). It is not a number of the modulator (`SetModulatorField`) but its own intent (`ExposeMacro`), whose answer is the state that this reads.
 */
export const ExposeControl = memo(function ExposeControl({ node }: { node: NodeId }) {
  const editor = useEditor();
  const exposed = useClientState((state) => macroExposedOf(state, node));
  if (exposed === undefined) return null;
  return (
    <Choice
      label="Expose"
      title="Show the knob of this macro on the box of the module that it is in"
      labels={["off", "on"]}
      value={exposed ? 1 : 0}
      onChange={(chosen) => {
        void editor.exposeMacro(node, chosen === 1);
      }}
    />
  );
});

/** Whether a note starts a step sequencer again at its first step, or an LFO again at the start of its wave: a choice of off and on. */
export const RetriggerControl = memo(function RetriggerControl({ node }: { node: NodeId }) {
  const control = useDrive().modulatorField(node, "retrigger");
  const stored = useClientState((state) => modulatorFieldOf(state, node, "retrigger"));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  return (
    <Choice
      label="Retrigger"
      labels={["off", "on"]}
      value={shown}
      onChange={(chosen) => {
        control.set(chosen);
        control.commit();
      }}
    />
  );
});

/** What the places of a modulator's `sync` are called: none (`free`), and then each note length (`SYNC_DIVISIONS`), as the screens and a session file name them. */
const SYNC_NAMES: readonly string[] = [SYNC_FREE, ...SYNC_DIVISIONS.map((division) => division.name)];

/**
 * The note length that an LFO or a step sequencer follows the tempo by, in place of its rate: free, or one of the note lengths, chosen. It is in the header of the node, in
 * a line with the name and the button that takes the node out: a cell of the grid has room for four controls in a row (the layout counts one row) and for names of a few
 * letters (`1/16 triplet` is cut in one). Nothing for a node that has no rate to follow the tempo by.
 */
export const SyncControl = memo(function SyncControl({ node }: { node: NodeId }) {
  const control = useDrive().modulatorField(node, "sync");
  const stored = useClientState((state) => modulatorFieldOf(state, node, "sync"));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  return (
    <span className="tang-node__sync nodrag nopan nowheel">
      <Choice
        inline
        label="Sync"
        title="Follow the tempo: how long a cycle is (a step, for a step sequencer), as a note length, in place of the rate"
        labels={SYNC_NAMES}
        value={shown}
        onChange={(chosen) => {
          control.set(chosen);
          control.commit();
        }}
      />
    </span>
  );
});

/**
 * The bars of a step sequencer: its steps as bars that are painted with a drag and moved with the keys, the ones past its length dimmed, and a playhead on the step it is at
 * (the position that the engine publishes). A stroke is sent as the steps, all of them, a frame at a time (`Drive.steps`), and shows what it has painted until the server has answered.
 * Nothing for a node that is no step sequencer.
 */
export const StepsControl = memo(function StepsControl({ node, name }: { node: NodeId; name: string }) {
  const client = useClient();
  const frames = useFrames();
  const control = useDrive().steps(node);
  const stored = useClientState((state) => stepsOf(state, node));
  const length = useClientState((state) => modulatorFieldOf(state, node, "length"));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const read = useMemo(() => slotReader(client.telemetry, "modulation", node, { type: "modOut" }, "modPosition"), [client, node]);
  const shown = pending ?? stored;
  if (shown === undefined || length === undefined) return null;
  return (
    <StepBars
      label={`Steps of ${name}`}
      values={shown}
      length={length}
      onChange={(values) => {
        control.set(values);
      }}
      onCommit={() => {
        control.commit();
      }}
      read={read}
      frames={frames}
    />
  );
});

/** The waveform of an LFO: one of the waveforms (`WAVEFORMS`), chosen. */
export const WaveformControl = memo(function WaveformControl({ node }: { node: NodeId }) {
  const control = useDrive().modulatorField(node, "waveform");
  const stored = useClientState((state) => modulatorFieldOf(state, node, "waveform"));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  return (
    <Choice
      label="Wave"
      labels={WAVEFORMS}
      value={shown}
      onChange={(chosen) => {
        control.set(chosen);
        control.commit();
      }}
    />
  );
});

/**
 * The place on the top edge of a control where a cable from a modulator ends, drawn by dragging one to it (a port does
 * not start a cable): a handle of the node named `handle` (`handleOf`), quiet until a cable ends at it (`driven`), and,
 * while a cable is dragged, saying whether it takes that one.
 */
function CablePort({ node, handle, driven }: { node: NodeId; handle: string; driven: boolean }) {
  const takes = useContext(ConnectingContext)?.has(keyOf(node, handle)) ?? false;
  const classes = ["tang-port", "tang-port--param", ...(driven ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return <Handle isConnectableStart={false} type="target" position={Position.Top} id={handle} className={classes.join(" ")} />;
}

/**
 * The place on the left edge of a row of a modulator's node where a cable from another modulator ends, to drive how deep the cable of that row
 * goes (`depth` of the cable `edge`, which starts at the modulator `node`): a handle named after the cable (`depthHandle`), quiet until a cable ends
 * at it (`driven`), and, while a cable is dragged, saying whether it takes that one (a cable ends there and is not started there, as at a control's).
 */
export function DepthPort({ node, edge, driven }: { node: NodeId; edge: EdgeId; driven: boolean }) {
  const handle = depthHandle(edge);
  const takes = useContext(ConnectingContext)?.has(keyOf(node, handle)) ?? false;
  const classes = ["tang-port", "tang-port--cable", ...(driven ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return <Handle isConnectableStart={false} type="target" position={Position.Left} id={handle} className={classes.join(" ")} />;
}

/**
 * One of the two inputs of an effect that has a second one (`SideInfo`): the first, which the chain feeds as well, or the second (`side`), which only cables end at, with the name its plugin calls it
 * by (`A`, `B`), written beside the port.
 */
export interface InputPair {
  side: boolean;
  label: string;
}

/**
 * The place on the edge of a node where audio leaves it (`out`, on the right: a cable is started there and none ends there) or comes in (`in`, on the left: a cable ends there and
 * none is started there): a handle of the node named `audioOut` or `audioIn`, and, while a cable is dragged, an input saying whether it takes that one (an output never does: a
 * cable that is dragged from one ends at an input). An effect that has a second input has two, one above the other (`pair`): the first is `audioIn` still, and the second `audioInSide`,
 * each with its name beside it, and while a cable is dragged each takes half of the node's edge.
 */
export function AudioPort({ node, direction, pair }: { node: NodeId; direction: "in" | "out"; pair?: InputPair }) {
  const input = direction === "in";
  const handle = !input ? "audioOut" : pair?.side ? "audioInSide" : "audioIn";
  const takes = useContext(ConnectingContext)?.has(keyOf(node, handle)) ?? false;
  const classes = [
    "tang-port",
    "tang-port--audio",
    input ? "tang-port--in" : "tang-port--out",
    ...(input && pair ? [pair.side ? "tang-port--pair-side" : "tang-port--pair-main"] : []),
    ...(input && takes ? ["tang-port--takes"] : []),
  ];
  return (
    <>
      <Handle
        isConnectableStart={!input}
        isConnectableEnd={input}
        type={input ? "target" : "source"}
        position={input ? Position.Left : Position.Right}
        id={handle}
        className={classes.join(" ")}
      />
      {input && pair ? (
        <span className={`tang-port-label ${pair.side ? "tang-port-label--side" : "tang-port-label--main"}`} aria-hidden="true">
          {pair.label}
        </span>
      ) : null}
    </>
  );
}

/**
 * The place on the edge of a node where notes leave it (`out`, on the right of a source: a cable of notes is started there and none ends there) or come in (`in`, on the left of a lane: a cable of notes
 * ends there and none is started there): a handle of the node named `eventsOut` or `eventsIn`, and, while a cable is dragged, an input saying whether it takes that one (an output never does: a cable
 * that is dragged from one ends at an input).
 */
export function NotesPort({ node, direction }: { node: NodeId; direction: "in" | "out" }) {
  const input = direction === "in";
  const takes = useContext(ConnectingContext)?.has(keyOf(node, "eventsIn")) ?? false;
  const classes = ["tang-port", "tang-port--notes", input ? "tang-port--in" : "tang-port--out", ...(input && takes ? ["tang-port--takes"] : [])];
  return (
    <Handle
      isConnectableStart={!input}
      isConnectableEnd={input}
      type={input ? "target" : "source"}
      position={input ? Position.Left : Position.Right}
      id={input ? "eventsIn" : "eventsOut"}
      className={classes.join(" ")}
    />
  );
}

/**
 * A parameter in a node with a place on its top edge where a cable from a modulator ends. (That the control is
 * there when a cable ends at it is for `toFlow` to see to: a parameter that is driven is always among those a node shows.)
 */
export const ParamCell = memo(function ParamCell({ node, index, driven }: { node: NodeId; index: number; driven: boolean }) {
  const unused = useClientState((state) => paramUnusedBecause(state, node, index));
  return (
    <div className="tang-cell" {...unusedProps(unused)}>
      <CablePort node={node} handle={paramHandle(index)} driven={driven} />
      <ParamControl node={node} index={index} />
      <ParamModulateButton node={node} index={index} />
    </div>
  );
});

/**
 * The button on the knob of a parameter that modulates it (`ModulateMenu`). A parameter whose values have names is a choice and has none: a cable can be dragged to
 * its port, and a modulator that steps through names is not what the button is for (as the TUI's `m`, which leaves such a row out).
 */
function ParamModulateButton({ node, index }: { node: NodeId; index: number }) {
  const facts = useClientSelection((state) => paramOf(state, node, index), sameParamFacts);
  if (!facts || facts.labels) return null;
  return <ModulateMenu node={node} port={{ type: "param", index }} name={facts.name} className="tang-modulate--cell" />;
}

/**
 * A host-side gain in a cell of the node's grid, with a place on its top edge where a cable from a modulator ends, and the `~` button that modulates it, as a parameter has: a lane's volume (a lane that has an
 * instrument), an effect's mix, a group's volume and its pan. (A lane that has no instrument has a volume that no cable can drive: nothing is drawn for one.)
 */
export const GainCell = memo(function GainCell({ node, gain }: { node: NodeId; gain: Gain }) {
  const port = GAIN_PORTS[gain];
  const drivable = useClientState((state) => (port ? (state.snapshot?.graph.nodes.some((n) => n.id === node && gainPorts(n).some((own) => own.type === port.type)) ?? false) : false));
  const driven = useClientState((state) => (port && state.snapshot ? modulationsAt(state.snapshot.graph, node, port).length > 0 : false));
  const name = useClientState((state) => (port ? gainName(state.snapshot?.graph.nodes.find((n) => n.id === node), port) : ""));
  return (
    <div className="tang-cell">
      {port && drivable ? <CablePort node={node} handle={handleOf(port)} driven={driven} /> : null}
      <GainControl node={node} gain={gain} />
      {port && drivable ? <ModulateMenu node={node} port={port} name={name} className="tang-modulate--cell" /> : null}
    </div>
  );
});

/**
 * The limiter on the output mix, as a button in a cell of the master's grid (step 102): pressed while the limiter is in the sound, which holds the mix under its ceiling and does not let it clip.
 * A press puts it in or takes it out (`SetLimiter`); it shows what the session has, which is what was asked for unless the controller refused.
 */
export const LimiterCell = memo(function LimiterCell() {
  const editor = useEditor();
  const on = useClientState(limiterOf);
  if (on === undefined) return null;
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Limiter</span>
        <button
          type="button"
          className="tang-flag tang-flag--limiter"
          aria-label="Limiter"
          aria-pressed={on}
          title={on ? "The limiter holds the output under its ceiling: press to take it out of the sound" : "The limiter is out of the sound, and the output can clip: press to put it in"}
          onClick={() => {
            void editor.setLimiter(!on);
          }}
        >
          {on ? "On" : "Off"}
        </button>
      </div>
    </div>
  );
});

/**
 * A setting of a modulator, in a cell of the node's grid: a choice for the waveform, which no cable ends at, and for the rest a
 * knob with a place on its top edge where a cable from another modulator ends (`driven`: one does).
 */
export const ModulatorSettingCell = memo(function ModulatorSettingCell({ node, field, driven }: { node: NodeId; field: SettingField; driven: boolean }) {
  if (field === "waveform") {
    return (
      <div className="tang-cell">
        <WaveformControl node={node} />
      </div>
    );
  }
  // (A macro's value is turned by a person: no cable ends at it, and no modulator is made for it. Nor at a step sequencer's glide and length, or an LFO's fade.)
  if (field === "value" || field === "glide" || field === "length" || field === "fade") {
    return (
      <div className="tang-cell">
        <ModulatorValueControl node={node} field={field} />
      </div>
    );
  }
  if (field === "retrigger") {
    return (
      <div className="tang-cell">
        <RetriggerControl node={node} />
      </div>
    );
  }
  return (
    <div className="tang-cell">
      <CablePort node={node} handle={field} driven={driven} />
      <ModulatorFieldControl node={node} field={field} />
      <ModulateMenu node={node} port={{ type: field }} name={field} className="tang-modulate--cell" />
    </div>
  );
});

/** A depth as a person says it: a percentage, which goes below nothing when the cable pulls the other way. */
export const percent = (depth: number): string => `${String(Math.round(depth * 100))}%`;

/**
 * How deep a modulation cable pulls, as a slider on the cable (a depth is read in a glance
 * and set in a stroke: a knob is more than the cable has room for). `label` says what the
 * cable joins. It is turned like any control: what the user has set is shown until the engine
 * has answered, at most one intent goes in a frame, and one that ends a gesture goes at once.
 * Another cable can drive the depth (an envelope that fades a vibrato in): the slider is still
 * what the user set, and a bar under it, from the middle, says what the engine makes of it.
 */
export const CableDepthControl = memo(function CableDepthControl({ edge, label }: { edge: EdgeId; label: string }) {
  const client = useClient();
  const frames = useFrames();
  const control = useDrive().cableDepth(edge);
  const stored = useClientState((state) => cableDepthOf(state, edge));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  // The depth is a setting of the modulator the cable starts at: that is the node its value is named at, and a cable that ends there drives it.
  const owner = useClientState((state) => cableSourceOf(state, edge));
  const driven = useClientState((state) => (state.snapshot && owner !== undefined ? modulationsAt(state.snapshot.graph, owner, { type: "depth", edge }).length > 0 : false));
  const readEffective = useMemo(() => (owner !== undefined ? slotReader(client.telemetry, "modulation", owner, { type: "depth", edge }, "effective") : undefined), [client, owner, edge]);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const [min, max] = DEPTH_RANGE;
  const commit = (): void => {
    control.commit();
  };
  return (
    <label className="tang-depth">
      <span className="tang-depth__body">
        <input
          type="range"
          className="tang-depth__slider"
          aria-label={`Depth of ${label}`}
          aria-valuetext={percent(shown)}
          min={min}
          max={max}
          step={0.01}
          value={shown}
          onChange={(event) => {
            control.set(event.target.valueAsNumber);
          }}
          onPointerUp={commit}
          onKeyUp={commit}
          onBlur={commit}
        />
        {driven && readEffective ? <LiveBar label={`Depth of ${label}, as modulated`} read={readEffective} min={min} max={max} centred format={percent} frames={frames} /> : null}
      </span>
      {/* (Not an `<output>`, which is a status to a screen reader: the slider has said it.) */}
      <span className="tang-depth__value" aria-hidden="true">
        {percent(shown)}
      </span>
    </label>
  );
});

/**
 * How loud an audio cable is, as a slider on the cable, as the depth of a cable of modulation is (`CableDepthControl`): the level the user has set, from nothing to twice, which is
 * shown until the engine has answered, at most one intent in a frame, and one that ends a gesture goes at once. `label` says what the cable joins.
 */
export const CableLevelControl = memo(function CableLevelControl({ edge, label }: { edge: EdgeId; label: string }) {
  const control = useDrive().cableGain(edge);
  const stored = useClientState((state) => cableGainOf(state, edge));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const [min, max] = CABLE_GAIN_RANGE;
  const commit = (): void => {
    control.commit();
  };
  return (
    <label className="tang-depth tang-level">
      <input
        type="range"
        className="tang-depth__slider tang-level__slider"
        aria-label={`Level of ${label}`}
        aria-valuetext={percent(shown)}
        min={min}
        max={max}
        step={0.01}
        value={shown}
        onChange={(event) => {
          control.set(event.target.valueAsNumber);
        }}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      {/* (Not an `<output>`, which is a status to a screen reader: the slider has said it.) */}
      <span className="tang-depth__value" aria-hidden="true">
        {percent(shown)}
      </span>
    </label>
  );
});

/** The `id` of the button of the list that draws a cable from the modulator `node`, which what takes a cable out gives the focus to. */
export const drivesId = (node: NodeId): string => `drives-${String(node)}`;

/**
 * The list that draws a cable from a modulator to any parameter it can still drive, plugin by plugin, and to any setting
 * of another modulator, modulator by modulator: for a keyboard (which cannot drag a cable), and for the parameters a node
 * does not show as controls. It is a menu and not a native list: the arrow keys of a closed one change its value on some
 * platforms, which here is a cable on each key. Choosing a parameter or a setting is the cable; the menu closes and the
 * focus is on its button.
 */
export const DrivePicker = memo(function DrivePicker({ modulator, name, targets }: { modulator: NodeId; name: string; targets: readonly Drivable[] }) {
  const editor = useEditor();
  // What each item is the cable to: a node and a port of it, by the key of the port (`keyOf`).
  const { items, ends } = useMemo(() => {
    const items: MenuItem[] = [];
    const ends = new Map<string, { node: NodeId; port: Port }>();
    const add = (node: NodeId, port: Port, label: string, group: string): void => {
      const id = keyOf(node, handleOf(port));
      items.push({ id, label, group });
      ends.set(id, { node, port });
    };
    for (const group of targets) {
      for (const param of group.params) add(group.node, { type: "param", index: param.index }, param.name, group.name);
      for (const field of group.fields) add(group.node, field.port, field.name, group.name);
    }
    return { items, ends };
  }, [targets]);
  return (
    <MenuButton
      id={drivesId(modulator)}
      className="tang-drives"
      label={`Add a cable from ${name}`}
      items={items}
      onChoose={(choice) => {
        const end = ends.get(choice);
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (end) void editor.addCable(modulator, end.node, end.port);
      }}
    >
      {items.length === 0 ? "Nothing to drive" : "Add a cable…"}
    </MenuButton>
  );
});
