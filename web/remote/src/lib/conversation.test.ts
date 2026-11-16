import { describe, expect, it } from "vitest";
import { mergeEntries, type ConvEntry } from "./conversation";

const u = (id: string, text = id): ConvEntry => ({ id, kind: "user", text });

describe("mergeEntries", () => {
  it("replace returns incoming", () => {
    expect(mergeEntries([u("a")], [u("b")], true)).toEqual([u("b")]);
    expect(mergeEntries([u("a")], [], true)).toEqual([]);
  });
  it("upsert replaces a known id in place and keeps order", () => {
    const out = mergeEntries([u("a"), u("b"), u("c")], [u("b", "new")], false);
    expect(out.map((e) => e.id)).toEqual(["a", "b", "c"]);
    expect(out[1].text).toBe("new");
  });
  it("appends unknown ids", () => {
    expect(mergeEntries([u("a")], [u("b"), u("c")], false).map((e) => e.id)).toEqual(["a", "b", "c"]);
  });
  it("caps to the last 200", () => {
    const list = Array.from({ length: 200 }, (_, i) => u(`e${i}`));
    const out = mergeEntries(list, [u("x"), u("y")], false);
    expect(out).toHaveLength(200);
    expect(out[0].id).toBe("e2");
    expect(out[199].id).toBe("y");
    expect(mergeEntries([], Array.from({ length: 300 }, (_, i) => u(`n${i}`)), true)).toHaveLength(200);
  });
});
