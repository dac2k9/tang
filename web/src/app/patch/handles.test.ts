import { describe, expect, test } from "vitest";

import { depthHandle, handleOf, memberHandle, memberOfHandle, paramHandle, plainHandle } from "./handles";

describe("the names of the handles", () => {
  test("a port is named by its kind, and a parameter or the depth of a cable by its number too", () => {
    expect(handleOf({ type: "audioOut" })).toBe("audioOut");
    expect(handleOf({ type: "audioInSide" })).toBe("audioInSide");
    expect(handleOf({ type: "mix" })).toBe("mix");
    expect(handleOf({ type: "param", index: 7 })).toBe(paramHandle(7));
    expect(paramHandle(7)).toBe("param-7");
    expect(handleOf({ type: "depth", edge: 4 })).toBe(depthHandle(4));
    expect(depthHandle(4)).toBe("depth-4");
  });

  test("a handle of a box says the port and the node inside the module that has it", () => {
    expect(memberHandle("audioIn", 12)).toBe("audioIn@12");
    expect(memberHandle(paramHandle(3), 40)).toBe("param-3@40");
    expect(memberOfHandle("audioIn@12")).toEqual({ handle: "audioIn", member: 12 });
    expect(memberOfHandle("param-3@40")).toEqual({ handle: "param-3", member: 40 });
    expect(memberOfHandle("depth-5@9")).toEqual({ handle: "depth-5", member: 9 });
  });

  test("a handle of a node's own port is no box's: nothing to say, or an answer that is not a node, is none", () => {
    expect(memberOfHandle("audioIn")).toBeNull();
    expect(memberOfHandle("param-3")).toBeNull();
    expect(memberOfHandle("audioIn@")).toBeNull();
    expect(memberOfHandle("audioIn@x")).toBeNull();
    expect(memberOfHandle("audioIn@12x")).toBeNull();
    expect(memberOfHandle("@12")).toBeNull();
    expect(memberOfHandle(null)).toBeNull();
    expect(memberOfHandle(undefined)).toBeNull();
  });

  test("the plain handle is the port's name, whether the handle is a box's or a node's", () => {
    expect(plainHandle("audioOut@4")).toBe("audioOut");
    expect(plainHandle("audioOut")).toBe("audioOut");
    expect(plainHandle(null)).toBeNull();
    expect(plainHandle(undefined)).toBeNull();
  });
});
