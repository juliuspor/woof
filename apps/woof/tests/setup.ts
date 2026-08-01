import "@testing-library/jest-dom/vitest";
import { vi } from "vitest";
import { invokeMock } from "./native-bridge.mock";

vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    const listener = (raw: Event) =>
      handler({ payload: (raw as CustomEvent<unknown>).detail });
    window.addEventListener(event, listener);
    return () => window.removeEventListener(event, listener);
  }
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ hide: async () => undefined })
}));

const storage = new Map<string, string>();

Object.defineProperty(window, "localStorage", {
  configurable: true,
  value: {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, String(value)),
    removeItem: (key: string) => storage.delete(key),
    clear: () => storage.clear(),
    key: (index: number) => [...storage.keys()][index] ?? null,
    get length() {
      return storage.size;
    }
  }
});

Object.defineProperty(window, "__TAURI_INTERNALS__", {
  configurable: true,
  value: { invoke: invokeMock }
});

Object.defineProperty(globalThis, "isTauri", {
  configurable: true,
  value: true
});

Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false
  })
});

Object.defineProperty(Element.prototype, "scrollIntoView", {
  writable: true,
  value: () => undefined
});
