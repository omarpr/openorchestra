import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Newer Node ships its own (file-backed, often undefined) `localStorage` that can
// shadow jsdom's. Install a plain in-memory Storage so tests are deterministic.
class MemoryStorage implements Storage {
  private m = new Map<string, string>();
  get length() {
    return this.m.size;
  }
  clear() {
    this.m.clear();
  }
  getItem(k: string) {
    return this.m.has(k) ? (this.m.get(k) as string) : null;
  }
  key(i: number) {
    return [...this.m.keys()][i] ?? null;
  }
  removeItem(k: string) {
    this.m.delete(k);
  }
  setItem(k: string, v: string) {
    this.m.set(k, String(v));
  }
}
Object.defineProperty(window, "localStorage", { value: new MemoryStorage(), configurable: true });

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
// Plain assignment (not vi.stubGlobal) so tests' unstubAllGlobals() can't remove them.
if (!("ResizeObserver" in globalThis)) (globalThis as { ResizeObserver: unknown }).ResizeObserver = ResizeObserverStub;
if (!document.elementFromPoint) document.elementFromPoint = () => null;

afterEach(() => cleanup());
