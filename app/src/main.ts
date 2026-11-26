// The DATfix window.
//
// Plain TypeScript, no framework. The whole UI state lives in `state`. After any change, call
// `render()`, which rebuilds the page from the state. Builds are cheap: the page is small.
//
// Flow for the person using it:
//   1. Add one or more .DAT files (drop, or click).  -> each is read and summarized.
//   2. A reference clip is found automatically on the same card, or chosen by hand.
//   3. Output goes next to the .DAT unless they choose a folder.
//   4. Repair -> progress -> result (Show in Finder / Play), or a friendly error.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, asAppError, type AppError, type AppInfo, type DatSummary, type Progress, type ReferenceMatch, type RepairOutcome, type Stage } from "./api";
import * as fmt from "./format";
import { h, type Child } from "./dom";

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/** Where reference clips come from, for every file in the list. */
type RefChoice =
  | { kind: "auto" } // search the .DAT's folder or card
  | { kind: "file"; path: string }
  | { kind: "folder"; path: string }
  | { kind: "builtin" }; // engine profile bank (when the engine supports it)

interface RefState {
  status: "waiting" | "searching" | "ok" | "mismatch" | "error";
  match?: ReferenceMatch;
  error?: AppError;
  /** A clip chosen for this file only. */
  override?: string;
}

interface RunState {
  status: "idle" | "running" | "done" | "failed" | "cancelled";
  stage?: Stage;
  stageLabel?: string;
  done?: number;
  total?: number;
  unit?: "bytes" | "frames";
  /** Start time and amount at the start of the current stage, for speed and time left. */
  stageStart?: number;
  rate?: number;
  outcome?: RepairOutcome;
  error?: AppError;
}

interface Item {
  id: string;
  dat: string;
  name: string;
  reading: boolean;
  readFraction?: number;
  summary?: DatSummary;
  readError?: AppError;
  ref: RefState;
  output?: string;
  /** True when the person picked the output name themselves (never renamed behind their back). */
  outputChosen?: boolean;
  run: RunState;
}

const state = {
  info: { name: "DATfix", version: "", os: "macos", capabilities: { referenceFree: false } } as AppInfo,
  items: [] as Item[],
  refChoice: { kind: "auto" } as RefChoice,
  /** null = save next to each .DAT. */
  outDir: null as string | null,
  repairing: false,
  stopRequested: false,
  currentJob: null as string | null,
  dragging: false,
};

let nextId = 1;
const newId = (prefix: string) => `${prefix}-${nextId++}-${Date.now()}`;

// Background preparation (reading .DATs, searching references) runs one job at a time: the
// work is disk-bound and parallel reads of one card only slow each other down.
let prepQueue: Promise<unknown> = Promise.resolve();
function enqueue<T>(work: () => Promise<T>): Promise<T> {
  const p = prepQueue.then(work, work);
  prepQueue = p.catch(() => {});
  return p;
}

// ---------------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------------

/** Adds dropped or chosen paths: .DAT files and folders go to the list, .MP4s become the reference. */
async function addPaths(paths: string[]) {
  if (state.repairing || paths.length === 0) return;
  const directClips = paths.filter((p) => /\.mp4$/i.test(p));
  const rest = paths.filter((p) => !/\.mp4$/i.test(p));
  if (directClips.length > 0) {
    await setRefChoice({ kind: "file", path: directClips[0] });
  }
  if (rest.length === 0) return;
  let found: { dats: string[] };
  try {
    found = await api.sortPaths(rest);
  } catch (e) {
    showToast(asAppError(e).message);
    return;
  }
  const known = new Set(state.items.map((i) => i.dat));
  const fresh = found.dats.filter((d) => !known.has(d));
  if (fresh.length === 0) {
    announce(found.dats.length ? "Those files are already in the list." : "No .DAT files were found there.");
    showToast(found.dats.length ? "Those files are already in the list." : "No .DAT files were found there. A .DAT file is what a Canon camera leaves when a recording is cut off.");
    return;
  }
  for (const dat of fresh) {
    const item: Item = {
      id: newId("item"),
      dat,
      name: fmt.baseName(dat),
      reading: true,
      ref: { status: "waiting" },
      run: { status: "idle" },
    };
    state.items.push(item);
    void prepare(item);
  }
  announce(`${fresh.length} file${fresh.length > 1 ? "s" : ""} added.`);
  render();
}

/** Reads a .DAT, then finds its reference and output name. */
async function prepare(item: Item) {
  await enqueue(async () => {
    if (!state.items.includes(item)) return;
    const job = newId("inspect");
    try {
      item.summary = await api.inspectDat(job, item.dat, (p) => {
        if (p.type === "step" && p.total > 0) {
          item.readFraction = p.done / p.total;
          scheduleRender();
        }
      });
    } catch (e) {
      item.readError = asAppError(e);
    }
    item.reading = false;
    render();
  });
  if (item.summary) {
    await refreshOutput(item);
    await resolveReference(item);
  }
}

/** Finds or checks the reference for one item, following the current choice. */
async function resolveReference(item: Item) {
  if (!item.summary || item.run.status === "done") return;
  const choice = state.refChoice;
  item.ref = { status: "searching", override: item.ref.override };
  render();
  await enqueue(async () => {
    if (!state.items.includes(item)) return;
    const job = newId("ref");
    try {
      let m: ReferenceMatch;
      if (item.ref.override) m = await api.checkReference(job, item.dat, item.ref.override);
      else if (choice.kind === "file") m = await api.checkReference(job, item.dat, choice.path);
      else if (choice.kind === "folder") m = await api.findReference(job, item.dat, choice.path);
      else if (choice.kind === "auto") m = await api.findReference(job, item.dat, null);
      else throw notAvailableYet();
      // The choice may have changed while this ran; the newer run wins.
      if (choice !== state.refChoice && !item.ref.override) return;
      item.ref = { status: m.ok ? "ok" : "mismatch", match: m, override: item.ref.override };
    } catch (e) {
      if (choice !== state.refChoice && !item.ref.override) return;
      item.ref = { status: "error", error: asAppError(e), override: item.ref.override };
    }
    render();
  });
}

function notAvailableYet(): AppError {
  return {
    kind: "not_available",
    title: "Not available yet",
    message: "This version of DATfix needs a healthy clip from the same camera.",
    nextStep: "Choose a clip, or search a folder of clips from the same camera.",
    reasons: [],
    details: "reference-free repair is not supported by this engine build",
  };
}

async function setRefChoice(choice: RefChoice) {
  state.refChoice = choice;
  for (const item of state.items) item.ref.override = undefined;
  render();
  await Promise.all(state.items.map((i) => resolveReference(i)));
}

/** Picks a free output name for an item, in the chosen folder or next to the .DAT. */
async function refreshOutput(item: Item) {
  const taken = state.items.filter((i) => i !== item && i.output).map((i) => i.output!);
  try {
    item.output = await api.suggestOutput(item.dat, state.outDir, taken);
  } catch {
    item.output = undefined;
  }
  render();
}

async function refreshAllOutputs() {
  for (const i of state.items) {
    i.output = undefined;
    i.outputChosen = false;
  }
  for (const i of state.items) if (i.summary && i.run.status !== "done") await refreshOutput(i);
}

/** Clears the list for a new repair. Keeps the reference and output choices. */
function startOver() {
  state.items = [];
  render();
  document.getElementById("dropzone")?.focus();
}

function removeItem(item: Item) {
  state.items = state.items.filter((i) => i !== item);
  announce(`${item.name} removed.`);
  render();
}

const readyItems = () =>
  state.items.filter((i) => i.summary && i.ref.status === "ok" && i.output && i.run.status !== "done");

/** Repairs every ready item, one after another. */
async function repairAll() {
  const queue = readyItems();
  if (queue.length === 0 || state.repairing) return;
  state.repairing = true;
  state.stopRequested = false;
  for (const item of queue) item.run = { status: "idle" };
  render();
  for (const item of queue) {
    if (state.stopRequested) break;
    await repairOne(item);
  }
  state.repairing = false;
  state.currentJob = null;
  render();
  const done = queue.filter((i) => i.run.status === "done").length;
  announce(done === queue.length ? "Repair finished." : `${done} of ${queue.length} files repaired.`);
  document.getElementById("results")?.focus();
}

async function repairOne(item: Item) {
  const job = newId("repair");
  state.currentJob = job;
  item.run = { status: "running", stageLabel: "Starting" };
  render();
  try {
    // Re-pick a suggested name right before writing: a file with that name may exist by now.
    if (!item.outputChosen) await refreshOutput(item);
    const outcome = await api.repairDat(job, item.dat, item.ref.match!.path, item.output!, (p) => onProgress(item, p));
    item.run = { status: "done", outcome };
  } catch (e) {
    const err = asAppError(e);
    item.run = { status: err.kind === "cancelled" ? "cancelled" : "failed", error: err };
  }
  render();
}

function onProgress(item: Item, p: Progress) {
  const run = item.run;
  const now = performance.now();
  if (p.type === "stage") {
    run.stage = p.stage;
    run.stageLabel = p.label;
    run.done = 0;
    run.total = 0;
    run.rate = undefined;
    run.stageStart = now;
  } else if (p.type === "step") {
    run.done = p.done;
    run.total = p.total;
    run.unit = p.unit;
    const elapsed = (now - (run.stageStart ?? now)) / 1000;
    // Wait a moment before showing a speed; the first reads are not representative.
    if (elapsed > 1.5 && p.done > 0) {
      const instant = p.done / elapsed;
      run.rate = run.rate ? run.rate * 0.8 + instant * 0.2 : instant;
    }
  }
  scheduleRender();
}

async function cancelRepair() {
  state.stopRequested = true;
  if (state.currentJob) await api.cancelJob(state.currentJob);
  announce("Stopping…");
  render();
}

// ---------------------------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------------------------

async function chooseDats() {
  const picked = await open({
    multiple: true,
    title: "Choose .DAT files",
    filters: [{ name: "Canon .DAT recording", extensions: ["DAT", "dat"] }],
  });
  if (picked) await addPaths(Array.isArray(picked) ? picked : [picked]);
}

async function chooseCardFolder() {
  const picked = await open({ directory: true, title: "Choose a card or folder with .DAT files" });
  if (typeof picked === "string") await addPaths([picked]);
}

async function chooseClip(item?: Item) {
  const picked = await open({
    title: "Choose a healthy clip from the same camera",
    filters: [{ name: "Video clip", extensions: ["MP4", "mp4"] }],
    defaultPath: item ? fmt.dirName(item.dat) : undefined,
  });
  if (typeof picked !== "string") return;
  if (item) {
    item.ref.override = picked;
    await resolveReference(item);
  } else {
    await setRefChoice({ kind: "file", path: picked });
  }
}

async function chooseRefFolder() {
  const picked = await open({ directory: true, title: "Choose a folder of clips from the same camera" });
  if (typeof picked === "string") await setRefChoice({ kind: "folder", path: picked });
}

async function chooseOutputFile(item: Item) {
  const picked = await save({
    title: "Save the repaired video as",
    defaultPath: item.output,
    filters: [{ name: "MP4 video", extensions: ["mp4"] }],
  });
  if (picked) {
    item.output = /\.mp4$/i.test(picked) ? picked : `${picked}.mp4`;
    item.outputChosen = true;
    render();
  }
}

async function chooseOutputFolder() {
  const picked = await open({ directory: true, title: "Choose where to save repaired videos" });
  if (typeof picked === "string") {
    state.outDir = picked;
    await refreshAllOutputs();
  }
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

let renderPending = false;
/** Coalesces many progress updates into one render per frame. */
function scheduleRender() {
  if (renderPending) return;
  renderPending = true;
  requestAnimationFrame(() => {
    renderPending = false;
    render();
  });
}

function render() {
  const root = document.getElementById("app")!;
  // Keep keyboard focus on the same control across re-renders.
  const focusedId = (document.activeElement as HTMLElement | null)?.id;
  const sections: (HTMLElement | null)[] = [
    h("header", { class: "top" }, [
      h("h1", {}, state.info.name),
      h("p", { class: "lede" }, "Get back the video from a Canon .DAT file. Your original file is never changed."),
    ]),
    renderFiles(),
    state.items.some((i) => i.summary) ? renderReference() : null,
    state.items.some((i) => i.summary) ? renderOutput() : null,
    state.items.length ? renderAction() : null,
    h("footer", { class: "foot" }, [
      `Free and open source · Not affiliated with Canon`,
      state.info.version ? h("span", { class: "muted" }, ` · v${state.info.version}`) : null,
    ]),
  ];
  root.replaceChildren(...sections.filter((x): x is HTMLElement => x !== null));
  if (focusedId) document.getElementById(focusedId)?.focus();
}

function renderFiles(): HTMLElement {
  const empty = state.items.length === 0;
  const zone = h(
    "button",
    {
      id: "dropzone",
      class: `dropzone${state.dragging ? " dragging" : ""}${empty ? "" : " compact"}`,
      type: "button",
      disabled: state.repairing,
      onclick: () => void chooseDats(),
    },
    empty
      ? [
          h("span", { class: "drop-icon", "aria-hidden": "true" }, "⬇︎"),
          h("span", { class: "drop-title" }, "Drop your .DAT file here"),
          h("span", { class: "drop-sub" }, "or click to choose it. You can also drop several files, or a whole card folder."),
        ]
      : [h("span", { class: "drop-title small" }, "Add more .DAT files"), h("span", { class: "drop-sub" }, "Drop them here, or click to choose.")],
  );
  return h("section", { class: "card", "aria-labelledby": "files-h" }, [
    h("h2", { id: "files-h" }, [h("span", { class: "step" }, "1"), "Your .DAT file" + (state.items.length > 1 ? "s" : "")]),
    zone,
    empty
      ? h("p", { class: "hint" }, [
          "Have the whole card? ",
          h("button", { id: "choose-card", class: "link", type: "button", onclick: () => void chooseCardFolder() }, "Choose the card folder"),
          " and DATfix will find every .DAT on it.",
        ])
      : h("ul", { class: "items", "aria-label": "Files to repair" }, state.items.map(renderItem)),
  ]);
}

function renderItem(item: Item): HTMLElement {
  const s = item.summary;
  const rows: Child[] = [];
  if (item.reading) {
    rows.push(
      h("p", { class: "muted" }, "Reading the file…"),
      progressBar(item.readFraction ?? 0, `Reading ${item.name}`),
    );
  } else if (item.readError) {
    rows.push(errorBox(item.readError, item));
  } else if (s) {
    const length =
      s.estimatedSeconds != null
        ? `About ${fmt.duration(s.estimatedSeconds)} of video can be recovered`
        : `${s.videoFrames.toLocaleString()} video frames can be recovered (the exact length is shown after repair)`;
    rows.push(
      h("p", { class: "big" }, length),
      h("dl", { class: "facts" }, [
        fact("Camera", s.camera),
        fact("Contains", s.contents || "unknown"),
        fact("Size", fmt.bytes(s.sizeBytes)),
        fact(
          "Cut off at the end",
          s.cutOff ? "Yes. The unfinished last moment (under a second) is left out." : "No. The file ends cleanly.",
        ),
      ]),
    );
    if (state.items.length > 1) rows.push(renderItemReference(item), renderItemOutput(item));
  }
  rows.push(renderRun(item));
  return h("li", { class: "item" }, [
    h("div", { class: "item-head" }, [
      h("span", { class: "file-name", title: item.dat }, item.name),
      state.repairing || item.run.status === "running"
        ? null
        : h(
            "button",
            { id: `remove-${item.id}`, class: "icon-btn", type: "button", "aria-label": `Remove ${item.name}`, onclick: () => removeItem(item) },
            "✕",
          ),
    ]),
    ...rows,
  ]);
}

function fact(label: string, value: string): Child[] {
  return [h("dt", {}, label), h("dd", {}, value)];
}

function renderReference(): HTMLElement {
  const single = state.items.length === 1 ? state.items[0] : undefined;
  const c = state.refChoice;
  const choiceText =
    c.kind === "auto"
      ? "Looking on the same card or folder automatically."
      : c.kind === "file"
        ? `Using ${fmt.baseName(c.path)}.`
        : c.kind === "folder"
          ? `Searching ${c.path} and its subfolders.`
          : "Using DATfix's built-in camera settings.";
  const canBuiltin = state.info.capabilities.referenceFree;
  return h("section", { class: "card", "aria-labelledby": "ref-h" }, [
    h("h2", { id: "ref-h" }, [h("span", { class: "step" }, "2"), "A healthy clip from the same camera"]),
    h(
      "p",
      { class: "explain" },
      "A .DAT file is missing the settings a video player needs. DATfix copies them from any clip that plays normally and was shot on the same camera with the same resolution, frame rate and sound settings.",
    ),
    h("p", { class: "muted" }, choiceText),
    h("div", { class: "row" }, [
      h("button", { id: "choose-clip", type: "button", disabled: state.repairing, onclick: () => void chooseClip() }, "Choose a clip…"),
      h("button", { id: "search-folder", type: "button", disabled: state.repairing, onclick: () => void chooseRefFolder() }, "Search a folder…"),
      h(
        "button",
        {
          id: "try-builtin",
          type: "button",
          disabled: state.repairing || !canBuiltin,
          title: canBuiltin ? "" : "Coming in a later version",
          onclick: () => void setRefChoice({ kind: "builtin" }),
        },
        canBuiltin ? "Try without a reference" : "Try without a reference (coming soon)",
      ),
      c.kind !== "auto"
        ? h("button", { id: "ref-auto", class: "link", type: "button", disabled: state.repairing, onclick: () => void setRefChoice({ kind: "auto" }) }, "Find automatically")
        : null,
    ]),
    single?.summary ? renderItemReference(single) : null,
  ]);
}

/** Match status for one item, in plain words. */
function renderItemReference(item: Item): HTMLElement {
  const r = item.ref;
  const label = state.items.length > 1 ? "Reference: " : "";
  const pickOwn =
    state.items.length > 1 && (r.status === "mismatch" || r.status === "error") && !state.repairing
      ? h("button", { id: `clip-${item.id}`, class: "link", type: "button", onclick: () => void chooseClip(item) }, "Choose a clip for this file")
      : null;
  switch (r.status) {
    case "waiting":
      return h("p", { class: "status muted" }, `${label}waiting…`);
    case "searching":
      return h("p", { class: "status muted" }, [spinner(), `${label}looking for a matching clip…`]);
    case "ok":
      return h("p", { class: "status ok" }, [
        h("span", { "aria-hidden": "true" }, "✓ "),
        `${label}${fmt.baseName(r.match!.path)} matches this file.`,
      ]);
    case "mismatch":
      return h("div", { class: "status bad" }, [
        h("p", {}, [h("span", { "aria-hidden": "true" }, "✗ "), `${label}${fmt.baseName(r.match!.path)} does not match:`]),
        h("ul", {}, r.match!.reasons.map((x) => h("li", {}, x))),
        h("p", {}, "Pick a clip recorded on the same camera with the same settings."),
        pickOwn,
      ]);
    case "error":
      return h("div", {}, [errorBox(r.error!, item), pickOwn]);
  }
}

function renderOutput(): HTMLElement {
  const single = state.items.length === 1 ? state.items[0] : undefined;
  const body: Child[] = [];
  if (single) {
    body.push(
      h("p", { class: "path", title: single.output ?? "" }, single.output ?? "…"),
      h("div", { class: "row" }, [
        h("button", { id: "change-output", type: "button", disabled: state.repairing || !single.output, onclick: () => void chooseOutputFile(single) }, "Change…"),
      ]),
    );
  } else {
    body.push(
      h("p", {}, state.outDir ? `In ${state.outDir}` : "Next to each .DAT file."),
      h("div", { class: "row" }, [
        h("button", { id: "output-folder", type: "button", disabled: state.repairing, onclick: () => void chooseOutputFolder() }, "Choose a folder…"),
        state.outDir
          ? h("button", { id: "output-reset", class: "link", type: "button", disabled: state.repairing, onclick: () => { state.outDir = null; void refreshAllOutputs(); } }, "Save next to each .DAT")
          : null,
      ]),
    );
  }
  body.push(h("p", { class: "muted small" }, "A new file is always created. Nothing is replaced, and the .DAT is never changed."));
  return h("section", { class: "card", "aria-labelledby": "out-h" }, [
    h("h2", { id: "out-h" }, [h("span", { class: "step" }, "3"), "Save the repaired video"]),
    ...body,
  ]);
}

function renderItemOutput(item: Item): HTMLElement {
  return h("p", { class: "muted small path", title: item.output ?? "" }, item.output ? `Saves as ${fmt.baseName(item.output)}` : "");
}

function renderAction(): HTMLElement {
  const ready = readyItems().length;
  const allDone = state.items.length > 0 && state.items.every((i) => i.run.status === "done");
  if (allDone && !state.repairing) {
    return h("section", { class: "card action", "aria-labelledby": "act-h" }, [
      h("h2", { id: "act-h", class: "visually-hidden" }, "Finished"),
      h("div", { class: "row spread" }, [
        h("p", { class: "done-msg" }, state.items.length > 1 ? "All files repaired." : "Your video is repaired."),
        h("button", { id: "start-over", type: "button", onclick: startOver }, "Repair another file"),
      ]),
    ]);
  }
  const label = state.repairing ? "Repairing…" : ready > 1 ? `Repair ${ready} files` : "Repair";
  const waiting = state.items.some((i) => i.reading || i.ref.status === "searching");
  return h("section", { class: "card action", "aria-labelledby": "act-h" }, [
    h("h2", { id: "act-h", class: "visually-hidden" }, "Repair"),
    state.repairing
      ? h("button", { id: "cancel", class: "danger big-btn", type: "button", disabled: state.stopRequested, onclick: () => void cancelRepair() }, state.stopRequested ? "Stopping…" : "Cancel")
      : h("button", { id: "repair", class: "primary big-btn", type: "button", disabled: ready === 0, onclick: () => void repairAll() }, label),
    !state.repairing && ready === 0
      ? h("p", { class: "muted small" }, waiting ? "Getting ready…" : "Add a .DAT file and a matching clip to start.")
      : null,
  ]);
}

/** Progress, result or error of a repair, inside the item card. */
function renderRun(item: Item): Child {
  const r = item.run;
  if (r.status === "running") {
    const frac = r.total ? (r.done ?? 0) / r.total : 0;
    const stepNo = { indexing: 1, checkingReference: 2, analyzingVideo: 3, writing: 4 }[r.stage ?? "indexing"];
    const amount =
      r.total && r.unit === "bytes"
        ? `${fmt.bytes(r.done ?? 0)} of ${fmt.bytes(r.total)}`
        : r.total
          ? `${(r.done ?? 0).toLocaleString()} of ${r.total.toLocaleString()} frames`
          : "";
    const speed = r.rate && r.unit === "bytes" ? `${fmt.bytes(r.rate)}/s` : r.rate ? `${Math.round(r.rate)} frames/s` : "";
    const left = r.rate && r.total ? fmt.eta((r.total - (r.done ?? 0)) / r.rate) : "";
    return h("div", { class: "run" }, [
      h("p", { class: "stage" }, `Step ${stepNo} of 4: ${r.stageLabel ?? "Starting"}`),
      progressBar(frac, r.stageLabel ?? "Repair progress"),
      h("p", { class: "muted small" }, [amount, speed, left].filter(Boolean).join(" · ") || " "),
    ]);
  }
  if (r.status === "done" && r.outcome) {
    const o = r.outcome;
    const reveal = state.info.os === "windows" ? "Show in Explorer" : state.info.os === "macos" ? "Show in Finder" : "Show in folder";
    return h("div", { class: "result", id: "results", tabindex: "-1" }, [
      h("p", { class: "recovered" }, [h("span", { "aria-hidden": "true" }, "✓ "), `Recovered ${fmt.duration(o.durationSeconds)}`]),
      h(
        "p",
        { class: "muted small" },
        `${o.videoFrames.toLocaleString()} frames · ${o.audioTracks} audio track${o.audioTracks === 1 ? "" : "s"} · ${fmt.bytes(o.bytesWritten)} · saved as ${fmt.baseName(o.output)}`,
      ),
      h("div", { class: "row" }, [
        h("button", { id: `reveal-${item.id}`, class: "primary", type: "button", onclick: () => void revealItemInDir(o.output).catch(openFailed) }, reveal),
        h("button", { id: `play-${item.id}`, type: "button", onclick: () => void openPath(o.output).catch(openFailed) }, "Play"),
      ]),
      o.notes.length
        ? h("details", {}, [h("summary", {}, "Details"), h("ul", { class: "small" }, o.notes.map((n) => h("li", {}, n)))])
        : null,
    ]);
  }
  if ((r.status === "failed" || r.status === "cancelled") && r.error) {
    return r.status === "cancelled"
      ? h("p", { class: "status muted", id: "results", tabindex: "-1" }, "Stopped. No partial file was left behind.")
      : h("div", { id: "results", tabindex: "-1" }, [errorBox(r.error, item)]);
  }
  return null;
}

function errorBox(err: AppError, item?: Item): HTMLElement {
  const copyId = `copy-${item?.id ?? "x"}-${err.kind}`;
  return h("div", { class: "error", role: "alert" }, [
    h("p", { class: "error-title" }, err.title),
    h("p", {}, err.message),
    err.reasons.length ? h("ul", {}, err.reasons.map((x) => h("li", {}, x))) : null,
    h("p", { class: "next" }, [h("strong", {}, "What to do: "), err.nextStep]),
    h("button", { id: copyId, class: "link", type: "button", onclick: (ev: Event) => void copyDetails(err, item, ev.currentTarget as HTMLButtonElement) }, "Copy details"),
  ]);
}

function progressBar(fraction: number, label: string): HTMLElement {
  const pct = Math.max(0, Math.min(100, fraction * 100));
  return h("div", { class: "bar", role: "progressbar", "aria-label": label, "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": pct.toFixed(0) }, [
    h("div", { class: "bar-fill", style: `width:${pct.toFixed(2)}%` }),
  ]);
}

const spinner = () => h("span", { class: "spinner", "aria-hidden": "true" });

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

/** Puts a bug-report-ready text on the clipboard. */
async function copyDetails(err: AppError, item: Item | undefined, button: HTMLButtonElement) {
  const s = item?.summary;
  const lines = [
    `${state.info.name} ${state.info.version} on ${state.info.os} (${navigator.userAgent})`,
    item ? `File: ${item.dat}` : "",
    s ? `Size: ${s.sizeBytes} bytes; camera: ${s.camera}; contents: ${s.contents}; frames: ${s.videoFrames}; cut off: ${s.cutOff}` : "",
    item?.ref.match ? `Reference: ${item.ref.match.path} (ok: ${item.ref.match.ok})` : "",
    item?.output ? `Output: ${item.output}` : "",
    `Error: ${err.kind}: ${err.title}`,
    err.details,
    ...err.reasons.map((r) => `- ${r}`),
  ].filter(Boolean);
  const text = lines.join("\n");
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // Older web views: fall back to a hidden text area.
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  button.textContent = "Copied";
  announce("Details copied.");
  setTimeout(() => (button.textContent = "Copy details"), 2000);
}

function openFailed(e: unknown) {
  showToast(`Could not open the file: ${String(e)}`);
}

function announce(text: string) {
  const el = document.getElementById("announcer");
  if (el) el.textContent = text;
}

function showToast(text: string) {
  const old = document.querySelector(".toast");
  old?.remove();
  const t = h("div", { class: "toast", role: "status" }, text);
  document.body.append(t);
  setTimeout(() => t.remove(), 6000);
}

// ---------------------------------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------------------------------

async function start() {
  try {
    state.info = await api.appInfo();
    document.title = state.info.name;
  } catch {
    // Running in a plain browser (vite dev without Tauri): keep the defaults.
  }
  render();

  // Files dragged from Finder or Explorer arrive through Tauri, with real paths.
  try {
    await getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload;
      if (p.type === "enter" || p.type === "over") {
        if (!state.dragging) {
          state.dragging = true;
          render();
        }
      } else if (p.type === "drop") {
        state.dragging = false;
        render();
        void addPaths(p.paths);
      } else {
        state.dragging = false;
        render();
      }
    });
  } catch {
    // Not inside Tauri.
  }

  await devAutorun();
}

/** Test hook: drives the UI from DATFIX_DEV_AUTORUN (see commands.rs). */
async function devAutorun() {
  let cfg;
  try {
    cfg = await api.devAutorun();
  } catch {
    return;
  }
  if (!cfg) return;
  if (cfg.outDir) state.outDir = cfg.outDir;
  if (cfg.reference) {
    const isClip = /\.mp4$/i.test(cfg.reference);
    state.refChoice = isClip ? { kind: "file", path: cfg.reference } : { kind: "folder", path: cfg.reference };
  }
  await addPaths(cfg.dats);
  // Wait until every file is read and has a reference result.
  while (state.items.some((i) => i.reading || i.ref.status === "searching" || i.ref.status === "waiting" && !i.readError || (i.summary && !i.output))) {
    await new Promise((r) => setTimeout(r, 200));
  }
  const report = () =>
    api.devReport(
      JSON.stringify(
        state.items.map((i) => ({ dat: i.dat, ref: i.ref.status, refPath: i.ref.match?.path, readError: i.readError?.kind, run: i.run.status, outcome: i.run.outcome, error: i.run.error?.kind })),
      ),
    );
  if (cfg.scrollY) window.scrollTo(0, cfg.scrollY);
  if (cfg.repair === false) {
    await report();
    return;
  }
  if (cfg.cancelAfterMs) setTimeout(() => void cancelRepair(), cfg.cancelAfterMs);
  await repairAll();
  await report();
}

void start();
