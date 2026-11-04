import { describe, expect, test } from "vitest";

import type { CatalogEntry } from "../protocol";
import { builtinCatalog } from "./fixtures";
import { effectItems, instrumentItems, MODULE_FORMAT, moduleItems } from "./catalog";

const entry = (id: string, name: string, format: string, isInstrument = false): CatalogEntry => ({ id, name, format, isInstrument, params: 1, presets: 0 });

describe("the plugins a person can choose an effect from", () => {
  test("are the ones that are not instruments, each by the id the server knows it by, under the format it is of", () => {
    expect(effectItems(builtinCatalog)).toEqual([
      { id: "builtin:delay", label: "Delay", group: "Built-in" },
      { id: "builtin:drive", label: "Drive", group: "Built-in" },
      { id: "builtin:filter", label: "Filter", group: "Built-in" },
      { id: "builtin:math", label: "Math", group: "Built-in" },
      { id: "builtin:reverb", label: "Reverb", group: "Built-in" },
    ]);
    expect(effectItems([])).toEqual([]);
    expect(effectItems([entry("builtin:osc", "Oscillator", "Built-in", true)])).toEqual([]);
  });

  test("are listed by format, the built-ins first and then CLAP, LV2 and VST3, in the order the server gave within each", () => {
    // (The server lists them by name, whatever the format.)
    const entries = [
      entry("vst3:Acme", "Acme Verb", "VST3"),
      entry("clap:com.acme.chorus", "Chorus", "CLAP"),
      entry("builtin:filter", "Filter", "Built-in"),
      entry("lv2:http://acme.example/delay", "Delay", "LV2"),
      entry("clap:com.acme.flanger", "Flanger", "CLAP"),
      entry("builtin:reverb", "Reverb", "Built-in"),
      entry("vst3:Zeta", "Zeta Comp", "VST3"),
    ];
    expect(effectItems(entries).map((item) => `${item.group ?? "?"}: ${item.label}`)).toEqual([
      "Built-in: Filter",
      "Built-in: Reverb",
      "CLAP: Chorus",
      "CLAP: Flanger",
      "LV2: Delay",
      "VST3: Acme Verb",
      "VST3: Zeta Comp",
    ]);
  });

  test("put a format that this page does not know of after those it does, together", () => {
    const entries = [entry("x:one", "One", "Other"), entry("builtin:filter", "Filter", "Built-in"), entry("x:two", "Two", "Other"), entry("vst3:A", "A", "VST3")];
    expect(effectItems(entries).map((item) => item.label)).toEqual(["Filter", "A", "One", "Two"]);
  });

  test("leave what was given alone", () => {
    const entries = [entry("vst3:A", "A", "VST3"), entry("builtin:filter", "Filter", "Built-in")];
    const copy = [...entries];
    effectItems(entries);
    expect(entries).toEqual(copy);
  });
});

describe("the modules a person saved", () => {
  const module = (id: string, name: string): CatalogEntry => entry(id, name, MODULE_FORMAT);
  const entries = [entry("clap:com.acme.chorus", "Chorus", "CLAP"), module("module:Room", "Room"), entry("builtin:filter", "Filter", "Built-in"), module("module:Big hall", "Big hall"), entry("builtin:osc", "Oscillator", "Built-in", true)];

  test("are no effect of a chain: the lane's and the bus's menus do not list them", () => {
    expect(effectItems(entries).map((item) => item.label)).toEqual(["Filter", "Chorus"]);
    expect(instrumentItems(entries).map((item) => item.label)).toEqual(["Oscillator"]);
  });

  test("are a category of their own (step 111): the modules, each by the id that the server knows it by, in the order that the server gave, and no plugin, with no heading to be listed under", () => {
    expect(moduleItems(entries)).toEqual([
      { id: "module:Room", label: "Room" },
      { id: "module:Big hall", label: "Big hall" },
    ]);
    // (The effects that stand by themselves are the plugins that can be an effect, and nothing else: the list of them has no module in it.)
    expect(effectItems(entries).some((item) => item.id.startsWith("module:"))).toBe(false);
  });

  test("are nothing when there are none: the plugins of a catalog are none of them, and an instrument that is called a module is not one", () => {
    expect(moduleItems(builtinCatalog)).toEqual([]);
    expect(moduleItems([entry("module:Odd", "Odd", MODULE_FORMAT, true)])).toEqual([]);
    expect(moduleItems([])).toEqual([]);
  });

  test("leave what was given alone", () => {
    const copy = [...entries];
    moduleItems(entries);
    expect(entries).toEqual(copy);
  });
});

describe("the plugins a person can choose an instrument from", () => {
  test("are the ones that are instruments, each by the id the server knows it by, under the format it is of", () => {
    // (The built-ins are the oscillator and the audio input, by name as the server gives them.)
    expect(instrumentItems(builtinCatalog)).toEqual([
      { id: "builtin:input", label: "Audio Input", group: "Built-in" },
      { id: "builtin:osc", label: "Oscillator", group: "Built-in" },
    ]);
    expect(instrumentItems([])).toEqual([]);
    expect(instrumentItems([entry("builtin:filter", "Filter", "Built-in")])).toEqual([]);
  });

  test("are listed by format, the built-ins first and then CLAP, LV2 and VST3, in the order the server gave within each; a format this page does not know of last", () => {
    const entries = [
      entry("vst3:Pianoteq", "Pianoteq 8", "VST3", true),
      entry("x:one", "One", "Other", true),
      entry("clap:com.acme.pad", "Pad", "CLAP", true),
      entry("builtin:osc", "Oscillator", "Built-in", true),
      entry("lv2:http://acme.example/helm", "Helm", "LV2", true),
      entry("clap:com.acme.fx", "Chorus", "CLAP"),
      entry("clap:com.acme.pluck", "Pluck", "CLAP", true),
    ];
    expect(instrumentItems(entries).map((item) => `${item.group ?? "?"}: ${item.label}`)).toEqual([
      "Built-in: Oscillator",
      "CLAP: Pad",
      "CLAP: Pluck",
      "LV2: Helm",
      "VST3: Pianoteq 8",
      "Other: One",
    ]);
  });

  test("and the ones an effect can be chosen from are the whole catalog between them, each in one list", () => {
    const entries = [
      entry("builtin:osc", "Oscillator", "Built-in", true),
      entry("builtin:filter", "Filter", "Built-in"),
      entry("vst3:Pianoteq", "Pianoteq 8", "VST3", true),
      entry("vst3:Verb", "Verb", "VST3"),
    ];
    const both = [...instrumentItems(entries), ...effectItems(entries)].map((item) => item.id).sort();
    expect(both).toEqual(entries.map((e) => e.id).sort());
  });

  test("leave what was given alone", () => {
    const entries = [entry("vst3:A", "A", "VST3", true), entry("builtin:osc", "Oscillator", "Built-in", true)];
    const copy = [...entries];
    instrumentItems(entries);
    expect(entries).toEqual(copy);
  });
});
