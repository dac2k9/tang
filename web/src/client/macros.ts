// What a macro has besides its value (step 110, docs/web-ui.md D110; the user's "let a Macro have a name and an expose option/flip, that will expose the macro-knob when used in a module"): a name that a person gave it,
// and whether the box of a module that it is in shows its knob. The wire leaves both out while they are what a macro is to begin with (no name, and shown), so that a reader finds out here, in one place, what an absent one is.
import type { ModulatorSource } from "../protocol";

/** A source of a modulator that is a macro. */
export type MacroSource = Extract<ModulatorSource, { type: "macro" }>;

/** Whether a macro shows its knob on the box of a module that it is in: it does unless the wire says it does not (what is on, which a macro is to begin with, is left out). */
export const macroExposed = (source: MacroSource): boolean => source.expose !== false;

/** The name that a person gave a macro, or nothing for one that has none: it is called `Macro` then, which is the name of its node. */
export const macroName = (source: MacroSource): string => source.name ?? "";
