import { JSDOM } from "jsdom";

let installed = false;

export function ensurePluginUiDomEnv(): void {
  if (installed) {
    return;
  }
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "http://127.0.0.1:3334/",
  });
  const win = dom.window;

  Object.defineProperties(globalThis, {
    window: { value: win, configurable: true, writable: true },
    document: { value: win.document, configurable: true, writable: true },
    HTMLElement: { value: win.HTMLElement, configurable: true, writable: true },
    Element: { value: win.Element, configurable: true, writable: true },
    Node: { value: win.Node, configurable: true, writable: true },
    KeyboardEvent: {
      value: win.KeyboardEvent,
      configurable: true,
      writable: true,
    },
    MouseEvent: { value: win.MouseEvent, configurable: true, writable: true },
    Event: { value: win.Event, configurable: true, writable: true },
    CustomEvent: { value: win.CustomEvent, configurable: true, writable: true },
    MutationObserver: {
      value: win.MutationObserver,
      configurable: true,
      writable: true,
    },
    IS_REACT_ACT_ENVIRONMENT: { value: true, configurable: true, writable: true },
  });

  if (!("navigator" in globalThis)) {
    Object.defineProperty(globalThis, "navigator", {
      value: win.navigator,
      configurable: true,
    });
  }
  installed = true;
}

ensurePluginUiDomEnv();
