import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./global.css", import.meta.url), "utf8");

/** The declarations of every rule whose selector list is exactly `selectors` (the list as written, whitespace and comments apart). */
function declarationsOf(selectors: string): string[] {
  const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter((match) => (match[1] ?? "").replace(/\/\*[\s\S]*?\*\//g, "").split(",").map((part) => part.trim()).join(", ") === selectors);
  expect(rules.length, `a rule for ${selectors}`).toBeGreaterThan(0);
  return rules.flatMap((rule) => (rule[2] ?? "").split(";").map((declaration) => declaration.trim().replace(/\s+/g, " ")).filter(Boolean));
}

describe("the page's stylesheet keeps a drag from selecting text (WebKit, the plugin's window on macOS)", () => {
  it("makes the page unselectable in both spellings, the prefixed one being the only one that WebKit knows", () => {
    const body = declarationsOf("body");
    expect(body).toContain("-webkit-user-select: none");
    expect(body).toContain("user-select: none");
  });

  it("keeps what takes text and what is shown to be copied selectable, in both spellings", () => {
    const selectable = declarationsOf("input, textarea, [contenteditable=\"true\"], .tang-notice__text, .tang-session");
    expect(selectable).toContain("-webkit-user-select: text");
    expect(selectable).toContain("user-select: text");
  });
});
