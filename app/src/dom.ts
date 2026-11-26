// A tiny helper to build DOM elements without innerHTML (file names can hold any characters,
// and text set with textContent is never parsed as HTML).

export type Child = Node | string | null | undefined | false | Child[];

type Attrs = Record<string, string | boolean | number | ((ev: Event) => void) | undefined>;

/**
 * `h("button", { id: "go", onclick: run }, "Repair")` creates `<button id="go">Repair</button>`.
 * Attributes starting with "on" become event listeners. `false` and `undefined` attributes are
 * left out; `true` sets an empty attribute (for example `disabled`).
 */
export function h(tag: string, attrs: Attrs = {}, children: Child = []): HTMLElement {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === false) continue;
    if (key.startsWith("on") && typeof value === "function") {
      el.addEventListener(key.slice(2), value);
    } else {
      el.setAttribute(key, value === true ? "" : String(value));
    }
  }
  append(el, children);
  return el;
}

function append(el: HTMLElement, child: Child) {
  if (child === null || child === undefined || child === false) return;
  if (Array.isArray(child)) child.forEach((c) => append(el, c));
  else el.append(child);
}
