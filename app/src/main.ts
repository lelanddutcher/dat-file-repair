// The DATfix window.
//
// Plain TypeScript, no framework. The whole UI state lives in `state`. After any change, call
// `render()`, which rebuilds the page from the state. Builds are cheap: the page is small.
//
// Flow for the person using it:
//   1. Add one or more .DAT files (drop, or click).  -> each is read and summarized.
//   2. Settings: a clip found automatically on the same card, else DATfix's built-in settings
//      for that camera. A clip the person chooses always wins.
//   3. Output goes next to the .DAT unless they choose a folder.
//   4. Repair -> progress -> result (Show in Finder / Play), or a friendly error.
//
// "Recover from a memory card" (card.ts) has its own screens. It copies recordings off a card
// image and then hands them to this flow, with the card's finished clips as references.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import { api, asAppError, type AppError, type AppInfo, type CardScanView, type DatSummary, type DevCardRun, type PartialView, type PreparedCard, type Progress, type RecordingHint, type ReferenceMatch, type RepairChoices, type RepairOutcome, type Stage } from "./api";
import { card, initCard, openCard, renderCard, scanImage, makeImage, scanDirect, cancelJob as cancelCardJob, repairAll as cardRepairAll } from "./card";
import * as fmt from "./format";
import { h, type Child } from "./dom";

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/** Where reference clips come from, for every file in the list. */
type RefChoice =
  | { kind: "auto" } // search the .DAT's folder or card, then the built-in settings
  | { kind: "file"; path: string }
  | { kind: "folder"; path: string }
  | { kind: "builtin" }; // the engine's built-in camera settings

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
  /** The "Advanced" options. */
  choices: RepairChoices;
  /** What the memory card said about a recording copied off it (name, card times). */
  hint: RecordingHint | null;
}

const state = {
  info: { name: "DATfix", version: "", os: "macos", capabilities: { referenceFree: false }, theme: null } as AppInfo,
  items: [] as Item[],
  refChoice: { kind: "auto" } as RefChoice,
  /** null = save next to each .DAT. */
  outDir: null as string | null,
  repairing: false,
  stopRequested: false,
  currentJob: null as string | null,
  dragging: false,
  /** Unfinished files from a repair that was killed, found next to the outputs. */
  partials: [] as PartialView[],
  partialsNote: null as string | null,
  /** "card" shows the "Recover from a memory card" screens. */
  view: "main" as "main" | "card",
  /** Shown above the list after recordings were copied off a card. */
  cardNote: null as { text: string; problems: string[]; clipsDir: string | null } | null,
  /** Ids of <details> sections the person opened, so a re-render keeps them open. */
  open: new Set<string>(),
};

/** File types that are card images, not .DAT files. */
const IMAGE_RE = /\.(img|dmg|dd|cdr|iso|raw|bin)$/i;

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
async function addPaths(paths: string[], hints?: Map<string, RecordingHint>) {
  if (state.repairing || paths.length === 0) return;
  // A card image goes to the card screens and is searched right away.
  const image = paths.find((p) => IMAGE_RE.test(p));
  if (image) {
    state.view = "card";
    openCard({ image });
    return;
  }
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
      choices: { rotation: null, keepReferenceMetadata: false },
      hint: hints?.get(dat) ?? null,
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
      else m = await api.checkBuiltin(job, item.dat);
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
  state.partialsNote = null;
  for (const item of queue) item.run = { status: "idle" };
  render();
  // A repair that was killed (force quit, power loss) leaves a hidden temporary file next to
  // its output. Offer to delete those; the repair goes on either way.
  try {
    const dirs = [...new Set(queue.map((i) => fmt.dirName(i.output!)))];
    state.partials = await api.findPartials(dirs);
  } catch {
    state.partials = [];
  }
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
    const m = item.ref.match!;
    const reference = m.source === "builtin" ? null : m.path;
    const outcome = await api.repairDat(job, item.dat, reference, item.output!, item.choices, item.hint, (p) => onProgress(item, p));
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

async function removePartials() {
  const files = state.partials;
  state.partials = [];
  try {
    const freed = await api.removePartials(files.map((p) => p.path));
    state.partialsNote = `Deleted ${files.length} unfinished file${files.length === 1 ? "" : "s"} (${fmt.bytes(freed)} freed).`;
  } catch (e) {
    state.partialsNote = `Could not delete them: ${asAppError(e).message}`;
  }
  announce(state.partialsNote);
  render();
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
  const header = h("header", { class: "top" }, [
    h("h1", {}, state.info.name),
    h("p", { class: "lede" }, "Get back the video from a Canon .DAT file. Your original file is never changed."),
  ]);
  const footer = h("footer", { class: "foot" }, [
    `Free and open source · Not affiliated with Canon`,
    state.info.version ? h("span", { class: "muted" }, ` · v${state.info.version}`) : null,
  ]);
  if (state.view === "card") {
    root.replaceChildren(header, ...renderCard(), footer);
    if (focusedId) document.getElementById(focusedId)?.focus();
    return;
  }
  const sections: (HTMLElement | null)[] = [
    header,
    renderFiles(),
    state.items.some((i) => i.summary) ? renderReference() : null,
    state.items.some((i) => i.summary) ? renderOutput() : null,
    state.items.length ? renderAction() : null,
    footer,
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
    renderCardNote(),
    zone,
    empty
      ? h("p", { class: "hint" }, [
          "Have the whole card? ",
          h("button", { id: "choose-card", class: "link", type: "button", onclick: () => void chooseCardFolder() }, "Choose the card folder"),
          " and DATfix will find every .DAT on it.",
        ])
      : h("ul", { class: "items", "aria-label": "Files to repair" }, state.items.map(renderItem)),
    empty
      ? h("div", { class: "card-entry" }, [
          h("p", { class: "small" }, "Is the .DAT file 0 bytes, missing or deleted? The video may still be on the memory card."),
          h("button", { id: "recover-card", type: "button", disabled: state.repairing, onclick: () => enterCard({}) }, "Recover from a memory card…"),
        ])
      : null,
  ]);
}

function enterCard(opts: { lookingFor?: string; hintPath?: string }) {
  state.view = "card";
  openCard(opts);
}

/** Where the files in the list came from, after a card recovery. */
function renderCardNote(): HTMLElement | null {
  const n = state.cardNote;
  if (!n) return null;
  return h("div", { class: "notice card-note", role: "status" }, [
    h("p", {}, n.text),
    n.problems.length ? h("ul", { class: "small" }, n.problems.map((p) => h("li", {}, p))) : null,
    n.clipsDir
      ? h("button", { id: "show-clips", class: "link small", type: "button", onclick: () => void revealItemInDir(n.clipsDir!).catch(openFailed) }, "Show the copied clips")
      : null,
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
        fact("Camera", s.frameRate ? `${s.camera}, ${s.frameRate} frames per second` : s.camera),
        fact("Contains", s.contents || "unknown"),
        fact("Size", fmt.bytes(s.sizeBytes)),
        fact(
          "Cut off at the end",
          s.cutOff ? "Yes. The unfinished last moment (under a second) is left out." : "No. The file ends cleanly.",
        ),
      ]),
    );
    // In a batch, each file shows its own settings and name until it is repaired; the result
    // then says the same thing.
    if (state.items.length > 1 && item.run.status !== "done") rows.push(renderItemReference(item), renderItemOutput(item));
    if (item.run.status !== "done" && item.run.status !== "running") rows.push(renderAdvanced(item));
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

/** A <details> section that stays open across re-renders once opened. */
function disclosure(id: string, cls: string, summary: string, children: Child): HTMLElement {
  return h(
    "details",
    {
      class: cls,
      id,
      open: state.open.has(id),
      ontoggle: (ev: Event) => {
        if ((ev.target as HTMLDetailsElement).open) state.open.add(id);
        else state.open.delete(id);
      },
    },
    [h("summary", {}, summary), children],
  );
}

const ROTATIONS: [string, string][] = [
  ["", "None"],
  ["90", "90° clockwise"],
  ["180", "180°"],
  ["270", "90° counter-clockwise (270°)"],
];

/** Per-file options: display rotation and the reference clip's camera metadata. */
function renderAdvanced(item: Item): HTMLElement {
  const c = item.choices;
  const off = state.repairing;
  const rotId = `rotate-${item.id}`;
  const metaId = `keep-meta-${item.id}`;
  return disclosure(`advanced-${item.id}`, "advanced", "Advanced", [
    h("div", { class: "adv-row" }, [
      h("label", { for: rotId }, "Rotate"),
      h(
        "select",
        {
          id: rotId,
          disabled: off,
          onchange: (ev: Event) => {
            const v = (ev.target as HTMLSelectElement).value;
            c.rotation = v === "" ? null : Number(v);
            render();
          },
        },
        ROTATIONS.map(([v, label]) => h("option", { value: v, selected: String(c.rotation ?? "") === v }, label)),
      ),
    ]),
    h("p", { class: "muted small adv-help" }, "Turns the picture when it plays, for clips filmed with the camera on its side. The video itself is not changed."),
    h("div", { class: "adv-row" }, [
      h("input", {
        id: metaId,
        type: "checkbox",
        checked: c.keepReferenceMetadata,
        disabled: off,
        onchange: (ev: Event) => {
          c.keepReferenceMetadata = (ev.target as HTMLInputElement).checked;
          render();
        },
      }),
      h("label", { for: metaId }, "Keep the reference clip's camera metadata"),
    ]),
    h(
      "p",
      { class: "muted small adv-help" },
      "Off by default: that clip's thumbnail, serial numbers, lens, dates and location describe another recording, so they are left out.",
    ),
  ]);
}

/** The recording time written into the file, for "Details". */
function recordedText(o: RepairOutcome): string {
  if (o.creationTime == null) return o.dateSource;
  const t = new Date(o.creationTime * 1000).toISOString().replace("T", " ").replace(/\.\d+Z$/, " UTC");
  return `${o.dateSource}: ${t}`;
}

function fact(label: string, value: string): Child[] {
  return [h("dt", {}, label), h("dd", {}, value)];
}

function renderReference(): HTMLElement {
  const single = state.items.length === 1 ? state.items[0] : undefined;
  const c = state.refChoice;
  const choiceText =
    c.kind === "auto"
      ? "DATfix looks for a matching clip on the same card or in the same folder. If there is none, it uses its built-in settings for your camera."
      : c.kind === "file"
        ? `Using the clip you chose: ${fmt.baseName(c.path)}.`
        : c.kind === "folder" && c.path === state.cardNote?.clipsDir
          ? "Using finished clips copied from the same card. If none matches, DATfix uses its built-in settings."
          : c.kind === "folder"
          ? `Searching ${c.path} and its subfolders. If no clip matches, DATfix uses its built-in settings.`
          : "Using DATfix's built-in settings for your camera. No clip needed.";
  const canBuiltin = state.info.capabilities.referenceFree;
  return h("section", { class: "card", "aria-labelledby": "ref-h" }, [
    h("h2", { id: "ref-h" }, [h("span", { class: "step" }, "2"), "Video settings"]),
    h(
      "p",
      { class: "explain" },
      "A .DAT file is missing the settings a video player needs. DATfix can copy them from a clip that plays normally and was shot on the same camera with the same settings, or use the settings it already knows for many Canon cameras.",
    ),
    h("p", { class: "muted" }, choiceText),
    h("div", { class: "row" }, [
      h("button", { id: "choose-clip", type: "button", disabled: state.repairing, onclick: () => void chooseClip() }, "Choose a clip…"),
      h("button", { id: "search-folder", type: "button", disabled: state.repairing, onclick: () => void chooseRefFolder() }, "Search a folder…"),
      canBuiltin && c.kind !== "builtin"
        ? h("button", { id: "try-builtin", type: "button", disabled: state.repairing, onclick: () => void setRefChoice({ kind: "builtin" }) }, "Try without a reference")
        : null,
      c.kind !== "auto"
        ? h("button", { id: "ref-auto", class: "link", type: "button", disabled: state.repairing, onclick: () => void setRefChoice({ kind: "auto" }) }, "Choose automatically")
        : null,
    ]),
    single?.summary ? renderItemReference(single) : null,
  ]);
}

/** Settings status for one item, in plain words. */
function renderItemReference(item: Item): HTMLElement {
  const r = item.ref;
  const many = state.items.length > 1;
  const pickOwn =
    many && (r.status === "mismatch" || r.status === "error") && !state.repairing
      ? h("button", { id: `clip-${item.id}`, class: "link", type: "button", onclick: () => void chooseClip(item) }, "Choose a clip for this file")
      : null;
  const useBuiltin =
    (r.status === "mismatch" || r.status === "error") && state.info.capabilities.referenceFree && !state.repairing && state.refChoice.kind !== "builtin"
      ? h(
          "button",
          {
            id: `builtin-${item.id}`,
            class: "link",
            type: "button",
            onclick: () => {
              if (many) {
                item.ref.override = undefined;
                void resolveBuiltin(item);
              } else void setRefChoice({ kind: "builtin" });
            },
          },
          "Use built-in settings instead",
        )
      : null;
  switch (r.status) {
    case "waiting":
      return h("p", { class: "status muted" }, "Waiting…");
    case "searching":
      return h("p", { class: "status muted" }, [spinner(), "Looking for matching settings…"]);
    case "ok": {
      const m = r.match!;
      return h("div", { class: "status ok-box" }, [
        h("p", { class: "ok" }, [h("span", { "aria-hidden": "true" }, "✓ "), m.summary]),
        m.searchNote ? h("p", { class: "muted small" }, m.searchNote) : null,
        m.rebuilt.length ? h("ul", { class: "small rebuilt" }, m.rebuilt.map((x) => h("li", {}, x))) : null,
        renderCandidates(m),
      ]);
    }
    case "mismatch": {
      const m = r.match!;
      return h("div", { class: "status bad" }, [
        h("p", {}, [h("span", { "aria-hidden": "true" }, "✗ "), m.summary]),
        h("ul", {}, m.reasons.map((x) => h("li", {}, x))),
        h("p", {}, "Choose a clip recorded on the same camera with the same settings."),
        h("div", { class: "row" }, [pickOwn, useBuiltin]),
        m.details ? detailsBox(m.details) : null,
      ]);
    }
    case "error":
      return h("div", {}, [errorBox(r.error!, item), h("div", { class: "row" }, [pickOwn, useBuiltin])]);
  }
}

/** The clips a search looked at, behind a disclosure. */
function renderCandidates(m: ReferenceMatch): HTMLElement | null {
  if (m.candidates.length === 0) return null;
  const n = m.candidates.length;
  return h("details", { class: "small" }, [
    h("summary", {}, `${n} clip${n === 1 ? "" : "s"} checked`),
    h(
      "ul",
      { class: "candidates" },
      m.candidates.map((c) =>
        h("li", { title: c.detail ?? "" }, [
          h("span", { class: "file-name" }, c.fileName),
          ": ",
          c.verdict === "fits" ? "matches" : c.verdict === "videoOnly" ? "video matches; sound or data tracks would be rebuilt" : (c.reason ?? "skipped"),
        ]),
      ),
    ),
  ]);
}

/** Built-in settings for one file of a batch. */
async function resolveBuiltin(item: Item) {
  item.ref = { status: "searching" };
  render();
  await enqueue(async () => {
    try {
      const m = await api.checkBuiltin(newId("ref"), item.dat);
      item.ref = { status: "ok", match: m, override: undefined };
    } catch (e) {
      item.ref = { status: "error", error: asAppError(e) };
    }
    render();
  });
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
    renderPartials(),
    state.repairing
      ? h("button", { id: "cancel", class: "danger big-btn", type: "button", disabled: state.stopRequested, onclick: () => void cancelRepair() }, state.stopRequested ? "Stopping…" : "Cancel")
      : h("button", { id: "repair", class: "primary big-btn", type: "button", disabled: ready === 0, onclick: () => void repairAll() }, label),
    !state.repairing && ready === 0 ? h("p", { class: "muted small" }, idleHint(waiting)) : null,
  ]);
}

/** Why the Repair button is off, in plain words. */
function idleHint(waiting: boolean): string {
  if (waiting) return "Getting ready…";
  const open = state.items.filter((i) => i.run.status !== "done");
  if (open.length > 0 && open.every((i) => i.readError)) return "Nothing here can be repaired. Remove it, or add another .DAT file.";
  if (open.some((i) => i.ref.status === "mismatch" || i.ref.status === "error")) return "Choose matching settings above to start.";
  return "Add a .DAT file to start.";
}

/** Offer to delete unfinished files left by a repair that was killed. */
function renderPartials(): HTMLElement | null {
  if (state.partialsNote) return h("p", { class: "muted small" }, state.partialsNote);
  const files = state.partials;
  if (files.length === 0) return null;
  const total = files.reduce((a, p) => a + p.bytes, 0);
  const n = files.length;
  return h("div", { class: "notice", role: "status" }, [
    h(
      "p",
      {},
      `Found ${n} unfinished file${n === 1 ? "" : "s"} (${fmt.bytes(total)}) from an earlier repair that was stopped before it finished. ${n === 1 ? "It is" : "They are"} hidden and can't be played.`,
    ),
    h("div", { class: "row" }, [
      h("button", { id: "partials-delete", type: "button", onclick: () => void removePartials() }, n === 1 ? "Delete it" : "Delete them"),
      h("button", { id: "partials-keep", class: "link", type: "button", onclick: () => { state.partials = []; render(); } }, "Keep"),
    ]),
  ]);
}

/** Progress, result or error of a repair, inside the item card. */
function renderRun(item: Item): Child {
  const r = item.run;
  if (r.status === "running") {
    const frac = r.total ? (r.done ?? 0) / r.total : 0;
    // The index was read when the file was added, so a repair has three steps. If the file
    // changed since, it is read again first ("Getting ready").
    const stepNo = { indexing: 0, checkingReference: 1, analyzingVideo: 2, writing: 3 }[r.stage ?? "checkingReference"];
    const amount =
      r.total && r.unit === "bytes"
        ? `${fmt.bytes(r.done ?? 0)} of ${fmt.bytes(r.total)}`
        : r.total
          ? `${(r.done ?? 0).toLocaleString()} of ${r.total.toLocaleString()} frames`
          : "";
    const speed = r.rate && r.unit === "bytes" ? `${fmt.bytes(r.rate)}/s` : r.rate ? `${Math.round(r.rate)} frames/s` : "";
    const left = r.rate && r.total ? fmt.eta((r.total - (r.done ?? 0)) / r.rate) : "";
    return h("div", { class: "run" }, [
      h("p", { class: "stage" }, stepNo ? `Step ${stepNo} of 3: ${r.stageLabel ?? "Starting"}` : `Getting ready: ${r.stageLabel ?? "Starting"}`),
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
      h("p", { class: "small" }, o.settings),
      o.rebuilt.length ? h("ul", { class: "small rebuilt" }, o.rebuilt.map((x) => h("li", {}, x))) : null,
      h("div", { class: "row" }, [
        h("button", { id: `reveal-${item.id}`, class: "primary", type: "button", onclick: () => void revealItemInDir(o.output).catch(openFailed) }, reveal),
        h("button", { id: `play-${item.id}`, type: "button", onclick: () => void openPath(o.output).catch(openFailed) }, "Play"),
      ]),
      disclosure(
        `details-${item.id}`,
        "",
        "Details",
        h("ul", { class: "small" }, [
          h("li", {}, recordedText(o)),
          o.rotationDegrees ? h("li", {}, `Rotated ${o.rotationDegrees}° clockwise when it plays`) : null,
          o.withheldMetadata.length
            ? h("li", {}, `Left out from the reference clip (privacy): ${o.withheldMetadata.join("; ")}`)
            : null,
          ...o.notes.map((n) => h("li", {}, n)),
        ]),
      ),
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
  // An empty .DAT: the video may still be on the card, and DATfix can search it.
  const emptyDat = err.kind === "empty_dat" && item;
  return h("div", { class: "error", role: "alert" }, [
    h("p", { class: "error-title" }, err.title),
    h("p", {}, err.message),
    err.reasons.length ? h("ul", {}, err.reasons.map((x) => h("li", {}, x))) : null,
    emptyDat
      ? h("div", {}, [
          h("p", { class: "next" }, [
            h("strong", {}, "What to do: "),
            "The video may still be on the memory card. Stop using the card and don't format it. DATfix can search the card for this recording. If DATfix finds nothing, a general recovery tool such as PhotoRec may still help.",
          ]),
          h("button", { id: `scan-card-${item.id}`, class: "primary", type: "button", disabled: state.repairing, onclick: () => enterCard({ lookingFor: item.name, hintPath: item.dat }) }, "Scan the memory card for this recording…"),
        ])
      : h("p", { class: "next" }, [h("strong", {}, "What to do: "), err.nextStep]),
    err.link
      ? h(
          "button",
          { id: `link-${item?.id ?? "x"}-${err.kind}`, type: "button", onclick: () => void openUrl(err.link!).catch(openFailed) },
          err.kind === "layout_not_supported" ? "Report this recording…" : "Open the issue page…",
        )
      : null,
    err.details ? detailsBox(err.details) : null,
    h("button", { id: copyId, class: "link", type: "button", onclick: (ev: Event) => void copyDetails(err, item, ev.currentTarget as HTMLButtonElement) }, "Copy details"),
  ]);
}

/** Technical text, hidden until asked for. */
function detailsBox(text: string): HTMLElement {
  return h("details", { class: "tech" }, [h("summary", {}, "Details"), h("pre", {}, text)]);
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
    item?.ref.match ? `Settings: ${item.ref.match.source} ${item.ref.match.path ?? ""} (ok: ${item.ref.match.ok}) ${item.ref.match.summary}` : "",
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

/** Repairs recordings copied off a card with the normal flow. */
async function repairFromCard(prepared: PreparedCard, outDir: string, scan: CardScanView) {
  const dats = prepared.recordings.filter((r) => r.dat).map((r) => r.dat!);
  const problems = prepared.recordings
    .filter((r) => r.error)
    .map((r) => `A recording at ${fmt.bytes(r.offset)} on the card could not be copied: ${r.error!.message}`);
  // The empty .DAT that led here has done its job; the copies replace it.
  state.items = state.items.filter((i) => i.readError?.kind !== "empty_dat");
  state.outDir = outDir;
  state.refChoice = prepared.clipsDir ? { kind: "folder", path: prepared.clipsDir } : { kind: "builtin" };
  state.cardNote = {
    text: `${dats.length === 1 ? "1 recording was" : `${dats.length} recordings were`} copied from ${scan.imageName} into ${outDir}.${
      prepared.clips ? ` ${prepared.clips === 1 ? "1 finished clip" : `${prepared.clips} finished clips`} from the same card ${prepared.clips === 1 ? "is" : "are"} used for the video settings.` : ""
    }`,
    problems,
    clipsDir: prepared.clipsDir,
  };
  state.view = "main";
  render();
  window.scrollTo(0, 0);
  const hints = new Map(prepared.recordings.filter((r) => r.dat).map((r) => [r.dat!, r.hint] as [string, RecordingHint]));
  await addPaths(dats, hints);
  await waitUntilReady();
  await repairAll();
}

/** Resolves when every file is read and has its settings and output name. */
async function waitUntilReady() {
  while (state.items.some((i) => i.reading || i.ref.status === "searching" || (i.ref.status === "waiting" && !i.readError) || (i.summary && !i.output))) {
    await new Promise((r) => setTimeout(r, 200));
  }
}

async function start() {
  initCard({
    render,
    scheduleRender,
    announce,
    toast: showToast,
    errorBox: (err) => errorBox(err),
    detailsBox,
    progressBar,
    spinner,
    os: () => state.info.os,
    close: () => {
      state.view = "main";
      render();
      window.scrollTo(0, 0);
    },
    repair: repairFromCard,
    devScreen,
  });
  try {
    state.info = await api.appInfo();
    document.title = state.info.name;
    // DATFIX_DEV_THEME forces a theme for screenshots; otherwise the system setting applies.
    if (state.info.theme) document.documentElement.dataset.theme = state.info.theme;
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
  if (cfg.card) {
    await devCardRun(cfg.card);
    return;
  }
  if (cfg.outDir) state.outDir = cfg.outDir;
  if (cfg.reference === "builtin") state.refChoice = { kind: "builtin" };
  else if (cfg.reference) {
    const isClip = /\.mp4$/i.test(cfg.reference);
    state.refChoice = isClip ? { kind: "file", path: cfg.reference } : { kind: "folder", path: cfg.reference };
  }
  devPauseMs = cfg.pauseMs ?? 0;
  await addPaths(cfg.dats);
  // Wait until every file is read and has a reference result.
  await waitUntilReady();
  for (const i of state.items) {
    if (cfg.rotation != null) i.choices.rotation = cfg.rotation;
    if (cfg.keepReferenceMetadata) i.choices.keepReferenceMetadata = true;
    if (cfg.openAdvanced) state.open.add(`advanced-${i.id}`);
  }
  render();
  if (cfg.scrollY) window.scrollTo(0, cfg.scrollY);
  await devScreen("ready");
  const report = () =>
    api.devReport(
      JSON.stringify(
        state.items.map((i) => ({
          dat: i.dat,
          camera: i.summary?.camera,
          estimatedSeconds: i.summary?.estimatedSeconds,
          ref: i.ref.status,
          refSource: i.ref.match?.source,
          refPath: i.ref.match?.path,
          refSummary: i.ref.match?.summary,
          searchNote: i.ref.match?.searchNote,
          rebuilt: i.ref.match?.rebuilt,
          checked: i.ref.match?.checked,
          readError: i.readError?.kind,
          readErrorTitle: i.readError?.title,
          run: i.run.status,
          outcome: i.run.outcome,
          error: i.run.error?.kind,
          partialsNote: state.partialsNote,
        })),
      ),
    );
  if (cfg.repair === false) {
    await report();
    return;
  }
  if (cfg.cancelAfterMs) setTimeout(() => void cancelRepair(), cfg.cancelAfterMs);
  const done = repairAll();
  if (devPauseMs) {
    await until(() => !state.repairing || state.items.some((i) => i.run.status === "running" && (i.run.done ?? 0) > 0), 60_000);
    if (state.repairing) await devScreen("repairing");
  }
  if (cfg.cleanPartials) {
    // Wait for the offer to appear, then accept it.
    for (let i = 0; i < 50 && state.partials.length === 0 && !state.partialsNote; i++) await new Promise((r) => setTimeout(r, 100));
    if (state.partials.length) await removePartials();
  }
  await done;
  for (const i of state.items) state.open.add(`details-${i.id}`);
  await devScreen("done");
  window.scrollTo(0, document.body.scrollHeight);
  await devScreen("done-bottom");
  await report();
}

/** Test hook: announces a screen on stdout and waits so a script can take a screenshot. */
let devPauseMs = 0;
async function devScreen(name: string) {
  if (!devPauseMs) return;
  render();
  await new Promise((r) => setTimeout(r, 150));
  await api.devReport(JSON.stringify({ screen: name }));
  await new Promise((r) => setTimeout(r, devPauseMs));
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
async function until(test: () => boolean, ms = 600_000) {
  const end = Date.now() + ms;
  while (!test() && Date.now() < end) await sleep(100);
}

/** Test hook: drives the card flow (see DevCardRun in api.ts). */
async function devCardRun(cfg: DevCardRun) {
  devPauseMs = cfg.pauseMs ?? 0;
  await devScreen("start");
  if (cfg.fromEmptyDat) {
    await addPaths([cfg.fromEmptyDat]);
    await waitUntilReady();
    await devScreen("empty-dat");
    const item = state.items.find((i) => i.readError?.kind === "empty_dat");
    enterCard({ lookingFor: item?.name, hintPath: item?.dat });
  } else {
    enterCard({});
  }
  await until(() => card.cards !== null);
  await devScreen("choose");
  if (cfg.outDir) card.outDir = cfg.outDir;
  let scanning: Promise<void>;
  if (cfg.device && cfg.imageDir && !cfg.direct) {
    card.mode = "make";
    card.device = cfg.device;
    card.imageDir = cfg.imageDir;
    await devScreen("choose-make");
    scanning = makeImage();
    await until(() => card.screen !== "imaging" || (card.job?.done ?? 0) > (card.job?.total ?? 1) * 0.3, 60_000);
    if (card.screen === "imaging") await devScreen("imaging");
  } else if (cfg.device && cfg.direct) {
    card.mode = "direct";
    card.device = cfg.device;
    await devScreen("choose-direct");
    scanning = scanDirect();
  } else {
    scanning = scanImage(cfg.image!);
  }
  await until(() => card.screen === "scanning" || card.screen === "results" || !!card.error);
  if (cfg.cancelAfterMs) {
    await sleep(cfg.cancelAfterMs);
    await devScreen("scanning");
    await cancelCardJob();
    await scanning;
    await devScreen("cancelled");
    await api.devReport(JSON.stringify({ screen: card.screen, error: card.error?.kind, scan: !!card.scan }));
    return;
  }
  await until(() => card.screen !== "scanning" || (card.job?.done ?? 0) > (card.job?.total ?? 1) * 0.4, 60_000);
  if (card.screen === "scanning") await devScreen("scanning");
  await scanning;
  await devScreen("results");
  card.open.add("clips");
  card.open.add("fragments");
  window.scrollTo(0, document.body.scrollHeight);
  await devScreen("results-bottom");
  const summary = {
    error: card.error?.kind,
    recordings: card.scan?.recordings.map((r) => ({ offset: r.offset, fileName: r.fileName, badge: r.badge, seconds: r.durationSeconds, frames: r.videoFrames, pieces: r.pieces, references: r.references })),
    clips: card.scan?.clips.length,
    fragments: card.scan?.fragments.length,
  };
  if (cfg.repair === false || !card.scan) {
    await api.devReport(JSON.stringify({ screen: "results", ...summary }));
    return;
  }
  window.scrollTo(0, 0);
  const repairing = cardRepairAll();
  await until(() => state.view === "main" || !!card.error);
  await until(() => !state.repairing || state.items.some((i) => i.run.status === "running" && (i.run.done ?? 0) > 0), 60_000);
  if (state.repairing) await devScreen("repairing");
  await repairing;
  await devScreen("done");
  window.scrollTo(0, document.body.scrollHeight);
  await devScreen("done-bottom");
  await api.devReport(
    JSON.stringify({
      screen: "done",
      ...summary,
      cardError: card.error?.kind,
      note: state.cardNote,
      items: state.items.map((i) => ({ dat: i.dat, ref: i.ref.match?.source, refPath: i.ref.match?.path, run: i.run.status, error: i.run.error?.kind, outcome: i.run.outcome })),
    }),
  );
}

void start();
