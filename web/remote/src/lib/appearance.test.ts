import { afterEach, describe, expect, it } from "vitest";
import { getAppearance, isDark, setAppearance } from "./appearance";

afterEach(() => setAppearance("system"));

describe("appearance", () => {
  it("defaults to the system and sets no override", () => {
    expect(getAppearance()).toBe("system");
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });

  it("stores a forced choice and marks <html>", () => {
    setAppearance("dark");
    expect(getAppearance()).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    expect(isDark()).toBe(true);
    setAppearance("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(isDark()).toBe(false);
  });

  it("going back to system clears the override", () => {
    setAppearance("dark");
    setAppearance("system");
    expect(document.documentElement.dataset.theme).toBeUndefined();
    expect(getAppearance()).toBe("system");
  });
});
